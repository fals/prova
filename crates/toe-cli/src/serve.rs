//! `toe serve`: owns a central store for many machines and pods (ADR 0014
//! section 3). Clients keep supervising their runs locally and replicate
//! them here; reads run the same engine code as the local CLI.
//!
//! HTTP/1.1 + JSON, bearer tokens, optional TLS. Endpoints:
//!
//! ```text
//! GET  /v1/health
//! POST /v1/sync                                  {repo, workspace, run, events} -> {last_seq}
//! PUT  /v1/repos/{repo}/runs/{run}/output        captured output (<= 16 MB)
//! GET  /v1/repos                                 known repositories
//! GET  /v1/repos/{repo}/runs?limit=N             recent runs
//! GET  /v1/repos/{repo}/runs/{run}               summary
//! GET  /v1/repos/{repo}/runs/{run}/wait?until=&after=&max_wait_ms=
//! GET  /v1/repos/{repo}/runs/{run}/output?tail=N
//! GET  /v1/repos/{repo}/runs/{run}/events?after=N   JSON Lines
//! GET  /v1/repos/{repo}/history?test=&window=
//! GET  /v1/repos/{repo}/flaky?window=
//! ```

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};
use toe_core::analysis;
use toe_core::engine::{self, Until, WaitReason};
use toe_core::home::{self, Repo};
use toe_core::ids::now_ms;
use toe_core::model::Event;
use toe_core::store::{RunSnapshot, Store};

/// Longest a wait may block: below the 60 s default of many HTTP clients.
pub const MAX_WAIT: Duration = Duration::from_secs(45);
const MAX_BODY: u64 = 16 * 1024 * 1024;

/// A token and the repositories it may touch (`None`: all).
#[derive(Debug, Clone)]
pub struct Token {
    secret: String,
    repos: Option<Vec<String>>,
}

/// Lines `TOKEN` or `TOKEN repoid,repoid`; `#` comments.
pub fn parse_tokens(text: &str) -> Result<Vec<Token>> {
    let mut out = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut parts = line.split_whitespace();
        let secret = parts.next().unwrap_or_default().to_owned();
        if secret.len() < 16 {
            bail!("tokens must be at least 16 characters");
        }
        let repos = parts
            .next()
            .map(|r| r.split(',').map(str::to_owned).collect());
        out.push(Token { secret, repos });
    }
    Ok(out)
}

pub struct Config {
    pub listen: String,
    pub tokens: Vec<Token>,
    pub insecure_no_auth: bool,
    pub tls: Option<(PathBuf, PathBuf)>,
    pub threads: usize,
}

