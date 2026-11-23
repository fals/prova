//! Code identity, recorded environment and machine fingerprint (ADR 0006).

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

use crate::ids::sha256_hex;

/// Environment variables recorded by default. Values of anything else are
/// never stored, so secrets in the environment stay out of the database.
pub const DEFAULT_ENV: &[&str] = &[
    "CI",
    "LANG",
    "TZ",
    "RUST_LOG",
    "RUSTFLAGS",
    "CARGO_PROFILE",
    "GOFLAGS",
    "GOARCH",
    "GOOS",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "NODE_ENV",
    "JAVA_HOME",
];

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Caches that running tests writes into the tree (bytecode and tool
/// caches). They are not code: a project that forgot to ignore them must
/// not get a new code identity after every test run.
const CACHES: &[&str] = &[
    ":(exclude,glob)**/__pycache__/**",
    ":(exclude,glob)**/*.pyc",
    ":(exclude,glob)**/.pytest_cache/**",
    ":(exclude,glob)**/.mypy_cache/**",
    ":(exclude,glob)**/.ruff_cache/**",
];

/// `tree:<12>` for a clean checkout, `tree:<12>+dirty:<12>` with uncommitted
/// changes, `nogit` outside git. Two runs are "same code" only when equal.
pub fn code_identity(dir: &Path) -> String {
    let Some(tree) = git(dir, &["rev-parse", "HEAD^{tree}"]) else {
        return "nogit".into();
    };
    let tree = String::from_utf8_lossy(&tree).trim().to_owned();
    let short = &tree[..tree.len().min(12)];
    let with_caches_excluded = |args: &[&'static str]| -> Vec<&'static str> {
        let mut v = args.to_vec();
        v.push("--");
        v.push(":/");
        v.extend_from_slice(CACHES);
        v
    };
    let status = git(
        dir,
        &with_caches_excluded(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
    )
    .unwrap_or_default();
    if status.is_empty() {
        return format!("tree:{short}");
    }
    let mut material = git(dir, &["diff", "HEAD", "--binary"]).unwrap_or_default();
    // Untracked files: hash their content so edits change the identity.
    let untracked = git(
        dir,
        &with_caches_excluded(&[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--full-name",
        ]),
    )
    .unwrap_or_default();
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .map(|r| String::from_utf8_lossy(&r).trim().to_owned())
        .unwrap_or_default();
    for name in untracked.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        material.extend_from_slice(name);
        let path = Path::new(&root).join(String::from_utf8_lossy(name).as_ref());
        // Regular files only (no FIFOs or devices via symlinks), size-capped.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() && m.len() <= 16 * 1024 * 1024 => {
                if let Ok(bytes) = std::fs::read(&path) {
                    material.extend_from_slice(sha256_hex(&bytes).as_bytes());
                }
            }
            Ok(m) => material.extend_from_slice(
                format!("{}:{}", m.len(), m.file_type().is_symlink()).as_bytes(),
            ),
            Err(_) => {}
        }
    }
    format!("tree:{short}+dirty:{}", &sha256_hex(&material)[..12])
}

pub fn recorded_env(extra: &[String]) -> Value {
    let mut m = Map::new();
    for name in DEFAULT_ENV
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
    {
        if let Ok(v) = std::env::var(name) {
            m.insert(name.to_owned(), Value::String(v));
        }
    }
    Value::Object(m)
}

/// Cheap machine fingerprint; experiments refuse to compare across differing ones.
pub fn machine_fingerprint() -> Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_owned())
        })
        .unwrap_or_default();
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let governor = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": cpu,
        "cores": cores,
        "kernel": kernel,
        "governor": governor,
    })
}

/// Fingerprint of a failure from the tail of its output: volatile parts
/// (numbers, hex addresses, temp paths) are normalised before hashing.
pub fn output_fingerprint(tail: &[String]) -> String {
    let normalised: Vec<String> = tail.iter().map(|l| normalise_line(l)).collect();
    sha256_hex(normalised.join("\n").as_bytes())[..12].to_owned()
}

pub fn normalise_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push_str("0x#");
        } else if c.is_ascii_digit() {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || *c == '.')
            {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
        }
    }
    for prefix in ["/tmp/", "/var/folders/"] {
        while let Some(i) = out.find(prefix) {
            let end = out[i..]
                .find(char::is_whitespace)
                .map_or(out.len(), |e| i + e);
            out.replace_range(i..end, "<tmp>");
        }
    }
    out.trim().to_owned()
}

/// Where a run happens (ADR 0014). In Kubernetes, POD_NAME and NODE_NAME
/// come from the downward API; HOSTNAME is the pod name by default.
pub fn run_origin() -> Value {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .or_else(|| var("HOSTNAME"))
        .unwrap_or_default();
    let in_k8s = var("KUBERNETES_SERVICE_HOST").is_some();
    let actor = var("PROVA_ACTOR")
        .or_else(|| var("GITHUB_RUN_ID").map(|id| format!("github-actions:{id}")))
        .or_else(|| var("CI_JOB_ID").map(|id| format!("ci:{id}")))
        .or_else(|| var("USER"))
        .unwrap_or_default();
    let mut m = Map::new();
    m.insert("host".into(), Value::String(host.clone()));
    if let Some(p) = var("POD_NAME").or_else(|| in_k8s.then_some(host)) {
        m.insert("pod".into(), Value::String(p));
    }
    if let Some(n) = var("NODE_NAME") {
        m.insert("node".into(), Value::String(n));
    }
    m.insert("actor".into(), Value::String(actor));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_volatile_parts() {
        let a = vec!["thread 'main' panicked at src/x.rs:12:5: got 17 at 0xdeadbeef".to_string()];
        let b = vec!["thread 'main' panicked at src/x.rs:14:9: got 3 at 0x1234".to_string()];
        let c = vec!["thread 'main' panicked at src/y.rs:12:5: got 17".to_string()];
        assert_eq!(output_fingerprint(&a), output_fingerprint(&b));
        assert_ne!(output_fingerprint(&a), output_fingerprint(&c));
        assert_eq!(
            normalise_line("open /tmp/abc123/f.txt failed"),
            "open <tmp> failed"
        );
    }

    #[test]
    fn env_records_only_allow_listed_names() {
        let env = recorded_env(&["PROVA_TEST_SURELY_UNSET_VAR".into()]);
        assert!(
            env.as_object()
                .unwrap()
                .keys()
                .all(|k| DEFAULT_ENV.contains(&k.as_str()))
        );
    }
}
