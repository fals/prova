//! Whether a suite's command is one test-runner call (ADR 0031 §1).
//!
//! A quarantine sets aside a failure only when the command does nothing but
//! run the tests: a step before, after or between runner calls, or one the
//! failure skips, is out of prova's sight. The check is syntactic and
//! conservative: anything not recognised is refused, with the reason named.

use std::path::{Path, PathBuf};

use crate::config::CommandSpec;

/// Wrappers and scripts may nest this deep (`npm test` running `npm run x`).
const MAX_DEPTH: usize = 6;

/// The runner a command calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Go,
    Cargo,
    Pytest,
    Vitest,
    Jest,
    Maven,
    Gradle,
    Dotnet,
}

/// What a qualifying command calls, and the words of that call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerCall {
    pub runner: Runner,
    pub words: Vec<String>,
}

/// `Ok` when `command` is one test-runner call that a quarantine can see
/// into; `Err` names what made it something else.
pub fn check(command: &CommandSpec, cwd: &Path) -> Result<RunnerCall, String> {
    let display = command.display();
    let call = match command {
        CommandSpec::Argv(words) => analyze(words, cwd, 0),
        CommandSpec::Shell(s) => analyze_shell(s, cwd, 0),
    }?;
    // Vitest checks its coverage thresholds only when every test passed.
    if call.runner == Runner::Vitest && vitest_coverage(&call.words, cwd) {
        return Err(format!(
            "`{display}` runs Vitest with coverage: Vitest skips the coverage thresholds once a test fails"
        ));
    }
    Ok(call)
}

fn analyze_shell(script: &str, cwd: &Path, depth: usize) -> Result<RunnerCall, String> {
    let words = shell_words(script)?;
    analyze(&words, cwd, depth)
}

/// Splits one shell command line into words, refusing anything that joins
/// it to another command.
fn shell_words(script: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = script.trim().chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut i = 0;
    let chains = |what: &str| Err(format!("the command chains steps with {what}"));
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    } else if chars[i] == '`' || (chars[i] == '$' && chars.get(i + 1) == Some(&'('))
                    {
                        return chains("a command substitution");
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = next {
                    cur.push(n);
                    i += 1;
                }
            }
            '`' => return chains("a command substitution"),
            '$' if next == Some('(') => return chains("a command substitution"),
            ';' => return chains("`;`"),
            '\n' => return chains("a newline"),
            '(' | ')' => return chains("a subshell"),
            '|' if next == Some('|') => return chains("`||`"),
            '|' => return chains("a pipe (`|`)"),
            '&' => {
                let redirect = cur.ends_with(['>', '<']) || next == Some('>');
                if redirect {
                    in_word = true;
                    cur.push('&');
                } else if next == Some('&') {
                    return chains("`&&`");
                } else {
                    return chains("`&` (a background command)");
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
        i += 1;
    }
    if in_word {
        words.push(cur);
    }
    Ok(strip_redirects(words))
}