struct Ctx {
    tokens: Vec<Token>,
    open: bool,
    home: PathBuf,
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Ctx {
    /// The token's repository scope, or an error for a bad token.
    fn auth(&self, req: &Request) -> std::result::Result<Option<Vec<String>>, (u16, String)> {
        if self.open {
            return Ok(None);
        }
        let given = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .unwrap_or("");
        self.tokens
            .iter()
            .find(|t| constant_time_eq(t.secret.as_bytes(), given.as_bytes()))
            .map(|t| t.repos.clone())
            .ok_or((401, "missing or invalid bearer token".into()))
    }

    fn store(&self, repo: &str) -> std::result::Result<Store, (u16, String)> {
        let dir = self.home.join("repos").join(repo);
        if !dir.join("toe.db").exists() {
            return Err((404, format!("unknown repository {repo}")));
        }
        Store::open_dir(Path::new("/"), &dir).map_err(|e| (500, e.to_string()))
    }
}

fn valid_id(s: &str, len: usize) -> bool {
    !s.is_empty() && s.len() <= len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn query(url: &str) -> BTreeMap<String, String> {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

type Reply = std::result::Result<(u16, String, &'static str), (u16, String)>;

fn ok_json(v: impl serde::Serialize) -> Reply {
    Ok((
        200,
        serde_json::to_string(&v).map_err(|e| (500, e.to_string()))?,
        "application/json",
    ))
}

fn read_body(req: &mut Request) -> std::result::Result<Vec<u8>, (u16, String)> {
    let mut buf = Vec::new();
    req.as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut buf)
        .map_err(|e| (400, e.to_string()))?;
    if buf.len() as u64 > MAX_BODY {
        return Err((413, "body larger than 16 MB".into()));
    }
    Ok(buf)
}

fn handle(ctx: &Ctx, req: &mut Request) -> Reply {
    let url = req.url().to_owned();
    let path = url.split('?').next().unwrap_or("").to_owned();
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    if req.method() == &Method::Get && segs == ["v1", "health"] {
        return ok_json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }));
    }
    let scope = ctx.auth(req)?;
    let allowed = |repo: &str| scope.as_ref().is_none_or(|r| r.iter().any(|x| x == repo));
    let q = query(&url);
    match (req.method().clone(), segs.as_slice()) {
        (Method::Post, ["v1", "sync"]) => {
            let body = read_body(req)?;
            #[derive(serde::Deserialize)]
            struct Sync {
                repo: Repo,
                workspace: String,
                run: RunSnapshot,
                events: Vec<Event>,
            }
            let s: Sync =
                serde_json::from_slice(&body).map_err(|e| (400, format!("bad sync: {e}")))?;
            if !valid_id(&s.repo.id, 16) || !valid_id(&s.run.id, 16) {
                return Err((400, "bad repository or run id".into()));
            }
            if !allowed(&s.repo.id) {
                return Err((403, "token not allowed for this repository".into()));
            }
            let dir = ctx.home.join("repos").join(&s.repo.id);
            let store =
                Store::open_dir(Path::new(&s.workspace), &dir).map_err(|e| (500, e.to_string()))?;
            home::register(&s.repo, Path::new(&s.workspace));
            let last = store
                .apply_sync(&s.run, &s.events, now_ms())
                .map_err(|e| (409, e.to_string()))?;
            ok_json(json!({ "last_seq": last }))
        }
        (Method::Get, ["v1", "repos"]) => {
            let repos: Vec<_> = home::known_repos()
                .into_iter()
                .filter(|k| allowed(&k.id))
                .collect();
            ok_json(repos)
        }
        (method, ["v1", "repos", repo, rest @ ..]) => {
            if !valid_id(repo, 16) {
                return Err((400, "bad repository id".into()));
            }
            if !allowed(repo) {
                return Err((403, "token not allowed for this repository".into()));
            }
            repo_route(ctx, req, &method, repo, rest, &q)
        }
        _ => Err((404, format!("no route for {path}"))),
    }
}

fn repo_route(
    ctx: &Ctx,
    req: &mut Request,
    method: &Method,
    repo: &str,
    rest: &[&str],
    q: &BTreeMap<String, String>,
) -> Reply {
    let num = |k: &str, d: u64| q.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(d);
    let store = ctx.store(repo)?;
    let e500 = |e: toe_core::store::StoreError| (500, e.to_string());
    let resolve = |r: &str| store.resolve_run(r).map_err(|e| (404, e.to_string()));
    match (method, rest) {
        (Method::Get, ["runs"]) => ok_json(
            store
                .recent_runs(num("limit", 20).min(500) as usize)
                .map_err(e500)?,
        ),
        (Method::Get, ["runs", run]) => {
            let id = resolve(run)?;
            let row = store.run(&id).map_err(e500)?;
            let reason = if row.status.is_terminal() {
                WaitReason::Done
            } else {
                WaitReason::Running
            };
            let seq = store.last_seq(&id).map_err(e500)?;
            ok_json(engine::summarize(&store, &id, reason, seq).map_err(e500)?)
        }
        (Method::Get, ["runs", run, "wait"]) => {
            let id = resolve(run)?;
            let until = match q.get("until").map(String::as_str) {
                Some("stall") => Until::Stall,
                Some("done") => Until::Done,
                _ => Until::FirstFailure,
            };
            let max = Duration::from_millis(num("max_wait_ms", 30_000)).min(MAX_WAIT);
            ok_json(engine::wait(&store, &id, until, num("after", 0), max).map_err(e500)?)
        }
        (Method::Get, ["runs", run, "events"]) => {
            let id = resolve(run)?;
            let mut out = String::new();
            for e in store.events_after(&id, num("after", 0)).map_err(e500)? {
                out.push_str(&serde_json::to_string(&e).map_err(|e| (500, e.to_string()))?);
                out.push('\n');
            }
            Ok((200, out, "application/x-ndjson"))
        }
        (Method::Get, ["runs", run, "output"]) => {
            let id = resolve(run)?;
            let text = match q.get("tail").and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => engine::output_tail(&store, &id, n).join("\n"),
                None => std::fs::read_to_string(store.run_dir(&id).join("output.log"))
                    .unwrap_or_default(),
            };
            Ok((200, text, "text/plain; charset=utf-8"))
        }
        (Method::Put, ["runs", run, "output"]) => {
            if !valid_id(run, 16) {
                return Err((400, "bad run id".into()));
            }
            store.run(run).map_err(|e| (404, e.to_string()))?;
            let body = read_body(req)?;
            let dir = store.run_dir(run);
            toe_core::store::private_dir(&dir).map_err(|e| (500, e.to_string()))?;
            std::io::Write::write_all(
                &mut toe_core::store::private_file(&dir.join("output.log"))
                    .map_err(|e| (500, e.to_string()))?,
                &body,
            )
            .map_err(|e| (500, e.to_string()))?;
            ok_json(json!({ "stored": body.len() }))
        }
        (Method::Get, ["history"]) => {
            let test = q.get("test").ok_or((400, "test is required".to_owned()))?;
            let found = analysis::find_tests(&store, test, None).map_err(e500)?;
            let Some(first) = found.iter().find(|t| &t.1 == test).or(found.first()) else {
                return Err((404, format!("no test matching {test:?}")));
            };
            ok_json(analysis::stats(&store, &first.0, num("window", 50) as usize).map_err(e500)?)
        }
        (Method::Get, ["flaky"]) => {
            ok_json(analysis::flaky(&store, None, num("window", 100) as usize).map_err(e500)?)
        }
        _ => Err((404, "no such route".into())),
    }
}

