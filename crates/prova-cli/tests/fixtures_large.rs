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

// ------------------------------------------------- containers (ADR 0035 §3)

/// A copy of python-make with a `./dev` wrapper that runs its argument in
/// the python image, as projects do (`exec docker run ... sh -c "$*"`).
/// Dropping it kills every container of this image that mounts the copy,
/// and nothing else.
struct DevWorld {
    dir: PathBuf,
    tag: String,
}

impl DevWorld {
    fn new(name: &str) -> DevWorld {
        let tag = image("python");
        let d = copy_fixture_as("python-make", &format!("python-make-{name}"))
            .canonicalize()
            .unwrap();
        let id = |flag: &str| {
            let o = Command::new("id").arg(flag).output().unwrap();
            String::from_utf8(o.stdout).unwrap().trim().to_owned()
        };
        let script = format!(
            "#!/bin/sh\nexec docker run --rm --user {}:{} -e HOME=/tmp -e HANG -v \"$PWD\":/w -w /w {tag} sh -c \"$*\"\n",
            id("-u"),
            id("-g")
        );
        std::fs::write(d.join("dev"), script).unwrap();
        assert!(
            Command::new("chmod")
                .args(["755", d.join("dev").to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        let w = DevWorld { dir: d, tag };
        assert!(
            w.containers().is_empty(),
            "stale containers: {:?}",
            w.containers()
        );
        w
    }

    /// Running containers of the image that mount this copy.
    fn containers(&self) -> Vec<String> {
        let o = docker(&[
            "ps",
            "-q",
            "--filter",
            &format!("ancestor={}", self.tag),
            "--filter",
            &format!("volume={}", self.dir.display()),
        ]);
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    fn prova(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        host(&self.dir, args, env)
    }

    /// Starts `./dev "python -m pytest -v"` detached with the test hanging.
    fn start_hanging(&self) -> String {
        let o = self.prova(
            &[
                "run",
                "--detach",
                "--json",
                "--stall-after",
                "5s",
                "--",
                "./dev",
                "python -m pytest -v",
            ],
            &[("HANG", "1")],
        );
        json_last_line(&o)["run"].as_str().unwrap().to_owned()
    }

    fn wait_stall(&self, run: &str) {
        let w = self.prova(
            &[
                "wait",
                run,
                "--until",
                "stall",
                "--max-wait",
                "120s",
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
    }

    fn cancel_and_wait(&self, run: &str) -> serde_json::Value {
        assert!(self.prova(&["cancel", run], &[]).status.success());
        json_last_line(&self.prova(
            &[
                "wait",
                run,
                "--until",
                "done",
                "--max-wait",
                "60s",
                "--json",
            ],
            &[],
        ))
    }
}

impl Drop for DevWorld {
    fn drop(&mut self) {
        for c in self.containers() {
            docker(&["kill", &c]);
        }
    }
}

/// Round 18 (blocker): `prova cancel` missed the run's container behind a
/// wrapper that quotes its command (`sh -c "$*"`), so a hung suite kept a
/// container running after the cancel. Large: only a real docker shows the
/// words docker keeps for the container's command.
#[test]
#[ignore = "large: needs docker (the cancel must find a container started by a wrapper script)"]
fn cancel_stops_the_container_a_quoting_wrapper_started() {
    let w = DevWorld::new("wrapper-cancel");
    let run = w.start_hanging();
    w.wait_stall(&run);
    assert_eq!(w.containers().len(), 1, "the run's container is up");
    let s = w.cancel_and_wait(&run);
    assert_eq!(s["status"], "cancelled", "{s}");
    assert!(
        w.containers().is_empty(),
        "container left running after cancel: {:?}\n{s}",
        w.containers()
    );
}

/// Round 18 (blocker): with two runs of one directory live, a cancel of one
/// killed the other's container. A cancel kills the container created for
/// the run (the other run already has its own, created before it) and
/// nothing else: the other run keeps running until it is cancelled too. The
/// case where a container could be either run's (contested, kill nothing) is
/// a race with docker's own latency and is tested in medium, with a fake client.
#[test]
#[ignore = "large: needs docker (two live runs of one directory, one container each)"]
fn cancel_does_not_kill_the_container_of_another_live_run() {
    let w = DevWorld::new("wrapper-two-runs");
    let a = w.start_hanging();
    w.wait_stall(&a);
    let a_container = w.containers();
    assert_eq!(a_container.len(), 1, "A's container");
    let b = w.start_hanging();
    w.wait_stall(&b);
    assert_eq!(w.containers().len(), 2, "one container per run");
    let s = w.cancel_and_wait(&b);
    assert_eq!(s["status"], "cancelled", "{s}");
    assert_eq!(
        w.containers(),
        a_container,
        "cancelling B must leave A's container and only it: {s}"
    );
    let other = json_last_line(&w.prova(&["status", &a, "--json"], &[]));
    assert_eq!(other["status"], "running", "{other}");
    let s = w.cancel_and_wait(&a);
    assert_eq!(s["status"], "cancelled", "{s}");
    assert!(w.containers().is_empty(), "{:?}\n{s}", w.containers());
}

/// Round 18: the first `--until first-failure` wait through a container
/// client announced 30 s and waited 65. A wrapper hides the client from the
/// command line, so the wait says when it learns of the container and how
/// long it will really take. The suite is quiet: no failure, no slow-test event.
#[test]
#[ignore = "large: needs docker; takes about 65 s (a container stall is called after 60 s)"]
fn the_first_wait_through_a_wrapper_says_how_long_it_really_is() {
    let w = DevWorld::new("wrapper-announce");
    let o = w.prova(
        &[
            "run",
            "--until",
            "first-failure",
            "--",
            "./dev",
            "sleep 600",
        ],
        &[("HANG", "1")],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("up to 1m05s in all"),
        "the wait never said it would last 1m05s in all:\n{err}\n{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let _ = w.prova(&["cancel", "last"], &[]);
    w.prova(
        &["wait", "last", "--until", "done", "--max-wait", "60s"],
        &[],
    );
}
