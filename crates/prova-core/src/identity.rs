//! Code identity, recorded environment and machine fingerprint (ADR 0006).

use std::collections::{BTreeMap, HashMap};
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
    // Bytecode is a cache only inside __pycache__ (above): a `json.pyc` next
    // to the sources imports as a module and is code (ADR 0035 section 4).
    ":(exclude,glob)**/.pytest_cache/**",
    ":(exclude,glob)**/.mypy_cache/**",
    ":(exclude,glob)**/.ruff_cache/**",
    // Build outputs written by the build itself (round 8: Maven Shade's
    // dependency-reduced-pom.xml changed the code identity).
    ":(exclude,glob)**/dependency-reduced-pom.xml",
    ":(exclude,glob)**/target/**",
    ":(exclude,glob)**/obj/**",
    ":(exclude,glob)**/bin/Debug/**",
    ":(exclude,glob)**/bin/Release/**",
    // Reports and caches Vitest, Jest, coverage tools and Playwright write
    // (round 17: every run after a failing run had a new identity).
    ":(exclude,glob)**/coverage/**",
    ":(exclude,glob)**/.nyc_output/**",
    ":(exclude,glob)**/.vitest/**",
    ":(exclude,glob)**/.vite/**",
    ":(exclude,glob)**/.cache/**",
    ":(exclude,glob)**/test-results/**",
    ":(exclude,glob)**/playwright-report/**",
    ":(exclude,glob)**/htmlcov/**",
    ":(exclude,glob)**/.coverage",
    ":(exclude,glob)**/.coverage.*",
    ":(exclude,glob)**/node_modules/**",
    // Core dumps from a crashing test (round 9).
    ":(exclude,glob)**/core",
    ":(exclude,glob)**/core.[0-9]*",
    // prova's own config and folder are not the code under test: adding
    // prova.toml must not make the same code look like a new version.
    ":(exclude,top)prova.toml",
    ":(exclude,top,glob).prova/**",
];

