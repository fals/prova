//! Large tests: every claimed language, real toolchains, real projects
//! (fixtures/). Each fixture is run with its own test command and must
//! show: the real bug found per test (not opaque), and a hang caught as a
//! stall with no configuration.
//!
//! Go and Rust use the host toolchain. Python, TypeScript, Java and C# run
//! inside official images with prova copied in (`COPY --from`, ADR 0010).
//! Each fixture also runs its native benchmark as prova experiments: the
//! prepared `faster` change must be accepted, `same` and `slower` rejected.
//!
//! Opt-in: `prova run large` (or `cargo test -p prova-cli --test fixtures_large -- --ignored`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_fixture(name: &str) -> PathBuf {
    match name.strip_suffix("-native") {
        Some(base) => copy_fixture_as(base, name),
        None => copy_fixture_as(name, name),
    }
}

fn copy_fixture_as(name: &str, copy: &str) -> PathBuf {
    let src = repo().join("fixtures").join(name);
    let d = std::env::temp_dir().join(format!("prova-large-{copy}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    assert!(
        Command::new("cp")
            .arg("-r")
            .arg(&src)
            .arg(&d)
            .status()
            .unwrap()
            .success()
    );
    d
}

fn json_last_line(o: &Output) -> serde_json::Value {
    let out = String::from_utf8_lossy(&o.stdout);
    let line = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| {
            panic!(
                "no JSON in output:\n{out}\nstderr:\n{}",
                String::from_utf8_lossy(&o.stderr)
            )
        });
    serde_json::from_str(line).unwrap()
}

fn assert_bug_found(s: &serde_json::Value, bug: &str) {
    assert_eq!(s["outcome"], "failed", "{s}");
    assert_ne!(s["level"], "opaque", "per-test results expected: {s}");
    let failing: Vec<&str> = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["test"].as_str().unwrap())
        .collect();
    assert!(
        failing.iter().any(|t| t.contains(bug)),
        "{bug} not among {failing:?}"
    );
    let f = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["test"].as_str().unwrap().contains(bug))
        .unwrap();
    assert!(
        f["message"].is_string() || !s["output_tail"].as_array().unwrap().is_empty(),
        "failure is explained: {s}"
    );
}

// ------------------------------------------------------------- host

