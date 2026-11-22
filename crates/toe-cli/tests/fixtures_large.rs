//! Large tests: every claimed language, real toolchains, real projects
//! (fixtures/). Each fixture is run with its own test command and must
//! show: the real bug found per test (not opaque), and a hang caught as a
//! stall with no configuration.
//!
//! Go and Rust use the host toolchain. Python, TypeScript, Java and C# run
//! inside official images with toe copied in (`COPY --from`, ADR 0010).
//! Each fixture also runs its native benchmark as toe experiments: the
//! prepared `faster` change must be accepted, `same` and `slower` rejected.
//!
//! Opt-in: `toe run large` (or `cargo test -p toe-cli --test fixtures_large -- --ignored`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_fixture(name: &str) -> PathBuf {
    copy_fixture_as(name, name)
}

fn copy_fixture_as(name: &str, copy: &str) -> PathBuf {
    let src = repo().join("fixtures").join(name);
    let d = std::env::temp_dir().join(format!("toe-large-{copy}-{}", std::process::id()));
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
    let mut c = Command::new(TOE);
    c.args(args).current_dir(dir).env("TOE_HOME", home_of(dir));
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

/// Runs fixtures/exp-check.sh in a copy of the fixture with the built toe.
fn exp_check_host(fixture: &str, args: &[&str]) {
    let d = copy_fixture_as(fixture, &format!("{fixture}-exp"));
    std::fs::copy(repo().join("fixtures/exp-check.sh"), d.join("exp-check.sh")).unwrap();
    let bin = Path::new(TOE).parent().unwrap();
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
        .env("TOE_HOME", home_of(&d))
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

// ------------------------------------------------------------- docker

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().expect("docker")
}

/// Builds toe:dev from this repository and the language image on top.
fn image(lang: &str) -> String {
    let o = docker(&["build", "-q", "-t", "toe:dev", repo().to_str().unwrap()]);
    assert!(
        o.status.success(),
        "toe:dev: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let tag = format!("toe-fixture-{lang}");
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
        "toe-fixture-cache:/cache",
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
        "toe-fixture-cache:/cache",
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
    let tag = image(lang);
    let d = copy_fixture_as(fixture, &format!("{fixture}-exp"));
    std::fs::copy(repo().join("fixtures/exp-check.sh"), d.join("exp-check.sh")).unwrap();
    let o = in_container(&tag, &d, prepare, &format!("sh exp-check.sh {args}"));
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        out.lines().last().unwrap_or("").trim(),
        VERDICTS,
        "stdout: {out}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn check_docker(lang: &str, fixture: &str, prepare: &str, cmd: &str, bug: &str) {
    let tag = image(lang);
    let d = copy_fixture(fixture);
    let o = in_container(&tag, &d, prepare, &format!("toe run --json -- {cmd}"));
    assert_bug_found(&json_last_line(&o), bug);

    let stall = format!(
        "HANG=1 toe run --detach --stall-after 5s -- {cmd} >/dev/null; \
         toe wait last --until stall --max-wait 240s --json; \
         toe cancel last >/dev/null; toe wait last --until done --max-wait 60s >/dev/null"
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

/// Isolated toe home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