/// The project a directory belongs to: the nearest directory holding
/// `prova.toml` at or below the git work tree's root, else the git work
/// tree's root, else the nearest `prova.toml` above, else the directory
/// itself. Test names are read relative to it (ADR 0035 section 1), so a
/// project in `services/api` with its own `prova.toml` keeps `src/x.test.ts`
/// names, and a monorepo with one `prova.toml` at its root names every
/// test from there.
pub fn repo_root(dir: &Path) -> std::path::PathBuf {
    let top = git(dir, &["rev-parse", "--show-toplevel"])
        .map(|t| String::from_utf8_lossy(&t).trim().to_owned())
        .filter(|t| !t.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(top) = top {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let (top_c, dir_c) = (canon(&top), canon(dir));
        // Walk up from the directory to the work tree's root; the nearest
        // `prova.toml` on the way is the project's.
        return dir_c
            .ancestors()
            .take_while(|d| d.starts_with(&top_c))
            .find(|d| d.join("prova.toml").exists())
            .map(Path::to_path_buf)
            .unwrap_or(top);
    }
    dir.ancestors()
        .find(|d| d.join("prova.toml").exists())
        .unwrap_or(dir)
        .to_path_buf()
}

/// `tree:<12>`: the git tree id the working content has (ADR 0038 section 1).
/// Equal content is equal code, whether committed or not. `nogit` outside
/// git. A repository whose object format is not sha1 keeps the older
/// `tree:<12>+dirty:<12>` form when it has uncommitted changes.
pub fn code_identity(dir: &Path) -> String {
    // The repository's, whatever directory the run started in (ADR 0035
    // section 1).
    let top = repo_root(dir);
    let dir = top.as_path();
    let Some(tree) = git(dir, &["rev-parse", "HEAD^{tree}"]) else {
        return "nogit".into();
    };
    let tree = String::from_utf8_lossy(&tree).trim().to_owned();
    let short = &tree[..tree.len().min(12)];
    let status = git(
        dir,
        &with_caches_excluded(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
    )
    .unwrap_or_default();
    if status.is_empty() {
        return format!("tree:{short}");
    }
    let sha1 = git(dir, &["rev-parse", "--show-object-format"])
        .is_none_or(|f| String::from_utf8_lossy(&f).trim() == "sha1");
    if sha1 && let Some(id) = working_tree_id(dir, &status) {
        return format!("tree:{}", &id[..12]);
    }
    format!("tree:{short}+dirty:{}", legacy_dirty_hash(dir))
}

fn with_caches_excluded(args: &[&'static str]) -> Vec<&'static str> {
    let mut v = args.to_vec();
    v.push("--");
    v.push(":/");
    v.extend_from_slice(CACHES);
    v
}

/// One entry of a git tree: octal mode without leading zeros, hex object id.
#[derive(Clone)]
struct Entry {
    mode: String,
    id: String,
}

/// The id git would give a tree holding HEAD's content with the changes in
/// `status` applied from the work tree. Writes no object: blobs and trees
/// are only hashed (`git hash-object` without `-w`).
fn working_tree_id(top: &Path, status: &[u8]) -> Option<String> {
    // dir ("" for the root) -> name -> entry, from HEAD.
    let mut dirs: HashMap<String, BTreeMap<String, Entry>> = HashMap::new();
    dirs.insert(String::new(), BTreeMap::new());
    let listing = git(top, &["ls-tree", "-r", "-t", "-z", "HEAD"])?;
    for rec in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = std::str::from_utf8(rec).ok()?;
        let (meta, path) = rec.split_once('\t')?;
        let mut f = meta.split(' ');
        let mode = f.next()?.trim_start_matches('0').to_owned();
        let kind = f.next()?;
        let id = f.next()?.to_owned();
        if kind == "tree" {
            dirs.entry(path.to_owned()).or_default();
        }
        let (parent, name) = split_path(path);
        dirs.entry(parent.to_owned())
            .or_default()
            .insert(name.to_owned(), Entry { mode, id });
    }

    // Paths the status names; a rename lists the old path as a second field.
    let mut paths: Vec<&str> = Vec::new();
    let mut fields = status.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        let f = std::str::from_utf8(f).ok()?;
        let (xy, path) = (f.get(..2)?, f.get(3..)?);
        if xy.contains('R') || xy.contains('C') {
            paths.push(std::str::from_utf8(fields.next()?).ok()?);
        }
        // A trailing slash is a nested repository, left as it is.
        if !path.ends_with('/') {
            paths.push(path);
        }
    }
    // A gitlink (submodule) stays as in HEAD.
    paths.retain(|p| {
        let (parent, name) = split_path(p);
        dirs.get(parent)
            .and_then(|d| d.get(name))
            .is_none_or(|e| e.mode != "160000")
    });

    // Current state of each path in the work tree.
    let mut files: Vec<(&str, &str)> = Vec::new();
    let mut symlinks: Vec<(&str, String)> = Vec::new();
    for p in &paths {
        if p.contains('\n') {
            return None;
        }
        let Ok(m) = std::fs::symlink_metadata(top.join(p)) else {
            continue;
        };
        if m.file_type().is_symlink() {
            let target = std::fs::read_link(top.join(p)).ok()?;
            symlinks.push((p, target.to_string_lossy().into_owned()));
        } else if m.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let mode = if m.permissions().mode() & 0o100 != 0 {
                "100755"
            } else {
                "100644"
            };
            files.push((p, mode));
        }
    }
    let file_ids = hash_files(top, &files.iter().map(|f| f.0).collect::<Vec<_>>())?;

    // Removals first so that a file becoming a directory (or back) is
    // replaced, not deleted after being set.
    for p in &paths {
        let (parent, name) = split_path(p);
        if let Some(d) = dirs.get_mut(parent) {
            d.remove(name);
        }
    }
    let mut set = |p: &str, mode: &str, id: String| {
        let (parent, name) = split_path(p);
        let mut dir = parent;
        loop {
            dirs.entry(dir.to_owned()).or_default();
            if dir.is_empty() {
                break;
            }
            dir = split_path(dir).0;
        }
        dirs.entry(parent.to_owned()).or_default().insert(
            name.to_owned(),
            Entry {
                mode: mode.to_owned(),
                id,
            },
        );
    };
    for ((p, mode), id) in files.iter().zip(file_ids) {
        set(p, mode, id);
    }
    for (p, target) in symlinks {
        let args = ["hash-object", "-t", "blob", "--no-filters", "--stdin"];
        set(p, "120000", git_hash(top, &args, target.as_bytes())?);
    }

    // Rebuild the directories from a touched path up to the root, deepest
    // first; every other directory keeps HEAD's id.
    let mut dirty: Vec<&str> = Vec::new();
    for p in &paths {
        let mut d = split_path(p).0;
        loop {
            if !dirty.contains(&d) {
                dirty.push(d);
            }
            if d.is_empty() {
                break;
            }
            d = split_path(d).0;
        }
    }
    let depth = |d: &str| {
        if d.is_empty() {
            0
        } else {
            d.matches('/').count() + 1
        }
    };
    dirty.sort_by_key(|d| std::cmp::Reverse(depth(d)));
    let mut root = None;
    for d in dirty {
        let entries = dirs.get(d).cloned().unwrap_or_default();
        if d.is_empty() {
            root = Some(hash_tree(top, &entries)?);
            continue;
        }
        let (parent, name) = split_path(d);
        let parent_dir = dirs.entry(parent.to_owned()).or_default();
        if entries.is_empty() {
            // An emptied directory vanishes, unless a file now has its name.
            if parent_dir.get(name).is_some_and(|e| e.mode == "40000") {
                parent_dir.remove(name);
            }
        } else {
            let id = hash_tree(top, &entries)?;
            let entry = Entry {
                mode: "40000".into(),
                id,
            };
            // Re-borrow: hashing needed no mutable access.
            dirs.entry(parent.to_owned())
                .or_default()
                .insert(name.to_owned(), entry);
        }
    }
    root
}

fn split_path(p: &str) -> (&str, &str) {
    p.rsplit_once('/').unwrap_or(("", p))
}

/// Tree object id for `entries`: git sorts a directory as if its name had a
/// trailing slash.
fn hash_tree(top: &Path, entries: &BTreeMap<String, Entry>) -> Option<String> {
    let mut sorted: Vec<(&String, &Entry)> = entries.iter().collect();
    sorted.sort_by_key(|(name, e)| {
        let mut k = name.as_bytes().to_vec();
        if e.mode == "40000" {
            k.push(b'/');
        }
        k
    });
    let mut body = Vec::new();
    for (name, e) in sorted {
        body.extend_from_slice(e.mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&hex_to_bytes(&e.id)?);
    }
    git_hash(top, &["hash-object", "-t", "tree", "--stdin"], &body)
}

fn hex_to_bytes(h: &str) -> Option<Vec<u8>> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `git hash-object` (never `-w`) with `input` on stdin; the output, trimmed.
fn git_hash(top: &Path, args: &[&str], input: &[u8]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(top)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    // Written on its own thread: the output may fill its pipe meanwhile.
    let out = std::thread::scope(|s| {
        s.spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(input);
        });
        child.wait_with_output().ok()
    })?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Blob ids of work tree files in one process, with the filters `git add`
