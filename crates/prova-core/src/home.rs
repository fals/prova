//! Where prova keeps data (ADR 0014): one central home per installation,
//! one SQLite file per repository, and a small catalog.
//!
//! ```text
//! $PROVA_HOME/catalog.db
//! $PROVA_HOME/repos/<repo-id>/{prova.db, runs/, artifacts/, experiments/, worktrees/}
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::ids::{now_ms, sha256_hex};

/// `PROVA_HOME`, else `$XDG_DATA_HOME/prova`, else `~/.local/share/prova`.
pub fn prova_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("PROVA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(h));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("prova"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".local/share/prova"))
}

/// Which repository a workspace belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo {
    /// Stable id: the same for every clone, worktree and pod of a repository.
    pub id: String,
    /// "git" (from the root commit) or "local" (from the path; no history in git).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub name: String,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// Removes credentials from a remote URL (`https://user:token@host/x`).
pub fn strip_credentials(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('@') {
            Some((_, host))
                if !rest.split('/').next().unwrap_or("").is_empty()
                    && rest.find('@') < rest.find('/') =>
            {
                format!("{scheme}://{host}")
            }
            _ => url.to_owned(),
        },
        None => url.to_owned(),
    }
}

pub fn identify(root: &Path) -> Repo {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Oldest root commit: deterministic even for repositories with several roots.
    if let Some(roots) = git(root, &["rev-list", "--max-parents=0", "HEAD"]) {
        let root_commit = roots.lines().last().unwrap_or("").to_owned();
        let origin = git(root, &["remote", "get-url", "origin"]).map(|u| strip_credentials(&u));
        let name = origin
            .as_deref()
            .and_then(|o| {
                o.trim_end_matches('/')
                    .trim_end_matches(".git")
                    .rsplit(['/', ':'])
                    .next()
            })
            .map(str::to_owned)
            .unwrap_or(name);
        return Repo {
            id: sha256_hex(format!("git:{root_commit}").as_bytes())[..16].to_owned(),
            kind: "git",
            root_commit: Some(root_commit),
            origin,
            name,
        };
    }
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    Repo {
        id: sha256_hex(format!("local:{}", canonical.display()).as_bytes())[..16].to_owned(),
        kind: "local",
        root_commit: None,
        origin: None,
        name,
    }
}

/// Where a workspace's data lives.
#[derive(Debug, Clone)]
pub struct Location {
    pub dir: PathBuf,
    pub repo: Repo,
    /// The central home was not usable; data stays in `<root>/.prova`.
    pub fallback: bool,
}

/// The data directory of `root` if it already holds a store; creates
/// nothing (shell completion asks on every TAB).
pub fn existing(root: &Path) -> Option<PathBuf> {
    let repo = identify(root);
    let central = prova_home().map(|h| h.join("repos").join(&repo.id));
    [central, Some(root.join(".prova"))]
        .into_iter()
        .flatten()
        .find(|d| d.join("prova.db").exists())
}

pub fn locate(root: &Path) -> Location {
    let repo = identify(root);
    if let Some(home) = prova_home() {
        let dir = home.join("repos").join(&repo.id);
        if crate::store::private_dir(&dir).is_ok() {
            return Location {
                dir,
                repo,
                fallback: false,
            };
        }
    }
    Location {
        dir: root.join(".prova"),
        repo,
        fallback: true,
    }
}

fn open_catalog(home: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(home.join("catalog.db"))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS repos (
             id TEXT PRIMARY KEY, kind TEXT NOT NULL, root_commit TEXT, origin TEXT,
             name TEXT NOT NULL, first_seen_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS workspaces (
             repo_id TEXT NOT NULL REFERENCES repos (id), path TEXT NOT NULL,
             last_seen_ms INTEGER NOT NULL, PRIMARY KEY (repo_id, path)
         ) STRICT;",
    )?;
    Ok(conn)
}

/// Records that `root` belongs to `repo`. Best effort: a read-only or
/// busy catalog never blocks a run.
pub fn register(repo: &Repo, root: &Path) {
    let Some(home) = prova_home() else { return };
    let Ok(conn) = open_catalog(&home) else {
        return;
    };
    let now = now_ms() as i64;
    let _ = conn.execute(
        "INSERT INTO repos (id, kind, root_commit, origin, name, first_seen_ms, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (id) DO UPDATE SET origin = COALESCE(?4, origin), last_seen_ms = ?6",
        params![
            repo.id,
            repo.kind,
            repo.root_commit,
            repo.origin,
            repo.name,
            now
        ],
    );
    let _ = conn.execute(
        "INSERT INTO workspaces (repo_id, path, last_seen_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (repo_id, path) DO UPDATE SET last_seen_ms = ?3",
        params![repo.id, root.to_string_lossy(), now],
    );
}

/// A repository known to this home, with its most recent workspace.
#[derive(Debug, Clone, Serialize)]
pub struct Known {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub workspace: String,
    pub dir: PathBuf,
}

/// Every repository with a store in this home, most recently seen first.
pub fn known_repos() -> Vec<Known> {
    let Some(home) = prova_home() else {
        return Vec::new();
    };
    let Ok(conn) = open_catalog(&home) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT r.id, r.name, r.origin,
                (SELECT path FROM workspaces w WHERE w.repo_id = r.id ORDER BY last_seen_ms DESC LIMIT 1)
         FROM repos r ORDER BY r.last_seen_ms DESC",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    });
    let Ok(rows) = rows else { return Vec::new() };
    rows.flatten()
        .map(|(id, name, origin, ws)| Known {
            dir: home.join("repos").join(&id),
            id,
            name,
            origin,
            workspace: ws.unwrap_or_default(),
        })
        .filter(|k| k.dir.join("prova.db").exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_stripped_from_remotes() {
        assert_eq!(
            strip_credentials("https://bob:tok3n@github.com/a/b.git"),
            "https://github.com/a/b.git"
        );
        assert_eq!(
            strip_credentials("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(
            strip_credentials("https://github.com/a/b"),
            "https://github.com/a/b"
        );
    }

    #[test]
    fn local_identity_depends_on_path() {
        let a = std::env::temp_dir();
        assert_eq!(identify(&a), identify(&a));
        assert_eq!(identify(&a).kind, "local");
        assert_ne!(identify(&a).id, identify(&a.join("..")).id);
    }
}
