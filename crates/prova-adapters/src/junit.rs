//! Lenient JUnit XML and TRX parsing (ADR 0003). Accepts the superset of
//! common flavors; malformed input yields an error or partial result, never
//! a panic.

use prova_core::model::{Outcome, TestKey};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

const MAX_MESSAGE: usize = 4000;

#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub outcome: Outcome,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
}

/// One test with its attempts in order; the last attempt is the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub key: TestKey,
    pub attempts: Vec<Attempt>,
    /// Body of the final `<failure>`/`<error>` (stack trace, captured
    /// output), unclipped: kept as the failing test's own output.
    pub output: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("xml: {0}")]
    Xml(String),
    #[error("not a JUnit or TRX document")]
    Unknown,
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .map(|a| {
            a.unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
        })
}

fn secs_to_ms(s: &str) -> Option<u64> {
    let v: f64 = s.trim().replace(',', "").parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| (v * 1000.0).round() as u64)
}

fn clip(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let mut out: String = t.chars().take(MAX_MESSAGE).collect();
    if out.len() < t.len() {
        out.push_str(" ...");
    }
    Some(out)
}

pub fn parse(xml: &str) -> Result<Vec<Case>, ParseError> {
    parse_checked(xml).map(|(cases, _)| cases)
}

/// The cases, and what shows the file is incomplete: it ends before its
/// root closes, or a suite's header counts more tests than it holds. Such a
/// file cannot verify a pass.
pub fn parse_checked(xml: &str) -> Result<(Vec<Case>, Vec<String>), ParseError> {
    let trimmed = xml.trim_start_matches('\u{feff}');
    let root = first_element(trimmed)?;
    match root.as_str() {
        "testsuites" | "testsuite" => parse_junit(trimmed),
        "TestRun" => parse_trx(trimmed).map(|c| (c, Vec::new())),
        _ => Err(ParseError::Unknown),
    }
    .map(|(c, problems)| (number_repeats(c), problems))
}

/// Each `<testcase>` is its own result: Gradle names a JUnit 5 repetition by
/// its display name, so two methods' `repetition 1 of 2` share classname
/// and name, and the framework counts both. Repeats within one file get
/// " #2", " #3" in document order, so a failing duplicate never disappears.
fn number_repeats(mut cases: Vec<Case>) -> Vec<Case> {
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for c in &mut cases {
        let n = seen.entry(c.key.unique()).or_insert(0);
        *n += 1;
        if *n > 1 {
            c.key.name = format!("{} #{n}", c.key.name);
        }
    }
    cases
}