/// would apply.
fn hash_files(top: &Path, paths: &[&str]) -> Option<Vec<String>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let input = paths.join("\n") + "\n";
    let out = git_hash(top, &["hash-object", "--stdin-paths"], input.as_bytes())?;
    let ids: Vec<String> = out.lines().map(str::to_owned).collect();
    (ids.len() == paths.len()).then_some(ids)
}

/// The identity of a dirty tree whose tree id cannot be computed: a hash of
/// the diff and the untracked files' content.
fn legacy_dirty_hash(dir: &Path) -> String {
    let mut material =
        git(dir, &with_caches_excluded(&["diff", "HEAD", "--binary"])).unwrap_or_default();
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
    for name in untracked.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        material.extend_from_slice(name);
        let path = dir.join(String::from_utf8_lossy(name).as_ref());
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
    sha256_hex(&material)[..12].to_owned()
}

/// Variables that change between shells and sessions without changing what
/// a test does, and names that may hold secrets: left out of the
/// environment fingerprint.
fn env_ignored(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
        "TERM",
        "COLORTERM",
        "COLUMNS",
        "LINES",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "TMUX",
        "TMUX_PANE",
        "WINDOWID",
        "GPG_TTY",
        "LS_COLORS",
        "PS1",
        "PS2",
        "HISTFILE",
        "HISTSIZE",
        "MAIL",
        "STY",
        "INVOCATION_ID",
        "JOURNAL_STREAM",
        "MANAGERPID",
        "SYSTEMD_EXEC_PID",
        "XDG_VTNR",
        "OLDPWD",
        "SSH_TTY",
    ];
    const PREFIX: &[&str] = &[
        "SSH_",
        "XDG_SESSION",
        "KITTY_",
        "ALACRITTY_",
        "TERM_",
        "VSCODE_",
        "CLAUDE",
        "OTEL_",
        "PROVA_",
        "LC_TERMINAL",
        "WT_",
        "TERMINAL_",
        "GHOSTTY",
    ];
    const SECRET: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "KEY",
        "AUTH",
        "CREDENTIAL",
        "COOKIE",
    ];
    EXACT.contains(&name)
        || PREFIX.iter().any(|p| name.starts_with(p))
        || name.ends_with("_PID")
        || SECRET.iter().any(|w| name.to_ascii_uppercase().contains(w))
}

/// Every other variable as `name -> short hash of its value` (values are
/// never stored): two runs whose fingerprints differ ran in different
/// environments (round 11: `HANG=1` and `BADENV=1` made a test "flaky").
pub fn env_fingerprint() -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in std::env::vars() {
        if !env_ignored(&k) {
            let h = sha256_hex(v.as_bytes());
            m.insert(k, Value::String(h[..8].to_owned()));
        }
    }
    m
}

pub fn recorded_env(extra: &[String]) -> Value {
    let mut m = Map::new();
    m.insert("__fingerprint".to_owned(), Value::Object(env_fingerprint()));
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
    let host = Some(
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .trim()
            .to_owned(),
    )
    .filter(|h| !h.is_empty())
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
                // Values only for allow-listed names; the rest is hashed.
                .all(|k| k == "__fingerprint" || DEFAULT_ENV.contains(&k.as_str()))
        );
    }
}
