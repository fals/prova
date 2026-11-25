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