/// Drops redirections (`> out.txt`, `2>&1`): they feed no word to the runner.
fn strip_redirects(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for w in words {
        if skip_next {
            skip_next = false;
            continue;
        }
        let body = w.trim_start_matches(|c: char| c.is_ascii_digit());
        if body.starts_with(['>', '<']) {
            // `>file`, `2>&1`: whole word; `>` or `>>` alone takes the next.
            if body.trim_start_matches(['>', '<']).is_empty() {
                skip_next = true;
            }
            continue;
        }
        out.push(w);
    }
    out
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn base(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

fn join(words: &[String]) -> String {
    words.join(" ")
}

/// Options of docker and podman (`run`, `exec`) that take a value.
const CONTAINER_VALUED: &[&str] = &[
    "-v",
    "--volume",
    "-w",
    "--workdir",
    "-e",
    "--env",
    "--env-file",
    "--name",
    "-u",
    "--user",
    "--network",
    "--net",
    "-p",
    "--publish",
    "--mount",
    "--platform",
    "-l",
    "--label",
    "--add-host",
    "--cap-add",
    "--cap-drop",
    "-m",
    "--memory",
    "--cpus",
    "-h",
    "--hostname",
    "--group-add",
    "--device",
    "--tmpfs",
    "--shm-size",
    "--pull",
    "--security-opt",
    "--ulimit",
    "--userns",
    "--ipc",
    "--pid",
    "--cidfile",
    "--log-driver",
    "--restart",
    "--detach-keys",
    "--gpus",
    "--runtime",
    "--stop-signal",
    "--user-ns",
];

/// Options before a `docker compose` subcommand that take a value.
const COMPOSE_VALUED: &[&str] = &[
    "-f",
    "--file",
    "-p",
    "--project-name",
    "--profile",
    "--env-file",
    "--project-directory",
    "--parallel",
    "--ansi",
    "--progress",
];

/// Skips options (with their values) from `words`, returning the rest.
fn skip_options<'a>(words: &'a [String], valued: &[&str]) -> &'a [String] {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if w == "--" {
            return &words[i + 1..];
        }
        if !w.starts_with('-') || w == "-" {
            break;
        }
        if valued.contains(&w) {
            i += 1;
        }
        i += 1;
    }
    &words[i.min(words.len())..]
}

