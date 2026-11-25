//! Medium: the single-runner-call check reads the run's directory
//! (package.json scripts, a Vitest config, go.mod and TestMain). A temp dir
//! is needed; a small test cannot touch disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use prova_core::config::CommandSpec;
use prova_core::runner_call::{check, go_package_has_testmain};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-rc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(c: &str, dir: &Path) -> Result<(), String> {
    check(&CommandSpec::Shell(c.into()), dir).map(|_| ())
}

#[test]
fn package_json_scripts_are_read() {
    let dir = tmp("pkg");
    let write = |s: &str| std::fs::write(dir.join("package.json"), s).unwrap();
    write(
        r#"{"scripts":{"test":"vitest run","lint":"eslint .","ci":"npm run lint && vitest run","t2":"jest"}}"#,
    );
    assert!(run("npm test", &dir).is_ok());
    assert!(run("npm test -- --reporter=verbose", &dir).is_ok());
    assert!(run("pnpm test", &dir).is_ok());
    assert!(run("yarn test", &dir).is_ok());
    assert!(run("npm run t2", &dir).is_ok());
    let e = run("npm run ci", &dir).unwrap_err();
    assert!(e.contains("&&"), "{e}");
    let e = run("npm run lint", &dir).unwrap_err();
    assert!(e.contains("eslint"), "{e}");
    let e = run("npm run nope", &dir).unwrap_err();
    assert!(e.contains("does not define"), "{e}");
    write(r#"{"scripts":{"test":"vitest run","pretest":"tsc"}}"#);
    let e = run("npm test", &dir).unwrap_err();
    assert!(e.contains("pretest"), "{e}");
    write(r#"{"scripts":{"test":"vitest run --coverage"}}"#);
    let e = run("npm test", &dir).unwrap_err();
    assert!(e.contains("coverage"), "{e}");
    std::fs::remove_file(dir.join("package.json")).unwrap();
    assert!(run("npm test", &dir).is_err());
}

#[test]
fn vitest_coverage_thresholds_in_a_config_are_refused() {
    let dir = tmp("vitest");
    assert!(run("vitest run", &dir).is_ok());
    std::fs::write(
        dir.join("vitest.config.ts"),
        "export default { test: { coverage: { thresholds: { lines: 90 } } } }",
    )
    .unwrap();
    let e = run("vitest run", &dir).unwrap_err();
    assert!(e.contains("coverage"), "{e}");
    std::fs::write(dir.join("vitest.config.ts"), "export default {}").unwrap();
    assert!(run("vitest run", &dir).is_ok());
}

/// Round 18: thresholds set in a module the config imports, or in the
/// config `--config`/`-c` names, were not seen.
#[test]
fn vitest_coverage_in_an_imported_or_named_config_is_refused() {
    let dir = tmp("vitest-import");
    std::fs::write(
        dir.join("vitest.config.ts"),
        "import { cov } from './cov';\nexport default { test: { coverage: cov } }",
    )
    .unwrap();
    std::fs::write(dir.join("cov.ts"), "export const cov = { enabled: false }").unwrap();
    assert!(run("vitest run", &dir).is_ok());
    std::fs::write(
        dir.join("cov.ts"),
        "export const cov = { thresholds: { lines: 90 } }",
    )
    .unwrap();
    let e = run("vitest run", &dir).unwrap_err();
    assert!(e.contains("coverage"), "{e}");

    let dir = tmp("vitest-named");
    std::fs::write(dir.join("vitest.config.ts"), "export default {}").unwrap();
    std::fs::write(
        dir.join("strict.config.mjs"),
        "export default { test: { coverage: { thresholds: { lines: 90 } } } }",
    )
    .unwrap();
    assert!(run("vitest run", &dir).is_ok());
    assert!(run("vitest run --config strict.config.mjs", &dir).is_err());
    assert!(run("vitest run -c strict.config.mjs", &dir).is_err());
    assert!(run("vitest run --config=strict.config.mjs", &dir).is_err());
    assert!(run("npx vitest run --config ./strict.config.mjs", &dir).is_err());
}

#[test]
fn go_testmain_is_found_through_the_module_path() {
    let p = tmp("gomod");
    std::fs::write(p.join("go.mod"), "module example.com/m\n\ngo 1.22\n").unwrap();
    std::fs::create_dir_all(p.join("leaky")).unwrap();
    std::fs::create_dir_all(p.join("plain")).unwrap();
    std::fs::write(
        p.join("leaky/main_test.go"),
        "package leaky\nfunc TestMain(m *testing.M) { os.Exit(m.Run()) }\n",
    )
    .unwrap();
    std::fs::write(p.join("plain/a_test.go"), "package plain\n").unwrap();
    assert!(go_package_has_testmain("example.com/m/leaky", &p));
    assert!(!go_package_has_testmain("example.com/m/plain", &p));
    assert!(go_package_has_testmain(
        "example.com/m/leaky",
        &p.join("plain")
    ));
    assert!(!go_package_has_testmain("example.com/other/leaky", &p));
}
