//! Lenient JUnit XML and TRX parsing (ADR 0003). Accepts the superset of
//! common flavors; malformed input yields an error or partial result, never
//! a panic.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use toe_core::model::{Outcome, TestKey};

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
    let trimmed = xml.trim_start_matches('\u{feff}');
    let root = first_element(trimmed)?;
    match root.as_str() {
        "testsuites" | "testsuite" => parse_junit(trimmed),
        "TestRun" => parse_trx(trimmed),
        _ => Err(ParseError::Unknown),
    }
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
    retries: Vec<Attempt>,
}

/// Which child element's text we are collecting.
enum Text {
    None,
    Final,
    Retry,
}

fn parse_junit(xml: &str) -> Result<Vec<Case>, ParseError> {
    let mut r = Reader::from_str(xml);
    let mut suites: Vec<String> = Vec::new();
    let mut cur: Option<Current> = None;
    let mut text = Text::None;
    let mut buf = String::new();
    let mut out = Vec::new();

    let finish = |c: Current, suites: &[String], out: &mut Vec<Case>| {
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
        });
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
                let name = e.local_name();
                match name.as_ref() {
                    b"testsuite" if !empty => suites.push(attr(e, b"name").unwrap_or_default()),
                    b"testcase" => {
                        let c = Current {
                            name: attr(e, b"name").unwrap_or_default(),
                            classname: attr(e, b"classname"),
                            file: attr(e, b"file"),
                            time: attr(e, b"time").as_deref().and_then(secs_to_ms),
                            ..Current::default()
                        };
                        if empty {
                            finish(c, &suites, &mut out);
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
                    suites.pop();
                }
                b"testcase" => {
                    if let Some(c) = cur.take() {
                        finish(c, &suites, &mut out);
                    }
                }
                b"failure" | b"error" | b"skipped" => {
                    if let (Text::Final, Some(c)) = (&text, cur.as_mut())
                        && c.final_msg.is_none()
                    {
                        c.final_msg = clip(&buf);
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
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// `hh:mm:ss.fffffff`
fn trx_duration(s: &str) -> Option<u64> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(((h * 3600.0 + m * 60.0 + sec) * 1000.0).round() as u64)
}

fn parse_trx(xml: &str) -> Result<Vec<Case>, ParseError> {
    let mut r = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<(String, Outcome, Option<u64>)> = None;
    let mut in_message = false;
    let mut msg = String::new();
    let push = |c: (String, Outcome, Option<u64>), msg: &str, out: &mut Vec<Case>| {
        out.push(Case {
            key: TestKey::new(Vec::new(), c.0),
            attempts: vec![Attempt {
                outcome: c.1,
                duration_ms: c.2,
                message: clip(msg),
            }],
        });
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
                    b"UnitTestResult" => {
                        let outcome = match attr(e, b"outcome").as_deref() {
                            Some("Passed") => Outcome::Passed,
                            Some("Failed") => Outcome::Failed,
                            Some("NotExecuted") | Some("Inconclusive") => Outcome::Skipped,
                            _ => Outcome::Errored,
                        };
                        let c = (
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
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(cases[1].key.path, vec!["toe-core".to_string()]);
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
}