fn analyze(words: &[String], cwd: &Path, depth: usize) -> Result<RunnerCall, String> {
    if depth > MAX_DEPTH {
        return Err("the command wraps the runner too deep to follow".into());
    }
    // Environment assignments, then `env`, `exec`, `time`.
    let mut w: &[String] = words;
    loop {
        match w.first().map(String::as_str) {
            Some(first) if is_assignment(first) => w = &w[1..],
            Some("env") => {
                let rest = &w[1..];
                let rest = skip_options(rest, &["-u", "--unset", "-C", "--chdir", "-S"]);
                w = rest;
            }
            Some("exec" | "time") => w = &w[1..],
            _ => break,
        }
    }
    let Some(prog) = w.first() else {
        return Err("the command has no program".into());
    };
    let name = base(prog);
    let rest = &w[1..];
    let whole = join(words);
    match name {
        "make" | "gmake" | "ninja" | "just" | "task" => Err(format!("`{whole}` runs {name}")),
        "sh" | "bash" | "zsh" | "dash" | "ash" => {
            let mut i = 0;
            while i < rest.len() {
                let o = rest[i].as_str();
                if o.starts_with('-') && !o.starts_with("--") && o.contains('c') {
                    return match rest.get(i + 1) {
                        Some(script) => analyze_shell(script, cwd, depth + 1),
                        None => Err(format!("`{whole}` has no script after -c")),
                    };
                }
                if !o.starts_with('-') {
                    break;
                }
                i += 1;
            }
            Err(format!("`{whole}` runs a shell script, not a test runner"))
        }
        "docker" | "podman" | "docker-compose" | "podman-compose" => {
            container(name, rest, &whole, cwd, depth)
        }
        "npx" => {
            let inner = skip_options(rest, &["-p", "--package", "-c", "--call"]);
            analyze(inner, cwd, depth + 1)
        }
        "pnpm" | "yarn" | "bun" | "npm" => package_manager(name, rest, &whole, cwd, depth),
        "uv" | "poetry" => {
            let Some(sub) = rest.first() else {
                return Err(format!("`{whole}` is not a test runner call"));
            };
            if sub != "run" {
                return Err(format!("`{whole}` runs {name} {sub}, not a test runner"));
            }
            let inner = skip_options(
                &rest[1..],
                &[
                    "--with",
                    "--python",
                    "-p",
                    "--project",
                    "--directory",
                    "--group",
                    "--extra",
                    "--env-file",
                    "--package",
                    "--with-requirements",
                ],
            );
            analyze(inner, cwd, depth + 1)
        }
        "go" => {
            if rest.first().map(String::as_str) == Some("test") {
                Ok(call(Runner::Go, rest))
            } else {
                Err(format!("`{whole}` is not `go test`"))
            }
        }
        "gotestsum" => {
            if rest.iter().any(|a| {
                a == "--raw-command"
                    || a == "--post-run-command"
                    || a.starts_with("--post-run-command=")
            }) {
                Err(format!(
                    "`{whole}` runs another command around the tests (gotestsum --raw-command or --post-run-command)"
                ))
            } else {
                Ok(call(Runner::Go, rest))
            }
        }
        "cargo" => {
            let rest: Vec<String> = rest
                .iter()
                .filter(|a| !a.starts_with('+'))
                .cloned()
                .collect();
            let rest = &rest[..];
            match rest {
                [t, ..] if t == "test" || t == "t" => Ok(call(Runner::Cargo, rest)),
                [n, r, ..] if n == "nextest" && (r == "run" || r == "r") => {
                    Ok(call(Runner::Cargo, rest))
                }
                _ => Err(format!(
                    "`{whole}` is not `cargo test` or `cargo nextest run`"
                )),
            }
        }
        "pytest" | "py.test" => Ok(call(Runner::Pytest, rest)),
        "python" | "python3" | "py" => {
            if rest.first().map(String::as_str) == Some("-m")
                && rest.get(1).map(String::as_str) == Some("pytest")
            {
                Ok(call(Runner::Pytest, &rest[2..]))
            } else {
                Err(format!("`{whole}` is not `python -m pytest`"))
            }
        }
        "vitest" => Ok(call(Runner::Vitest, rest)),
        "jest" => Ok(call(Runner::Jest, rest)),
        "mvn" | "mvnw" | "mvnd" => {
            let goals = lifecycle_words(
                rest,
                &[
                    "-pl",
                    "-f",
                    "-P",
                    "-rf",
                    "-T",
                    "-s",
                    "-l",
                    "-t",
                    "-gs",
                    "-b",
                    "-C",
                    "--projects",
                    "--file",
                    "--activate-profiles",
                    "--resume-from",
                    "--threads",
                    "--settings",
                    "--log-file",
                    "--toolchains",
                    "--global-settings",
                    "--builder",
                ],
            );
            if goals == ["test"] {
                Ok(call(Runner::Maven, rest))
            } else {
                Err(format!(
                    "`{whole}` runs {} as Maven goals: only `test` alone is one test-runner call",
                    if goals.is_empty() {
                        "no goal".to_owned()
                    } else {
                        goals.join(", ")
                    }
                ))
            }
        }
        "gradle" | "gradlew" => {
            let tasks = lifecycle_words(
                rest,
                &[
                    "-p",
                    "--project-dir",
                    "-c",
                    "--settings-file",
                    "-b",
                    "--build-file",
                    "-g",
                    "--gradle-user-home",
                    "-I",
                    "--init-script",
                    "--tests",
                    "--include-build",
                    "--max-workers",
                ],
            );
            if tasks == ["test"] {
                Ok(call(Runner::Gradle, rest))
            } else {
                Err(format!(
                    "`{whole}` runs {} as Gradle tasks: only `test` alone is one test-runner call",
                    if tasks.is_empty() {
                        "no task".to_owned()
                    } else {
                        tasks.join(", ")
                    }
                ))
            }
        }
        "dotnet" => {
            if rest.first().map(String::as_str) == Some("test") {
                Ok(call(Runner::Dotnet, rest))
            } else {
                Err(format!("`{whole}` is not `dotnet test`"))
            }
        }
        _ => {
            // A script given the command as its one quoted argument:
            // `./dev "go test ./..."`.
            if rest.len() == 1 && rest[0].split_whitespace().count() > 1 {
                return analyze_shell(&rest[0], cwd, depth + 1);
            }
            Err(format!(
                "`{whole}` runs `{prog}`, which is not a known test runner"
            ))
        }
    }
}