pub fn serve(cfg: Config) -> Result<()> {
    let home = home::toe_home().ok_or_else(|| anyhow!("no toe home: set TOE_HOME"))?;
    toe_core::store::private_dir(&home)?;
    if cfg.tokens.is_empty() && !cfg.insecure_no_auth {
        bail!(
            "refusing to serve without tokens: give --tokens-file or TOE_SERVE_TOKENS (or --insecure-no-auth for local experiments)"
        );
    }
    let server = match &cfg.tls {
        Some((cert, key)) => Server::https(
            &cfg.listen,
            tiny_http::SslConfig {
                certificate: std::fs::read(cert).with_context(|| format!("{}", cert.display()))?,
                private_key: std::fs::read(key).with_context(|| format!("{}", key.display()))?,
            },
        ),
        None => Server::http(&cfg.listen),
    }
    .map_err(|e| anyhow!("listen on {}: {e}", cfg.listen))?;
    let server = Arc::new(server);
    let ctx = Arc::new(Ctx {
        tokens: cfg.tokens,
        open: cfg.insecure_no_auth,
        home: home.clone(),
    });
    eprintln!(
        "toe serve: listening on {} ({}), store {}",
        server.server_addr(),
        if cfg.tls.is_some() { "https" } else { "http" },
        home.display()
    );
    let workers: Vec<_> = (0..cfg.threads.max(2))
        .map(|_| {
            let (server, ctx) = (server.clone(), ctx.clone());
            std::thread::spawn(move || {
                while let Ok(mut req) = server.recv() {
                    let (code, body, ctype) = match handle(&ctx, &mut req) {
                        Ok(r) => r,
                        Err((code, msg)) => (
                            code,
                            json!({ "error": msg }).to_string(),
                            "application/json",
                        ),
                    };
                    let header = Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes())
                        .expect("static header");
                    let _ = req.respond(
                        Response::from_string(body)
                            .with_status_code(code)
                            .with_header(header),
                    );
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_scopes() {
        let t = parse_tokens("# ci\nabcdefghijklmnop\nqrstuvwxyz0123456 aaaa,bbbb\n").unwrap();
        assert_eq!(t.len(), 2);
        assert!(t[0].repos.is_none());
        assert_eq!(
            t[1].repos.as_deref(),
            Some(&["aaaa".to_string(), "bbbb".to_string()][..])
        );
        assert!(parse_tokens("short").is_err());
        assert!(
            constant_time_eq(b"abc", b"abc")
                && !constant_time_eq(b"abc", b"abd")
                && !constant_time_eq(b"a", b"ab")
        );
    }

    #[test]
    fn query_decoding_and_ids() {
        let q = query("/x?test=a%3A%3Ab+c&window=5");
        assert_eq!(q["test"], "a::b c");
        assert_eq!(q["window"], "5");
        assert!(valid_id("0123abcd", 16) && !valid_id("../etc", 16) && !valid_id("", 16));
    }
}
