//! Client side of `toe serve` (ADR 0014 section 3): replication of local
//! runs and remote reads. Enabled by `TOE_SERVER` (+ `TOE_TOKEN`).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use toe_core::home::Repo;
use toe_core::store::Store;

pub struct Remote {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

/// Events sent per request.
const BATCH: usize = 500;
/// Push the run state at least this often, so the server knows we live.
const HEARTBEAT: Duration = Duration::from_secs(10);
/// After the run ended, keep trying to deliver this long; `toe sync` later.
const DRAIN: Duration = Duration::from_secs(5);

impl Remote {
    pub fn from_env() -> Option<Remote> {
        let base = std::env::var("TOE_SERVER").ok().filter(|v| !v.is_empty())?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Some(Remote {
            base: base.trim_end_matches('/').to_owned(),
            token: std::env::var("TOE_TOKEN").ok().filter(|v| !v.is_empty()),
            agent,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => req.header("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    fn check(
        mut resp: ureq::http::Response<ureq::Body>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let code = resp.status().as_u16();
        if code >= 400 {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or(body);
            bail!("toe server: {code} {msg}");
        }
        Ok(resp)
    }

    pub fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .auth(self.agent.get(format!("{}{path}", self.base)))
            .call()?;
        Ok(Self::check(resp)?.body_mut().read_json()?)
    }

    pub fn get_text(&self, path: &str) -> Result<String> {
        let resp = self
            .auth(self.agent.get(format!("{}{path}", self.base)))
            .call()?;
        Ok(Self::check(resp)?
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()?)
    }

    /// Sends one batch of a run; returns the last seq the server holds.
    pub fn sync_once(&self, store: &Store, repo: &Repo, run: &str, after: u64) -> Result<u64> {
        let snap = store.snapshot(run)?;
        let mut events = store.events_after(run, after)?;
        events.truncate(BATCH);
        let body = json!({
            "repo": repo,
            "workspace": store.root().display().to_string(),
            "run": snap,
            "events": events,
        });
        let resp = self
            .auth(self.agent.post(format!("{}/v1/sync", self.base)))
            .send_json(&body)?;
        let v: Value = Self::check(resp)?.body_mut().read_json()?;
        let last = v["last_seq"]
            .as_u64()
            .ok_or_else(|| anyhow!("bad sync reply"))?;
        store.set_synced(run, last)?;
        Ok(last)
    }

    /// Delivers everything the server lacks for a run, then its output.
    pub fn sync_run(&self, store: &Store, repo: &Repo, run: &str) -> Result<()> {
        let mut acked = 0;
        loop {
            let last = store.last_seq(run)?;
            acked = self.sync_once(store, repo, run, acked)?;
            if acked >= last {
                break;
            }
        }
        self.upload_output(store, repo, run)
    }

    pub fn upload_output(&self, store: &Store, repo: &Repo, run: &str) -> Result<()> {
        let path = store.run_dir(run).join("output.log");
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(());
        };
        let tail = &bytes[bytes.len().saturating_sub(16 * 1024 * 1024 - 1)..];
        let url = format!("{}/v1/repos/{}/runs/{run}/output", self.base, repo.id);
        Self::check(self.auth(self.agent.put(url)).send(tail)?)?;
        Ok(())
    }
}

/// Runs beside the supervisor: replicates the run while it happens and
/// drains after it ended. Nothing is lost if the server is unreachable:
/// the local store keeps everything and `toe sync` retries later.
pub fn replicate(
    root: &Path,
    dir: &Path,
    run: String,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let (root, dir) = (root.to_path_buf(), dir.to_path_buf());
    std::thread::spawn(move || {
        let Some(remote) = Remote::from_env() else {
            return;
        };
        let Ok(store) = Store::open_dir(&root, &dir) else {
            return;
        };
        let repo = toe_core::home::identify(&root);
        let mut acked = 0;
        let mut pushed_at: Option<Instant> = None;
        let mut ended_at: Option<Instant> = None;
        let mut backoff = Duration::from_millis(500);
        loop {
            let finished = done.load(Ordering::SeqCst);
            if finished && ended_at.is_none() {
                ended_at = Some(Instant::now());
            }
            let last = store.last_seq(&run).unwrap_or(0);
            let due =
                acked < last || pushed_at.is_none_or(|t| t.elapsed() >= HEARTBEAT) || finished;
            if due {
                match remote.sync_once(&store, &repo, &run, acked) {
                    Ok(seq) => {
                        acked = seq;
                        pushed_at = Some(Instant::now());
                        backoff = Duration::from_millis(500);
                    }
                    Err(e) => {
                        eprintln!("toe: sync to {} failed: {e}", remote.base());
                        std::thread::sleep(if finished {
                            Duration::from_millis(500)
                        } else {
                            backoff
                        });
                        backoff = (backoff * 2).min(Duration::from_secs(10));
                    }
                }
            }
            if finished && acked >= store.last_seq(&run).unwrap_or(0) && pushed_at.is_some() {
                if let Err(e) = remote.upload_output(&store, &repo, &run) {
                    eprintln!("toe: output upload failed: {e}");
                }
                return;
            }
            if ended_at.is_some_and(|t| t.elapsed() > DRAIN) {
                eprintln!("toe: run {run} not fully delivered; `toe sync` will retry");
                return;
            }
            std::thread::sleep(if acked < last {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(500)
            });
        }
    })
}