fn call(runner: Runner, words: &[String]) -> RunnerCall {
    RunnerCall {
        runner,
        words: words.to_vec(),
    }
}

/// The non-option words of a Maven or Gradle command line.
fn lifecycle_words(rest: &[String], valued: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let w = rest[i].as_str();
        if w.starts_with('-') {
            if valued.contains(&w) {
                i += 1;
            }
        } else {
            out.push(w.to_owned());
        }
        i += 1;
    }
    out
}

fn container(
    name: &str,
    rest: &[String],
    whole: &str,
    cwd: &Path,
    depth: usize,
) -> Result<RunnerCall, String> {
    let mut rest = rest;
    let compose = name.ends_with("compose");
    let mut is_compose = compose;
    if !compose {
        // `docker compose ...`, after the client's own options.
        let after = skip_options(rest, &["-H", "--host", "--context", "-c", "--config"]);
        if after.first().map(String::as_str) == Some("compose") {
            is_compose = true;
            rest = &after[1..];
        } else {
            rest = after;
        }
    }
    if is_compose {
        rest = skip_options(rest, COMPOSE_VALUED);
    }
    let Some(sub) = rest.first() else {
        return Err(format!("`{whole}` is not a run or exec of a container"));
    };
    if !matches!(sub.as_str(), "run" | "exec") {
        // `docker container run|exec`.
        if sub == "container"
            && let Some(s2) = rest.get(1)
            && matches!(s2.as_str(), "run" | "exec")
        {
            return container(name, &rest[1..], whole, cwd, depth);
        }
        return Err(format!(
            "`{whole}` runs `{name} {sub}`, not a test runner in a container"
        ));
    }
    if rest.iter().any(|a| a.starts_with("--entrypoint")) {
        return Err(format!(
            "`{whole}` sets a container entrypoint: what runs is not the command given"
        ));
    }
    // The image (run) or container (exec) or service (compose), then the command.
    let args = skip_options(&rest[1..], CONTAINER_VALUED);
    match args.split_first() {
        Some((_, cmd)) if !cmd.is_empty() => analyze(cmd, cwd, depth + 1),
        _ => Err(format!(
            "`{whole}` gives the container no command: what runs is the image's own"
        )),
    }
}

fn package_manager(
    name: &str,
    rest: &[String],
    whole: &str,
    cwd: &Path,
    depth: usize,
) -> Result<RunnerCall, String> {
    // Options before the subcommand; another directory's package is not read.
    if rest.iter().take_while(|a| a.starts_with('-')).any(|a| {
        matches!(
            a.as_str(),
            "--prefix" | "-C" | "--cwd" | "-w" | "--workspace" | "--filter" | "-F" | "-r"
        ) || a.starts_with("--prefix=")
            || a.starts_with("--filter=")
    }) {
        return Err(format!("`{whole}` runs a script of another package"));
    }
    let rest = skip_options(rest, &[]);
    let Some(sub) = rest.first() else {
        return Err(format!("`{whole}` is not a script or exec"));
    };
    let after = &rest[1..];
    match sub.as_str() {
        "exec" | "x" if matches!(name, "pnpm" | "yarn" | "bun" | "npm") => {
            let inner = skip_options(after, &[]);
            analyze(inner, cwd, depth + 1)
        }
        "dlx" => Err(format!("`{whole}` downloads and runs a package")),
        "test" | "t" | "run" | "run-script" | "start" => {
            let (script, args): (&str, &[String]) = match sub.as_str() {
                "test" | "t" => ("test", after),
                "start" => ("start", after),
                _ => {
                    let a = skip_options(after, &[]);
                    match a.split_first() {
                        Some((s, r)) => (s.as_str(), r),
                        None => return Err(format!("`{whole}` names no script")),
                    }
                }
            };
            run_script(name, script, args, whole, cwd, depth)
        }
        other if matches!(name, "yarn" | "pnpm" | "bun") && !other.starts_with('-') => {
            // `yarn lint`, `pnpm lint`: a script by its name.
            run_script(name, other, after, whole, cwd, depth)
        }
        other => Err(format!("`{whole}` runs `{name} {other}`, not a script")),
    }
}

