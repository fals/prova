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

/// Round 18 (python MAJOR 4 and 5): a pytest node id, and a parametrized case
/// whose id has a space and a `$`, selected through the adopted `./dev`
/// wrapper run exactly that one test. Large: a real pytest in the language
/// image behind a real container wrapper; a fake `./dev` cannot show that the
/// id reaches pytest as one word (small and medium tests cover the template).
#[test]
#[ignore = "large: needs docker"]
fn python_select_node_id_through_a_wrapper() {
    let tag = image("python");
    let d = copy_fixture_as("python-make", "python-make-select");
    std::fs::write(
        d.join("tests/test_params.py"),
        "import pytest\nfrom calc import add\n\n\n\
         @pytest.mark.parametrize(\"label,a,b,want\", [\n    (\"two plus $two\", 2, 2, 4),\n    (\"neg one\", -1, 1, 0),\n])\n\
         def test_add_cases(label, a, b, want):\n    assert add(a, b) == want\n\n\n\
         class TestCls:\n    def test_m(self):\n        assert add(1, 1) == 2\n",
    )
    .unwrap();
    let name = format!("prova-large-select-{}", std::process::id());
    let dev = d.join("dev");
    std::fs::write(
        &dev,
        format!(
            "#!/bin/sh\nexec docker run --rm --name {name} --user \"$(id -u):$(id -g)\" -e HOME=/tmp \
             -e PYTEST_ADDOPTS -v \"$PWD\":/w -w /w {tag} sh -c \"$*\"\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dev, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let check = || {
        // An earlier ad-hoc run through the wrapper: adopt learns it.
        host(
            &d,
            &["run", "--json", "--", "./dev", "python3 -m pytest -q"],
            &[],
        );
        let o = host(&d, &["adopt", "--write"], &[]);
        assert!(
            o.status.success(),
            "adopt: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        for (select, want) in [
            // A method of a class, by prova's own name for it (review: the
            // class must not become a path).
            (
                "tests.test_params.TestCls::test_m",
                "tests/test_params.py::TestCls::test_m",
            ),
            (
                "tests/test_calc.py::test_add",
                "tests/test_calc.py::test_add",
            ),
            (
                "tests/test_params.py::test_add_cases[two plus $two-2-2-4]",
                "test_add_cases[two plus $two-2-2-4]",
            ),
        ] {
            let s = json_last_line(&host(
                &d,
                &["run", "default", "--select", select, "--json"],
                &[],
            ));
            assert_eq!(s["outcome"], "passed", "{select}: {s}");
            assert_eq!(
                (
                    s["counts"]["passed"].as_u64(),
                    s["counts"]["failed"].as_u64()
                ),
                (Some(1), Some(0)),
                "exactly one test ran for {select} ({want}): {s}"
            );
            assert_eq!(s["selection"], select, "{s}");
        }
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
    let _ = docker(&["rm", "-f", &name]);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Round 18 (rust BLOCKER B1): the suite `prova adopt` proposes for a Cargo
/// workspace runs the doctests of every member; one failing doctest fails
/// it. Large: a real cargo (and cargo-nextest) building a real two-crate
/// workspace.
#[test]
#[ignore = "large: needs cargo and cargo-nextest, compiles a workspace"]
fn rust_workspace_doctest_failure_fails_the_adopted_suite() {
    let d = std::env::temp_dir().join(format!("prova-large-rust-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (member, name, doc, tests) in [("a", "one", 1, 1), ("b", "two", 3, 2)] {
        std::fs::create_dir_all(d.join(member).join("src")).unwrap();
        std::fs::write(
            d.join(member).join("Cargo.toml"),
            format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        // Member b's doctest asserts 3 for a function that returns 2.
        std::fs::write(
            d.join(member).join("src/lib.rs"),
            format!(
                "/// ```\n/// assert_eq!({member}::{name}(), {doc});\n/// ```\npub fn {name}() -> i32 {{\n    {tests}\n}}\n\n\
                 #[cfg(test)]\nmod tests {{\n    #[test]\n    fn it_is_{name}() {{\n        assert_eq!(super::{name}(), {tests});\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        d.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    let check = || {
        let o = host(&d, &["adopt", "--write"], &[]);
        assert!(
            o.status.success(),
            "adopt: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        // Without this the failure below could come from anywhere.
        let toml = std::fs::read_to_string(d.join("prova.toml")).unwrap();
        assert!(
            toml.contains("--doc --workspace"),
            "the adopted run has a doctest step:\n{toml}"
        );
        let s = json_last_line(&host(&d, &["run", "default", "--json"], &[]));
        assert_eq!(
            s["outcome"], "failed",
            "doctest failure must fail the suite: {s}"
        );
        let failing: Vec<&str> = s["failures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["test"].as_str().unwrap())
            .collect();
        assert!(
            failing.iter().any(|t| t.contains("b/src/lib.rs")),
            "member b's doctest is named: {failing:?}"
        );
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(home_of(&d));
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
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

// ------------------------------------------------- places (ADR 0035 section 1)

/// A throwaway git repository holding `files`, outside the checkout.
fn repo_with(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-large-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (path, content) in files {
        let p = d.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    d
}

/// Removes a test's temporary directories when it ends, however it ends.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for d in &self.0 {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn said(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Round 18 (ts-vitest BLOCKER): `cd packages/b && prova run` named tests
/// relative to packages/b, merging them with the other package's tests of
/// the same name. Real Vitest in the node image: the names carry the path
/// from the repository root and the two packages' histories stay apart.
#[test]
#[ignore = "large: needs docker and npm registry access"]
fn typescript_packages_started_from_a_subdirectory_keep_repository_names() {
    let t = "import { expect, it } from \"vitest\";\n\nit(\"adds\", () => {\n  expect(process.env.BAD ? 0 : 1).toBe(1);\n});\n";
    let d = repo_with(
        "tspackages",
        &[
            (
                "package.json",
                "{\n  \"name\": \"mono\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"devDependencies\": { \"vitest\": \"3.2.4\" }\n}\n",
            ),
            ("packages/a/src/calc.test.ts", t),
            ("packages/b/src/calc.test.ts", t),
            (".gitignore", "node_modules\n"),
        ],
    );
    let _cleanup = Cleanup(vec![d.clone(), home_of(&d)]);
    let tag = image("node");
    let vitest = "../../node_modules/.bin/vitest run --reporter=verbose";
    let script = format!(
        "(cd packages/a && prova run -- {vitest}); \
         (cd packages/b && BAD=1 prova run -- {vitest}); \
         echo ---HISTORY---; prova history adds"
    );
    let o = in_container(&tag, &d, "npm install --no-audit --no-fund", &script);
    let out = said(&o);
    let (runs, history) = out.split_once("---HISTORY---").expect(&out);
    assert!(
        runs.contains("packages/b/src/calc.test.ts"),
        "b's tests are named from the repository root: {out}"
    );
    assert!(
        runs.contains("first time seen") && !runs.contains("FLAKY"),
        "b's failure took no other package's history: {out}"
    );
    assert!(
        history.contains("packages/a/src/calc.test.ts")
            && history.contains("packages/b/src/calc.test.ts"),
        "the two packages' same-named tests stay apart: {out}"
    );
}