fn host(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(PROVA);
    c.args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir));
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn check_host(name: &str, cmd: &[&str], bug: &str) {
    let d = copy_fixture(name);
    let mut args = vec!["run", "--json", "--"];
    args.extend_from_slice(cmd);
    assert_bug_found(&json_last_line(&host(&d, &args, &[])), bug);

    let mut args = vec!["run", "--detach", "--json", "--stall-after", "5s", "--"];
    args.extend_from_slice(cmd);
    host(&d, &args, &[("HANG", "1")]);
    let w = host(
        &d,
        &[
            "wait",
            "last",
            "--until",
            "stall",
            "--max-wait",
            "240s",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        json_last_line(&w)["reason"],
        "stall",
        "{}",
        String::from_utf8_lossy(&w.stdout)
    );
    host(&d, &["cancel", "last"], &[]);
    host(
        &d,
        &["wait", "last", "--until", "done", "--max-wait", "60s"],
        &[],
    );
}

#[test]
#[ignore = "large: needs go"]
fn go() {
    check_host("go-make", &["make", "test"], "TestDiv");
}

#[test]
#[ignore = "large: needs cargo, compiles a crate"]
fn rust() {
    check_host("rust-make", &["make", "test"], "divides");
}

const VERDICTS: &str = "faster=accept same=reject slower=reject";

/// Runs fixtures/exp-check.sh in a copy of the fixture with the built prova.
fn exp_check_host(fixture: &str, args: &[&str]) {
    let d = copy_fixture_as(fixture, &format!("{fixture}-exp"));
    std::fs::copy(repo().join("fixtures/exp-check.sh"), d.join("exp-check.sh")).unwrap();
    let bin = Path::new(PROVA).parent().unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = Command::new("sh")
        .arg("exp-check.sh")
        .args(args)
        .current_dir(&d)
        .env("PATH", path)
        .env("PROVA_HOME", home_of(&d))
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        out.trim(),
        VERDICTS,
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
#[ignore = "large: needs go"]
fn go_experiments() {
    exp_check_host(
        "go-make",
        &[
            "BenchmarkCountPrimes.ns_per_op",
            "primes.go",
            "--",
            "go",
            "test",
            "-run",
            "^$",
            "-bench",
            "CountPrimes",
            "-count",
            "3",
            ".",
        ],
    );
}

#[test]
#[ignore = "large: needs cargo, compiles criterion"]
fn rust_experiments() {
    exp_check_host(
        "rust-make",
        &[
            "count_primes.time_ns",
            "src/primes.rs",
            "--",
            "cargo",
            "bench",
            "-q",
            "--bench",
            "primes",
            "--",
            "--warm-up-time",
            "1",
            "--measurement-time",
            "2",
            "--noplot",
        ],
    );
}

const METRIC_MAIN: &str = r#"fn main() {
    //PRE//
    println!("{{\"metric\":\"steps\",\"value\":{}}}", //VAL//);
}
"#;

fn write_main(d: &Path, pre: &str, val: &str) {
    let src = METRIC_MAIN.replace("//PRE//", pre).replace("//VAL//", val);
    std::fs::write(d.join("src/main.rs"), src).unwrap();
}

fn git(d: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A throwaway cargo project (real cargo, no dependencies) whose binary
/// prints one metric line, committed as the baseline of experiment `e`
/// (metric `steps`, lower is better, baseline 100).
fn cargo_experiment(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-large-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("Cargo.toml"),
        "[package]\nname = \"w\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(d.join(".gitignore"), "target/\n.prova/\n").unwrap();
    write_main(&d, "", "100");
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "base"]);
    let o = host(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "steps",
            "--better",
            "lower",
            "--min-effect",
            "10%",
            "--trials",
            "5",
            "--max-trials",
            "5",
            "--warmup",
            "0",
            "--",
            "cargo",
            "run",
            "-q",
            "--offline",
        ],
        &[],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    d
}

/// ADR 0035 section 4 (round 18 B9): a candidate that adds
/// `.cargo/config.toml` with a `runner` that prints the metric forged the
/// result. Runner configuration is a harness file: out of scope, unmeasured.
#[test]
#[ignore = "large: needs cargo"]
fn cargo_runner_config_is_out_of_scope() {
    let d = cargo_experiment("exp-runner");
    std::fs::create_dir_all(d.join(".cargo")).unwrap();
    std::fs::write(
        d.join(".cargo/config.toml"),
        r#"[target.'cfg(unix)']
runner = ["sh", "-c", "echo '{\"metric\":\"steps\",\"value\":1}'", "x"]
"#,
    )
    .unwrap();
    assert!(host(&d, &["exp", "candidate", "e"], &[]).status.success());
    let r = json_last_line(&host(&d, &["exp", "run", "e", "--json"], &[]));
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    assert_eq!(
        r["comparison"]["candidate_trials"], 0,
        "never measured: {r}"
    );
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("out of scope") && reason.contains(".cargo/config.toml"),
        "{reason}"
    );
}

/// ADR 0035 section 4 (round 18 B8): a candidate fast only while an
/// environment variable is set is accepted with it, and the revalidation
/// without it withdraws the accept.
#[test]
#[ignore = "large: needs cargo"]
fn cargo_env_dependent_speedup_is_withdrawn_on_revalidation() {
    let d = cargo_experiment("exp-env");
    write_main(
        &d,
        "let v = if std::env::var(\"KNOB\").is_ok() { 50 } else { 100 };",
        "v",
    );
    assert!(host(&d, &["exp", "candidate", "e"], &[]).status.success());
    let r = json_last_line(&host(&d, &["exp", "run", "e", "--json"], &[("KNOB", "1")]));
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
    assert_eq!(r["best"], "c1", "{r}");
    let v = json_last_line(&host(&d, &["exp", "revalidate", "e", "--json"], &[]));
    assert_eq!(v["best"], "baseline", "the accept must be withdrawn: {v}");
    assert!(v.to_string().contains("withdrawn"), "{v}");
}

/// ADR 0035 section 4 (round 18 M1): a candidate that caches results in the
/// temp directory between iterations gets the private-/tmp caveat.
#[test]
#[ignore = "large: needs cargo and bubblewrap"]
fn cargo_results_cached_in_tmp_are_reported() {
    if !prova_core::experiment::sandbox_available() {
        eprintln!("skipped: no bubblewrap here, /tmp is not private");
        return;
    }
    let d = cargo_experiment("exp-tmp");
    write_main(
        &d,
        "std::fs::write(std::env::temp_dir().join(\"results-cache\"), \"x\").unwrap();",
        "100",
    );
    assert!(host(&d, &["exp", "candidate", "e"], &[]).status.success());
    let r = json_last_line(&host(&d, &["exp", "run", "e", "--json"], &[]));
    let caveats = r["comparison"]["result"]["caveats"].to_string();
    assert!(
        caveats.contains("private /tmp") && caveats.contains("results-cache"),
        "{r}"
    );
}