fn run_script(
    manager: &str,
    script: &str,
    args: &[String],
    whole: &str,
    cwd: &Path,
    depth: usize,
) -> Result<RunnerCall, String> {
    let path = cwd.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "`{whole}` runs a {manager} script, but there is no package.json in the run's directory"
        )
    })?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| format!("`{whole}`: package.json is not valid JSON"))?;
    let scripts = json.get("scripts");
    let Some(body) = scripts.and_then(|s| s.get(script)).and_then(|b| b.as_str()) else {
        return Err(format!(
            "`{whole}` runs the package script `{script}`, which package.json does not define"
        ));
    };
    for hook in [format!("pre{script}"), format!("post{script}")] {
        if scripts.and_then(|s| s.get(&hook)).is_some() {
            return Err(format!(
                "`{whole}` also runs the package script `{hook}` around `{script}`"
            ));
        }
    }
    // `npm test -- --reporter x`: the words after `--` are the script's own.
    let extra: Vec<&String> = args.iter().filter(|a| a.as_str() != "--").collect();
    let mut line = body.to_owned();
    for a in extra {
        line.push(' ');
        line.push_str(&shell_quote(a));
    }
    analyze_shell(&line, cwd, depth + 1).map_err(|e| {
        if e.starts_with("the command") {
            format!("`{whole}` runs the package script `{script}` (`{body}`): {e}")
        } else {
            e
        }
    })
}

fn shell_quote(a: &str) -> String {
    if a.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+".contains(c))
    {
        a.to_owned()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// Vitest with coverage: the flag, or a config (the default ones, or the
/// one `--config`/`-c` names, with the modules they import) that sets
/// thresholds. What this misses, Vitest's own `Coverage enabled` line shows.
fn vitest_coverage(words: &[String], cwd: &Path) -> bool {
    if words
        .iter()
        .any(|w| w == "--coverage" || w.starts_with("--coverage.") || w.starts_with("--coverage="))
    {
        return true;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for stem in ["vitest.config", "vite.config", "vitest.workspace"] {
        for ext in ["ts", "js", "mjs", "cjs", "mts", "cts"] {
            files.push(cwd.join(format!("{stem}.{ext}")));
        }
    }
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let value = match w {
            "--config" | "-c" | "--workspace" => {
                i += 1;
                words.get(i).map(String::as_str)
            }
            _ => w
                .strip_prefix("--config=")
                .or_else(|| w.strip_prefix("--workspace=")),
        };
        if let Some(v) = value {
            files.push(cwd.join(v));
        }
        i += 1;
    }
    // The files, then what each imports by a relative path (three levels,
    // twenty files: a config is small).
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut queue: Vec<(PathBuf, usize)> = files.into_iter().map(|f| (f, 0)).collect();
    while let Some((file, depth)) = queue.pop() {
        if seen.contains(&file) || seen.len() >= 20 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        seen.push(file.clone());
        if text.contains("thresholds")
            || (text.contains("coverage")
                && ["lines:", "statements:", "branches:", "functions:"]
                    .iter()
                    .any(|k| text.contains(k)))
        {
            return true;
        }
        if depth < 3 {
            let dir = file.parent().unwrap_or(cwd);
            for spec in relative_imports(&text) {
                for cand in import_candidates(&dir.join(spec)) {
                    queue.push((cand, depth + 1));
                }
            }
        }
    }
    false
}

/// The relative module paths a JS/TS source imports (`from './x'`,
/// `import('./x')`, `require('./x')`).
fn relative_imports(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for marker in ["from ", "import(", "require(", "import "] {
        let mut rest = text;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let r = rest.trim_start();
            let Some(q) = r.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                continue;
            };
            let body = &r[1..];
            if let Some(end) = body.find(q) {
                let spec = &body[..end];
                if spec.starts_with('.') {
                    out.push(spec);
                }
            }
        }
    }
    out
}