fn first_element(xml: &str) -> Result<String, ParseError> {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                return Ok(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
            }
            Ok(Event::Eof) => return Err(ParseError::Unknown),
            Err(e) => return Err(ParseError::Xml(e.to_string())),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Current {
    name: String,
    classname: Option<String>,
    file: Option<String>,
    time: Option<u64>,
    final_kind: Option<Outcome>,
    final_msg: Option<String>,
    final_body: Option<String>,
    retries: Vec<Attempt>,
}

/// A `<testsuite>` being read: what its header says failed, and what its
/// test cases show.
#[derive(Default)]
struct Frame {
    header_tests: u64,
    cases: u64,
    header_failed: u64,
    cases_failed: u64,
    has_child_suite: bool,
    /// Catch2 (`hostname="tbd"`) and doctest (`status=` on test cases) put
    /// the number of assertions in `tests=`: it says nothing about how many
    /// test cases the file should hold.
    counts_assertions: bool,
}

fn header_count(e: &BytesStart<'_>, name: &[u8]) -> u64 {
    attr(e, name)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Which child element's text we are collecting.
enum Text {
    None,
    Final,
    Retry,
}

fn parse_junit(xml: &str) -> Result<(Vec<Case>, Vec<String>), ParseError> {
    let mut r = Reader::from_str(xml);
    let mut suites: Vec<String> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut cur: Option<Current> = None;
    let mut text = Text::None;
    let mut buf = String::new();
    let mut out = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // A suite whose header says more tests failed than its test cases show
    // (a class that failed to initialize, a `ignoreFailures` build): the
    // gap is an error of the suite itself.
    let close_suite = |f: Frame, name: &str, out: &mut Vec<Case>, problems: &mut Vec<String>| {
        if !f.has_child_suite && !f.counts_assertions && f.header_tests > f.cases {
            problems.push(format!(
                "suite {name:?} counts {} tests in its header but the file holds {}",
                f.header_tests, f.cases
            ));
        }
        if !f.has_child_suite && f.header_failed > f.cases_failed {
            out.push(Case {
                key: TestKey::new(vec![name.to_owned()], "(suite)"),
                attempts: vec![Attempt {
                    outcome: Outcome::Errored,
                    duration_ms: None,
                    message: Some(format!(
                        "the suite's header counts {} failure(s) or error(s) but its test cases show {}: a setup or class initialization error?",
                        f.header_failed, f.cases_failed
                    )),
                }],
                output: None,
            });
        }
    };

    let finish = |c: Current, suites: &[String], out: &mut Vec<Case>, frames: &mut [Frame]| {
        if let Some(f) = frames.last_mut()
            && c.final_kind.is_some_and(|k| k.is_failure())
        {
            f.cases_failed += 1;
        }
        if let Some(f) = frames.last_mut() {
            f.cases += 1;
        }
        let path = match &c.classname {
            Some(cn) if !cn.is_empty() && *cn != c.name => vec![cn.clone()],
            _ => suites.last().cloned().into_iter().collect(),
        };
        let mut attempts = c.retries;
        attempts.push(Attempt {
            outcome: c.final_kind.unwrap_or(Outcome::Passed),
            duration_ms: c.time,
            message: c.final_msg,
        });
        out.push(Case {
            key: TestKey {
                file: c.file,
                path,
                name: c.name,
            },
            attempts,
            output: c.final_body,
        });
    };

    loop {
        let ev = match r.read_event() {
            Ok(ev) => ev,
            // Truncated or broken file: keep what was complete.
            Err(_) if !out.is_empty() => {
                problems.push(
                    "the file is cut short (invalid XML after its last complete test)".into(),
                );
                break;
            }
            Err(e) => return Err(ParseError::Xml(e.to_string())),
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let name = e.local_name();
                match name.as_ref() {
                    b"testsuite" => {
                        if let Some(parent) = frames.last_mut() {
                            parent.has_child_suite = true;
                        }
                        let f = Frame {
                            header_tests: header_count(e, b"tests"),
                            header_failed: header_count(e, b"failures")
                                + header_count(e, b"errors"),
                            counts_assertions: attr(e, b"hostname").as_deref() == Some("tbd")
                                || attr(e, b"assertions").is_some(),
                            ..Frame::default()
                        };
                        let name = attr(e, b"name").unwrap_or_default();
                        if empty {
                            close_suite(f, &name, &mut out, &mut problems);
                        } else {
                            suites.push(name);
                            frames.push(f);
                        }
                    }
                    b"testcase" => {
                        if attr(e, b"status").is_some()
                            && let Some(f) = frames.last_mut()
                        {
                            f.counts_assertions = true;
                        }
                        let c = Current {
                            name: attr(e, b"name").unwrap_or_default(),
                            classname: attr(e, b"classname"),
                            file: attr(e, b"file"),
                            time: attr(e, b"time").as_deref().and_then(secs_to_ms),
                            ..Current::default()
                        };
                        if empty {
                            finish(c, &suites, &mut out, &mut frames);
                        } else {
                            cur = Some(c);
                        }
                    }
                    kind @ (b"failure" | b"error" | b"skipped") => {
                        if let Some(c) = cur.as_mut() {
                            c.final_kind = Some(match kind {
                                b"failure" => Outcome::Failed,
                                b"error" => Outcome::Errored,
                                _ => Outcome::Skipped,
                            });
                            c.final_msg = attr(e, b"message").as_deref().and_then(clip);
                            if !empty {
                                text = Text::Final;
                                buf.clear();
                            }
                        }
                    }
                    kind @ (b"flakyFailure" | b"flakyError" | b"rerunFailure" | b"rerunError") => {
                        if let Some(c) = cur.as_mut() {
                            let outcome = if kind.ends_with(b"Error") {
                                Outcome::Errored
                            } else {
                                Outcome::Failed
                            };
                            c.retries.push(Attempt {
                                outcome,
                                duration_ms: attr(e, b"time").as_deref().and_then(secs_to_ms),
                                message: attr(e, b"message").as_deref().and_then(clip),
                            });
                            if !empty {
                                text = Text::Retry;
                                buf.clear();
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if !matches!(text, Text::None) => {
                buf.push_str(&String::from_utf8_lossy(&t));
            }
            Event::CData(t) if !matches!(text, Text::None) => {
                buf.push_str(&String::from_utf8_lossy(&t));
            }
            Event::GeneralRef(rf) if !matches!(text, Text::None) => {
                buf.push_str(match &*rf {
                    b"lt" => "<",
                    b"gt" => ">",
                    b"amp" => "&",
                    b"quot" => "\"",
                    b"apos" => "'",
                    _ => "",
                });
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"testsuite" => {
                    let name = suites.pop().unwrap_or_default();
                    if let Some(f) = frames.pop() {
                        close_suite(f, &name, &mut out, &mut problems);
                    }
                }
                b"testcase" => {
                    if let Some(c) = cur.take() {
                        finish(c, &suites, &mut out, &mut frames);
                    }
                }
                b"failure" | b"error" | b"skipped" => {
                    if let (Text::Final, Some(c)) = (&text, cur.as_mut()) {
                        if c.final_msg.is_none() {
                            c.final_msg = clip(&buf);
                        }
                        let body = buf.trim();
                        if !body.is_empty() && c.final_kind.is_some_and(|k| k.is_failure()) {
                            c.final_body = Some(body.to_owned());
                        }
                    }
                    text = Text::None;
                }
                b"flakyFailure" | b"flakyError" | b"rerunFailure" | b"rerunError" => {
                    if let (Text::Retry, Some(c)) = (&text, cur.as_mut())
                        && let Some(last) = c.retries.last_mut()
                        && last.message.is_none()
                    {
                        last.message = clip(&buf);
                    }
                    text = Text::None;
                }
                _ => {}
            },
            Event::Eof => {
                if !frames.is_empty() || cur.is_some() {
                    problems.push("the file ends before its root element closes".into());
                }
                break;
            }
            _ => {}
        }
    }
    Ok((out, problems))
}

/// `hh:mm:ss.fffffff`
fn trx_duration(s: &str) -> Option<u64> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(((h * 3600.0 + m * 60.0 + sec) * 1000.0).round() as u64)
}

/// `Ns.Class, Assembly, Version=...` (MSTest) to `Ns.Class`.
fn trx_class(raw: &str) -> &str {
    raw.split(',').next().unwrap_or(raw).trim()
}

/// NUnit and MSTest put the bare method name in `testName`, so two classes'
/// `Same` would merge; the class comes from the test definition. xUnit
/// already writes `Class.Method(theory args)`: kept as is, which is also the
/// name the live VSTest text shows for it.
fn trx_name(class: Option<&str>, test_name: &str) -> String {
    match class {
        Some(c) if !c.is_empty() && !test_name.starts_with(&format!("{c}.")) => {
            format!("{c}.{test_name}")
        }
        _ => test_name.to_owned(),
    }
}

fn parse_trx(xml: &str) -> Result<Vec<Case>, ParseError> {
    let mut r = Reader::from_str(xml);
    // (test id, case with the bare testName as key)
    let mut out: Vec<(Option<String>, Case)> = Vec::new();
    let mut classes: std::collections::HashMap<String, String> = Default::default();
    let mut def_id: Option<String> = None;
    let mut cur: Option<(Option<String>, String, Outcome, Option<u64>)> = None;
    let mut in_message = false;
    let mut msg = String::new();
    let push = |c: (Option<String>, String, Outcome, Option<u64>),
                msg: &str,
                out: &mut Vec<(Option<String>, Case)>| {
        out.push((
            c.0,
            Case {
                key: TestKey::new(Vec::new(), c.1),
                attempts: vec![Attempt {
                    outcome: c.2,
                    duration_ms: c.3,
                    message: clip(msg),
                }],
                output: (c.2.is_failure() && !msg.trim().is_empty()).then(|| msg.trim().to_owned()),
            },
        ));
    };
    loop {
        let ev = match r.read_event() {
            Ok(ev) => ev,
            // Truncated or broken file: keep what was complete.
            Err(_) if !out.is_empty() => break,
            Err(e) => return Err(ParseError::Xml(e.to_string())),
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    b"UnitTest" => def_id = attr(e, b"id").filter(|_| !empty),
                    b"TestMethod" => {
                        if let (Some(id), Some(class)) = (def_id.as_ref(), attr(e, b"className")) {
                            classes.insert(id.clone(), trx_class(&class).to_owned());
                        }
                    }
                    b"UnitTestResult" => {
                        let outcome = match attr(e, b"outcome").as_deref() {
                            Some("Passed") => Outcome::Passed,
                            Some("Failed") => Outcome::Failed,
                            Some("NotExecuted") | Some("Inconclusive") => Outcome::Skipped,
                            _ => Outcome::Errored,
                        };
                        let c = (
                            attr(e, b"testId"),
                            attr(e, b"testName").unwrap_or_default(),
                            outcome,
                            attr(e, b"duration").as_deref().and_then(trx_duration),
                        );
                        msg.clear();
                        if empty {
                            push(c, "", &mut out);
                        } else {
                            cur = Some(c);
                        }
                    }
                    b"Message" if cur.is_some() && !empty => in_message = true,
                    _ => {}
                }
            }
            Event::Text(t) if in_message => msg.push_str(&String::from_utf8_lossy(&t)),
            Event::End(e) => match e.local_name().as_ref() {
                b"Message" => in_message = false,
                b"UnitTest" => def_id = None,
                b"UnitTestResult" => {
                    if let Some(c) = cur.take() {
                        push(c, &msg, &mut out);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    // Definitions come after the results in a TRX file: resolve at the end.
    Ok(out
        .into_iter()
        .map(|(id, mut case)| {
            let class = id.and_then(|i| classes.get(&i).map(String::as_str));
            case.key.name = trx_name(class, &case.key.name);
            case
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Catch2 and doctest reports (README-assertion-counters.md): their
    /// `tests=` counts assertions, so a header above the test cases is not
    /// a cut report and the pass is not made unverified.
    #[test]
    fn real_assertion_counting_reports_are_not_called_incomplete() {
        for (name, cases) in [
            ("catch2-selftest.xml", 1045),
            ("doctest-assertion-macros.xml", 23),
        ] {
            let (parsed, problems) = parse_checked(&fixture(name)).unwrap();
            assert!(parsed.len() >= cases, "{name}: {} results", parsed.len());
            assert!(problems.is_empty(), "{name}: {problems:?}");
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/fixtures/junit/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn outcomes(cases: &[Case]) -> Vec<(String, Vec<Outcome>)> {
        cases
            .iter()
            .map(|c| {
                (
                    c.key.display(),
                    c.attempts.iter().map(|a| a.outcome).collect(),
                )
            })
            .collect()
    }

    /// Catch2 and doctest put the number of assertions in `tests=`: a
    /// passing run must stay verified, while a header that counts more tests
    /// than a plain JUnit file holds is still incomplete.
    #[test]
    fn header_counts_of_assertion_counting_generators_are_not_missing_tests() {
        let catch2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="exe" errors="0" failures="0" tests="12" hostname="tbd" time="0.003" timestamp="2024-05-01T10:00:00Z">
    <testcase classname="exe.global" name="Factorial" time="0.001"/>
    <testcase classname="exe.global" name="Sum" time="0.001"/>
    <system-out/>
    <system-err/>
  </testsuite>
</testsuites>"#;
        let (cases, problems) = parse_checked(catch2).unwrap();
        assert_eq!(cases.len(), 2);
        assert!(problems.is_empty(), "{problems:?}");
        let doctest = r#"<testsuites>
  <testsuite name="exe" errors="0" failures="0" tests="9">
    <testcase classname="a.cpp" name="adds" status="run"/>
    <testcase classname="a.cpp" name="subs" status="run"/>
  </testsuite>
</testsuites>"#;
        let (_, problems) = parse_checked(doctest).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        // The truncated plain JUnit file still warns.
        let plain = r#"<testsuite name="s" tests="2" errors="0" failures="0">
            <testcase classname="s" name="a" time="0.1"/></testsuite>"#;
        let (_, problems) = parse_checked(plain).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    /// Round 14: a suite's header counts an error no test case shows (a
    /// class that failed to initialize; Gradle `ignoreFailures`).
    #[test]
    fn a_suite_header_with_more_failures_than_cases_is_an_error() {
        let xml = r#"<testsuite name="com.acme.CalcTest" tests="3" errors="1" failures="0">
            <testcase classname="com.acme.CalcTest" name="adds" time="0.1"/>
            <testcase classname="com.acme.CalcTest" name="subs" time="0.1"/>
            <system-err>java.lang.ExceptionInInitializerError</system-err></testsuite>"#;
        let cases = parse(xml).unwrap();
        assert_eq!(
            outcomes(&cases),
            vec![
                ("com.acme.CalcTest::adds".into(), vec![Outcome::Passed]),
                ("com.acme.CalcTest::subs".into(), vec![Outcome::Passed]),
                ("com.acme.CalcTest::(suite)".into(), vec![Outcome::Errored]),
            ]
        );
        // A header that agrees with its cases adds nothing.
        let ok = r#"<testsuite name="s" tests="2" failures="1"><testcase classname="s" name="a"><failure message="x"/></testcase><testcase classname="s" name="b"/></testsuite>"#;
        assert_eq!(parse(ok).unwrap().len(), 2);
        // Nested suites: only the leaf's own header counts.
        let nested = r#"<testsuites><testsuite name="outer" failures="1"><testsuite name="inner" failures="1"><testcase classname="i" name="a"><failure message="x"/></testcase></testsuite></testsuite></testsuites>"#;
        assert_eq!(parse(nested).unwrap().len(), 1);
    }

    #[test]
    fn surefire_flavor() {
        let cases = parse(&fixture("surefire.xml")).unwrap();
        assert_eq!(
            outcomes(&cases),
            vec![
                ("com.acme.CalcTest::adds".into(), vec![Outcome::Passed]),
                ("com.acme.CalcTest::divides".into(), vec![Outcome::Failed]),
                ("com.acme.CalcTest::io".into(), vec![Outcome::Errored]),
                ("com.acme.CalcTest::later".into(), vec![Outcome::Skipped]),
                (
                    "com.acme.CalcTest::flaky".into(),
                    vec![Outcome::Failed, Outcome::Passed]
                ),
            ]
        );
        assert_eq!(
            cases[1].attempts[0].message.as_deref(),
            Some("expected:<2> but was:<3>")
        );
        assert_eq!(cases[0].attempts[0].duration_ms, Some(12));
    }

    #[test]
    fn pytest_flavor_uses_text_when_no_message() {
        let cases = parse(&fixture("pytest.xml")).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[1].key.path, vec!["tests.test_parse".to_string()]);
        assert!(
            cases[1].attempts[0]
                .message
                .as_deref()
                .unwrap()
                .contains("assert 1 == 2")
        );
    }

    #[test]
    fn nextest_flavor_with_retries() {
        let cases = parse(&fixture("nextest.xml")).unwrap();
        let o = outcomes(&cases);
        assert_eq!(o[0].1, vec![Outcome::Passed]);
        assert_eq!(
            o[1].1,
            vec![Outcome::Failed, Outcome::Failed, Outcome::Failed]
        );
        assert_eq!(
            cases[1].key.path,
            vec!["prova-core".to_string()],
            "captured output keeps its old name"
        );
    }

    #[test]
    fn gtest_and_vitest_flavors() {
        let g = parse(&fixture("gtest.xml")).unwrap();
        assert_eq!(
            outcomes(&g)[1],
            ("MathTest::Divides".into(), vec![Outcome::Failed])
        );
        let v = parse(&fixture("vitest.xml")).unwrap();
        assert_eq!(v[0].key.path, vec!["src/sum.test.ts".to_string()]);
        assert_eq!(v[1].attempts[0].outcome, Outcome::Failed);
    }

    #[test]
    fn trx() {
        let cases = parse(&fixture("results.trx")).unwrap();
        assert_eq!(
            outcomes(&cases),
            vec![
                ("Adds".into(), vec![Outcome::Passed]),
                ("Divides".into(), vec![Outcome::Failed]),
                ("Later".into(), vec![Outcome::Skipped]),
            ]
        );
        assert_eq!(
            cases[1].attempts[0].message.as_deref(),
            Some("Assert.AreEqual failed")
        );
        assert_eq!(cases[0].attempts[0].duration_ms, Some(1500));
    }

    #[test]
    fn malformed_input_never_panics() {
        for bad in [
            "",
            "<",
            "<testsuite><testcase name='a'>",
            "<testsuites><testcase",
            "<html/>",
            "\u{0}\u{1}",
        ] {
            let _ = parse(bad);
        }
        assert!(matches!(parse("<html/>"), Err(ParseError::Unknown)));
        // Truncated file still yields what was complete.
        let partial = parse("<testsuite name='s'><testcase name='a'/><testcase name='b'>");
        if let Ok(c) = partial {
            assert_eq!(c.len(), 1);
        }
    }

    #[test]
    fn trx_keys_carry_the_class_from_the_test_definition() {
        let trx = r#"<TestRun xmlns="x"><Results>
<UnitTestResult testId="1" testName="Same" outcome="Passed" />
<UnitTestResult testId="2" testName="Same" outcome="Failed"><Output><ErrorInfo><Message>boom</Message></ErrorInfo></Output></UnitTestResult>
<UnitTestResult testId="3" testName="Same" outcome="Passed" />
<UnitTestResult testId="4" testName="Same" outcome="Failed" />
<UnitTestResult testId="5" testName="Xu.CalcTests.Slash(s: &quot;x/y&quot;)" outcome="Passed" />
</Results><TestDefinitions>
<UnitTest name="Same" id="1"><TestMethod codeBase="a.dll" className="Nu.CalcTests" name="Same" /></UnitTest>
<UnitTest name="Same" id="2"><TestMethod codeBase="a.dll" className="Nu.CalcTests+Inner" name="Same" /></UnitTest>
<UnitTest name="Same" id="3"><TestMethod codeBase="a.dll" className="Nu.OtherTests" name="Same" /></UnitTest>
<UnitTest name="Same" id="4"><TestMethod codeBase="a.dll" className="Ms.CalcTests, Ms, Version=1.0.0.0, Culture=neutral, PublicKeyToken=null" name="Same" /></UnitTest>
<UnitTest name="Slash" id="5"><TestMethod codeBase="a.dll" className="Xu.CalcTests" name="Slash" /></UnitTest>
</TestDefinitions></TestRun>"#;
        let cases = parse(trx).unwrap();
        assert_eq!(
            outcomes(&cases),
            vec![
                ("Nu.CalcTests.Same".into(), vec![Outcome::Passed]),
                ("Nu.CalcTests+Inner.Same".into(), vec![Outcome::Failed]),
                ("Nu.OtherTests.Same".into(), vec![Outcome::Passed]),
                ("Ms.CalcTests.Same".into(), vec![Outcome::Failed]),
                (
                    "Xu.CalcTests.Slash(s: \"x/y\")".into(),
                    vec![Outcome::Passed]
                ),
            ]
        );
    }

    #[test]
    fn repeated_names_in_one_file_are_each_a_result() {
        let xml = r#"<testsuite name="s">
<testcase classname="C" name="repetition 1 of 2" />
<testcase classname="C" name="repetition 2 of 2" />
<testcase classname="C" name="repetition 1 of 2"><failure message="bad" /></testcase>
<testcase classname="C" name="repetition 1 of 2" />
</testsuite>"#;
        let cases = parse(xml).unwrap();
        assert_eq!(
            outcomes(&cases),
            vec![
                ("C::repetition 1 of 2".into(), vec![Outcome::Passed]),
                ("C::repetition 2 of 2".into(), vec![Outcome::Passed]),
                ("C::repetition 1 of 2 #2".into(), vec![Outcome::Failed]),
                ("C::repetition 1 of 2 #3".into(), vec![Outcome::Passed]),
            ]
        );
    }
}