// ------------------------------------------------------------- docker

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().expect("docker")
}

/// Builds prova:dev from this repository and the language image on top.
fn image(lang: &str) -> String {
    let o = docker(&["build", "-q", "-t", "prova:dev", repo().to_str().unwrap()]);
    assert!(
        o.status.success(),
        "prova:dev: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let tag = format!("prova-fixture-{lang}");
    let file = repo().join(format!("fixtures/docker/{lang}.Dockerfile"));
    let ctx = repo().join("fixtures/docker");
    let o = docker(&[
        "build",
        "-q",
        "-t",
        &tag,
        "-f",
        file.to_str().unwrap(),
        ctx.to_str().unwrap(),
    ]);
    assert!(
        o.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    // Shared, writable package caches across runs.
    docker(&[
        "run",
        "--rm",
        "-v",
        "prova-fixture-cache:/cache",
        &tag,
        "chmod",
        "777",
        "/cache",
    ]);
    tag
}

/// Runs `script` in the language image as the current user, with the
/// fixture at /w. `prepare` warms dependencies so downloads never look
/// like a stall.
fn in_container(tag: &str, dir: &Path, prepare: &str, script: &str) -> Output {
    let uid = String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout).unwrap();
    let gid = String::from_utf8(Command::new("id").arg("-g").output().unwrap().stdout).unwrap();
    let user = format!("{}:{}", uid.trim(), gid.trim());
    let mount = format!("{}:/w", dir.display());
    let full = format!(
        "{prepare} >/tmp/prepare.log 2>&1 || {{ cat /tmp/prepare.log; exit 9; }}; {script}"
    );
    docker(&[
        "run",
        "--rm",
        "--user",
        &user,
        "-v",
        &mount,
        "-w",
        "/w",
        "-v",
        "prova-fixture-cache:/cache",
        "-e",
        "HOME=/tmp/home",
        "-e",
        "npm_config_cache=/cache/npm",
        "-e",
        "MAVEN_OPTS=-Dmaven.repo.local=/cache/m2",
        "-e",
        "NUGET_PACKAGES=/cache/nuget",
        "-e",
        "DOTNET_CLI_HOME=/tmp/home",
        "-e",
        "DOTNET_NOLOGO=1",
        "-e",
        "DOTNET_CLI_TELEMETRY_OPTOUT=1",
        tag,
        "sh",
        "-c",
        &full,
    ])
}

/// Same check inside the language image.
fn exp_check_docker(lang: &str, fixture: &str, prepare: &str, args: &str) {
    exp_check_docker_expect(lang, fixture, prepare, args, VERDICTS);
}

fn exp_check_docker_expect(lang: &str, fixture: &str, prepare: &str, args: &str, expected: &str) {
    let tag = image(lang);
    // Per image: tests run in parallel, and two experiments may use one
    // fixture (go-make on the host and in the Lean image).
    let d = copy_fixture_as(fixture, &format!("{fixture}-exp-{lang}"));
    std::fs::copy(repo().join("fixtures/exp-check.sh"), d.join("exp-check.sh")).unwrap();
    let o = in_container(&tag, &d, prepare, &format!("sh exp-check.sh {args}"));
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        out.lines().last().unwrap_or("").trim(),
        expected,
        "stdout: {out}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn check_docker(lang: &str, fixture: &str, prepare: &str, cmd: &str, bug: &str) {
    let tag = image(lang);
    let d = copy_fixture(fixture);
    let o = in_container(&tag, &d, prepare, &format!("prova run --json -- {cmd}"));
    assert_bug_found(&json_last_line(&o), bug);

    let stall = format!(
        "HANG=1 prova run --detach --stall-after 5s -- {cmd} >/dev/null; \
         prova wait last --until stall --max-wait 240s --json; \
         prova cancel last >/dev/null; prova wait last --until done --max-wait 60s >/dev/null"
    );
    let w = in_container(&tag, &d, prepare, &stall);
    assert_eq!(
        json_last_line(&w)["reason"],
        "stall",
        "{}",
        String::from_utf8_lossy(&w.stdout)
    );
}

#[test]
#[ignore = "large: needs docker"]
fn python() {
    check_docker("python", "python-make", "true", "make test", "test_div");
}

#[test]
#[ignore = "large: needs docker and npm registry access"]
fn typescript() {
    check_docker(
        "node",
        "ts-vitest",
        "npm install --no-audit --no-fund",
        "npm test",
        "divides",
    );
}

/// Level 3 through an opt-in recipe, with the project's own command:
/// every test arrives as a structured event, with its duration.
fn check_native(
    lang: &str,
    fixture: &str,
    prepare: &str,
    recipe: &str,
    cmd: &str,
    bug: &str,
    said: &str,
) {
    let tag = image(lang);
    let d = copy_fixture(&format!("{fixture}-native"));
    // One container: the prova home lives in it.
    let o = in_container(
        &tag,
        &d,
        prepare,
        &format!("prova run --json --inject {recipe} -- {cmd}; prova history slow --json"),
    );
    let out = String::from_utf8_lossy(&o.stdout);
    let docs: Vec<serde_json::Value> = out
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        docs.len() >= 2,
        "run and history JSON expected: {out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let s = &docs[docs.len() - 2];
    assert_bug_found(s, bug);
    assert_eq!(s["level"], "native", "{s}");
    let f = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["test"].as_str().unwrap().contains(bug))
        .unwrap();
    assert!(f["message"].as_str().unwrap_or("").contains(said), "{s}");
    // The 1.5 s test carries its own duration, from the stream.
    let hist = &docs[docs.len() - 1];
    let p50 = hist["p50_ms"].as_u64().unwrap_or(0);
    assert!((1400..5000).contains(&p50), "slow test timed: {hist}");
}