/// The files a relative import may mean.
fn import_candidates(base: &Path) -> Vec<PathBuf> {
    let mut out = vec![base.to_path_buf()];
    for ext in ["ts", "js", "mjs", "cjs", "mts", "cts", "json"] {
        out.push(PathBuf::from(format!("{}.{ext}", base.display())));
        out.push(base.join(format!("index.{ext}")));
    }
    out
}

/// The Go package a test's lifecycle key belongs to: its file part when
/// the key has one, else the package in its display name
/// (`example.com/m/a::TestX`; round 18: a Go test's key has no file, so the
/// `TestMain` check looked at no package at all).
pub fn go_package_of_key(key: &str) -> Option<&str> {
    let (file, display) = key.rsplit_once('\u{1f}').unwrap_or(("", key));
    if !file.is_empty() && !file.starts_with('(') {
        return Some(file);
    }
    display.split_once("::").map(|(pkg, _)| pkg)
}

/// The module path of the nearest go.mod at or above `cwd`, and its directory.
fn go_module(cwd: &Path) -> Option<(String, PathBuf)> {
    cwd.ancestors().find_map(|d| {
        let text = std::fs::read_to_string(d.join("go.mod")).ok()?;
        text.lines().find_map(|l| {
            let m = l.trim().strip_prefix("module")?.trim();
            (!m.is_empty()).then(|| (m.trim_matches('"').to_owned(), d.to_path_buf()))
        })
    })
}

/// Whether the Go package `pkg` (an import path, as the run names it) has a
/// `TestMain`: the code after `m.Run()` may check only after a pass (goleak).
pub fn go_package_has_testmain(pkg: &str, cwd: &Path) -> bool {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some((module, root)) = go_module(cwd) {
        if pkg == module {
            dirs.push(root);
        } else if let Some(rel) = pkg.strip_prefix(&format!("{module}/")) {
            dirs.push(root.join(rel));
        }
    }
    if dirs.is_empty() {
        let rel = pkg.trim_start_matches("./");
        dirs.push(cwd.join(rel));
    }
    dirs.iter().any(|d| {
        std::fs::read_dir(d).is_ok_and(|rd| {
            rd.flatten().any(|e| {
                let p = e.path();
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("_test.go"))
                    && std::fs::read_to_string(&p).is_ok_and(|t| t.contains("func TestMain("))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(s: &str) -> Result<RunnerCall, String> {
        check(
            &CommandSpec::Shell(s.into()),
            Path::new("/nonexistent-prova"),
        )
    }

    fn argv(a: &[&str]) -> Result<RunnerCall, String> {
        check(
            &CommandSpec::Argv(a.iter().map(|s| (*s).to_owned()).collect()),
            Path::new("/nonexistent-prova"),
        )
    }

    #[test]
    fn runners_alone_qualify() {
        for c in [
            "go test ./...",
            "go test -run TestX -v ./pkg 2>&1",
            "gotestsum --format testname -- ./...",
            "cargo test --workspace",
            "cargo +nightly test",
            "cargo nextest run --no-fail-fast",
            "pytest -q tests",
            "py.test",
            "python -m pytest -x",
            "python3 -m pytest",
            "vitest run",
            "jest --ci",
            "mvn -B -q test",
            "./mvnw test",
            "mvn -pl core test",
            "gradle test",
            "./gradlew test --tests Foo",
            "dotnet test",
            "FOO=1 BAR=2 go test ./...",
            "env FOO=1 go test ./...",
            "npx vitest run",
            "npx -y jest",
            "pnpm exec vitest run",
            "yarn exec jest",
            "bun exec vitest",
            "uv run pytest -q",
            "uv run --with pytest python -m pytest",
            "poetry run pytest",
            "sh -c 'go test ./...'",
            "bash -c \"cargo test\"",
            "bash -lc 'go test ./...'",
            "./dev \"go test ./...\"",
            "docker run --rm -v $PWD:/w -w /w golang:1.22 go test ./...",
            "docker exec -it dev go test ./...",
            "docker compose run --rm app pytest",
            "docker-compose exec app go test ./...",
            "podman run --rm img cargo test",
            "docker run --rm img sh -c 'go test ./...'",
        ] {
            assert!(sh(c).is_ok(), "{c}: {:?}", sh(c));
        }
    }

    #[test]
    fn argv_wrappers_qualify() {
        assert!(argv(&["./dev", "go test ./..."]).is_ok());
        assert!(argv(&["sh", "-c", "pytest -q"]).is_ok());
        assert!(argv(&["go", "test", "./..."]).is_ok());
        assert!(argv(&["docker", "run", "--rm", "img", "pytest"]).is_ok());
        assert!(argv(&["./dev", "test"]).is_err());
    }

    #[test]
    fn chains_and_unknown_programs_are_refused_with_the_reason() {
        let why = |c: &str| sh(c).expect_err(c);
        assert!(why("make test").contains("`make test` runs make"));
        assert!(why("go vet ./... && go test ./...").contains("&&"));
        assert!(why("go test ./... || true").contains("||"));
        assert!(why("go test ./... ; go vet ./...").contains("`;`"));
        assert!(why("go test ./... | tee out").contains("pipe"));
        assert!(why("go test ./... &").contains("&"));
        assert!(why("go test ./...\ngo vet ./...").contains("newline"));
        assert!(why("go test $(go list ./...)").contains("substitution"));
        assert!(why("go test `go list`").contains("substitution"));
        assert!(why("./ci.sh").contains("not a known test runner"));
        assert!(why("./dev test").contains("not a known test runner"));
        assert!(why("sh -c 'go vet ./... && go test ./...'").contains("&&"));
        assert!(why("sh script.sh").contains("shell script"));
        assert!(why("mvn verify").contains("verify"));
        assert!(why("mvn clean test").contains("clean"));
        assert!(why("gradle build").contains("build"));
        assert!(why("gradle clean test").contains("clean"));
        assert!(why("cargo build").contains("not `cargo test`"));
        assert!(why("go vet ./...").contains("not `go test`"));
        assert!(why("gotestsum --post-run-command ./x -- ./...").contains("post-run-command"));
        assert!(why("docker run --rm img").contains("no command"));
        assert!(why("docker ps").contains("docker ps"));
        assert!(why("docker run --entrypoint sh img -c x").contains("entrypoint"));
    }

    #[test]
    fn redirections_and_quotes_are_not_chains() {
        assert!(sh("go test ./... 2>&1").is_ok());
        assert!(sh("go test ./... > out.txt").is_ok());
        assert!(sh("go test -run 'A|B' ./...").is_ok());
        assert!(sh("go test -run \"A&&B\" ./...").is_ok());
        assert!(sh("mvn test > out.txt").is_ok());
    }

    #[test]
    fn a_go_tests_package_comes_from_its_key_or_name() {
        assert_eq!(
            go_package_of_key("\u{1f}example.com/m/a::TestX/sub"),
            Some("example.com/m/a")
        );
        assert_eq!(
            go_package_of_key("example.com/m\u{1f}TestX"),
            Some("example.com/m")
        );
        assert_eq!(go_package_of_key("\u{1f}TestBare"), None);
    }

    #[test]
    fn vitest_coverage_flag_is_refused() {
        let e = sh("vitest run --coverage").expect_err("flag");
        assert!(e.contains("coverage"), "{e}");
    }
}