#[test]
#[ignore = "large: needs docker"]
fn python_native_stream() {
    check_native(
        "python",
        "python-make",
        "true",
        "pytest-reportlog",
        "make test",
        "test_div",
        "assert 16 == 4",
    );
}

#[test]
#[ignore = "large: needs docker and npm registry access"]
fn typescript_native_stream() {
    check_native(
        "node",
        "ts-vitest",
        "npm install --no-audit --no-fund",
        "vitest-stream",
        "sh -c 'npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\"'",
        "divides",
        "expected 16 to be 4",
    );
}

#[test]
#[ignore = "large: needs docker and Maven Central access"]
fn java() {
    check_docker(
        "java",
        "java-maven",
        "mvn -q -B test-compile",
        "mvn -q -B test",
        "divides",
    );
}

#[test]
#[ignore = "large: needs docker and NuGet access"]
fn csharp() {
    check_docker(
        "dotnet",
        "csharp-dotnet",
        "dotnet build -nologo -v q",
        "dotnet test --no-build",
        "Divides",
    );
}

#[test]
#[ignore = "large: needs docker"]
fn python_experiments() {
    exp_check_docker(
        "python",
        "python-make",
        "true",
        "test_count_primes.median_ns primes.py -- python3 -m pytest benchmarks -q -p no:cacheprovider --benchmark-only --benchmark-min-rounds=5 --benchmark-columns=min,max,mean,median,stddev,rounds",
    );
}

#[test]
#[ignore = "large: needs docker and npm registry access"]
fn typescript_experiments() {
    exp_check_docker(
        "node",
        "ts-vitest",
        "npm install --no-audit --no-fund",
        "countPrimes.mean_ns src/primes.ts --setup 'npm install --no-audit --no-fund' -- npx vitest bench --run",
    );
}

#[test]
#[ignore = "large: needs docker and Maven Central access"]
fn java_experiments() {
    exp_check_docker(
        "java",
        "java-maven",
        "mvn -q -B -Pbench package -DskipTests",
        "PrimesBench.countPrimes.ns_per_op src/main/java/calc/Primes.java -- sh bench.sh",
    );
}

#[test]
#[ignore = "large: needs docker and NuGet access"]
fn csharp_experiments() {
    exp_check_docker(
        "dotnet",
        "csharp-dotnet",
        "dotnet build -nologo -v q bench",
        "CountPrimes.mean_ns Primes.cs -- dotnet run -c Release --project bench -- --filter '*'",
    );
}

/// Spec-gated experiment: the Go fixture's Lean specification (theorems
/// checked by `lake build`, answers compared with the implementation for
/// every n < 3000) gates the experiment. The sieve is accepted; a faster
/// sieve that counts n itself is rejected by the oracle, never measured.
#[test]
#[ignore = "large: needs docker (Go + Lean image)"]
fn go_lean_spec_experiment() {
    exp_check_docker_expect(
        "lean",
        "go-make",
        "true",
        "BenchmarkCountPrimes.ns_per_op primes.go --spec spec/Primes.lean --oracle 'sh spec/oracle.sh' \
         --variants 'faster wrong' -- go test -run '^$' -bench CountPrimes -count 3 .",
        "faster=accept wrong=reject(spec)",
    );
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
