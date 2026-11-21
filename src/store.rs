use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::LinkParams;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
  ns TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (ns, key));
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS token (
  id INTEGER PRIMARY KEY CHECK (id = 1), access_token TEXT NOT NULL, client_id TEXT NOT NULL,
  expires_at TEXT NOT NULL, obtained_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS nodes (thread TEXT PRIMARY KEY, role TEXT NOT NULL, at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS links (
  child TEXT PRIMARY KEY, parent TEXT NOT NULL, closed INTEGER NOT NULL, data TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS links_parent ON links (parent);
CREATE TABLE IF NOT EXISTS pending (
  parent TEXT NOT NULL, label TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (parent, label));
CREATE TABLE IF NOT EXISTS messages (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, from_id TEXT NOT NULL, to_id TEXT NOT NULL,
  kind TEXT NOT NULL, body TEXT NOT NULL, at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS messages_from ON messages (from_id, seq);
CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT, to_id TEXT NOT NULL, text TEXT NOT NULL,
  handle TEXT NOT NULL, queued TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS waiting (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, id TEXT NOT NULL, text TEXT NOT NULL,
  from_label TEXT, link TEXT, opened_at TEXT NOT NULL, resolved_at TEXT, note TEXT);
CREATE UNIQUE INDEX IF NOT EXISTS waiting_open ON waiting (owner, id) WHERE resolved_at IS NULL;
CREATE TABLE IF NOT EXISTS asks (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, thread TEXT NOT NULL, open INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS merge_waits (
  repo TEXT NOT NULL, pr INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (repo, pr));
CREATE TABLE IF NOT EXISTS contexts (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pool_members (
  pool TEXT NOT NULL, instance TEXT NOT NULL, credit INTEGER NOT NULL, PRIMARY KEY (pool, instance));
CREATE TABLE IF NOT EXISTS instance_limits (instance TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pool_picks (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, pool TEXT NOT NULL, instance TEXT NOT NULL, at TEXT NOT NULL,
  handle TEXT, data TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS pool_picks_pool ON pool_picks (pool, seq);
CREATE INDEX IF NOT EXISTS pool_picks_handle ON pool_picks (handle) WHERE handle IS NOT NULL;
CREATE TABLE IF NOT EXISTS limit_stops (
  thread TEXT NOT NULL, run TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (thread, run));
CREATE TABLE IF NOT EXISTS new_issues (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pr_seen (
  repo TEXT NOT NULL, pr INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (repo, pr));
CREATE TABLE IF NOT EXISTS instance_folders (
  instance TEXT PRIMARY KEY, driver TEXT NOT NULL, folder TEXT NOT NULL, at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS slot_resolutions (
  key TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, data TEXT NOT NULL, at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pr_events (
  repo TEXT NOT NULL, pr INTEGER NOT NULL, event TEXT NOT NULL, ticket TEXT NOT NULL, at TEXT NOT NULL,
  PRIMARY KEY (repo, pr, event, ticket));
"#;

const KEEP_MESSAGES: i64 = 5000;
const KEEP_PICKS: i64 = 2000;
const UNSETTLED: &str = "json_extract(data, '$.settledAt') IS NULL";

pub type WaitingRow = (WaitingItem, Option<Value>, Option<(String, Option<String>)>, i64);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub child: String,
    pub parent: String,
    pub label: String,
    pub role: String,
    pub parent_role: Option<String>,
    pub params: LinkParams,
    pub link: Option<String>,
    pub opened: String,
    pub last_up: String,
    pub last_seen: Option<String>,
    pub last_nudge: Option<String>,
    pub nudges: i64,
    pub escalated: bool,
    pub reported: Vec<String>,
    pub gate: Option<String>,
    pub gate_since: Option<String>,
    pub closed: bool,
    pub closed_at: Option<String>,
    pub closed_how: Option<String>,
    pub unread: Vec<String>,
    pub bad_run: Option<String>,
    #[serde(default)]
    pub settled_at: Option<String>,
    #[serde(default)]
    pub restored_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Link {
    pub fn closed_since(&self) -> Option<&str> {
        match (self.closed_at.as_deref(), self.restored_at.as_deref()) {
            (Some(c), Some(r)) if r > c => Some(r),
            (c, _) => c,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Bind {
    Kept(Link),
    Saved(Link),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub parent: String,
    pub label: String,
    pub role: String,
    pub params: LinkParams,
    pub at: String,
    pub reported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Message {
    pub handle: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub at: String,
    pub body: String,
}

impl Message {
    pub fn head(&self, max: usize) -> String {
        let flat = self.body.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.chars().take(max).collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MergeWait {
    pub repo: String,
    pub pr: u64,
    pub head: String,
    pub pull_request_id: String,
    pub method: Option<String>,
    pub thread: Option<String>,
    pub label: Option<String>,
    pub merge_state: String,
    pub at: String,
    #[serde(default)]
    pub status: WaitStatus,
    #[serde(default)]
    pub approval: String,
    #[serde(default)]
    pub failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WaitStatus {
    #[default]
    Waiting,
    Sent,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrSeen {
    pub repo: String,
    pub pr: u64,
    pub state: String,
    pub head: String,
    pub queued: bool,
    pub since: String,
    pub tickets: Vec<(String, String)>,
    pub at: String,
    #[serde(default)]
    pub left: Option<String>,
}

pub struct Wake<'a> {
    pub repo: &'a str,
    pub pr: u64,
    pub event: Option<&'a str>,
    pub ticket: &'a str,
    pub from: &'a str,
    pub kind: &'a str,
    pub body: &'a str,
    pub at: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    pub until: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LimitStop {
    pub limit: bool,
    pub seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PickRecord {
    pub pool: String,
    pub instance_id: String,
    pub model: String,
    pub strategy: String,
    pub reason: String,
    pub skipped: Value,
    pub at: String,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewIssue {
    pub url: String,
    pub repo: String,
    pub number: u64,
    pub label: String,
    pub title: Option<String>,
    pub note: Option<String>,
    pub thread: String,
    pub thread_link: Option<String>,
    pub thread_title: Option<String>,
    pub at: String,
    #[serde(default)]
    pub dismissed_at: Option<String>,
    #[serde(default)]
    pub restored_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxItem {
    pub id: i64,
    pub to: String,
    pub text: String,
    pub handle: String,
    pub queued: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct WaitingItem {
    pub id: String,
    pub text: String,
    pub from: Option<String>,
    pub link: Option<String>,
    pub at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContextSpec {
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub statuses: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BoardContext {
    pub id: String,
    pub name: String,
    pub spec: ContextSpec,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContextPut {
    Created(BoardContext),
    Updated(BoardContext),
    NameTaken(String),
    Full,
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredToken {
    pub access_token: String,
    pub client_id: String,
    pub expires_at: String,
    pub obtained_at: String,
}

pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceFolder {
    pub instance: String,
    pub driver: String,
    pub folder: String,
    pub at: String,
}

fn handle_seq(handle: &str) -> Option<i64> {
    handle.strip_prefix('m')?.parse().ok()
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        crate::platform::private_dir(dir)?;
        let path = dir.join("store.db");
        let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        crate::platform::private_file(&path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        let has_data: i64 =
            conn.query_row("SELECT COUNT(*) FROM pragma_table_info('waiting') WHERE name = 'data'", [], |r| r.get(0))?;
        if has_data == 0 {
            conn.execute_batch("ALTER TABLE waiting ADD COLUMN data TEXT")?;
        }
        Ok(Store { conn: Mutex::new(conn), path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&conn)?)
    }

    pub fn kv_get(&self, ns: &str, key: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT value FROM kv WHERE ns = ?1 AND key = ?2", params![ns, key], |r| r.get(0)).optional()
        })?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn kv_set(&self, ns: &str, key: &str, value: &Value, at: &str) -> Result<()> {
        let s = value.to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO kv (ns, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (ns, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![ns, key, s, at],
            )
        })?;
        Ok(())
    }

    pub fn kv_del(&self, ns: &str, key: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM kv WHERE ns = ?1 AND key = ?2", params![ns, key]))? > 0)
    }

    pub fn kv_list(&self, ns: &str, prefix: &str) -> Result<Vec<String>> {
        self.with(|c| {
            let mut st =
                c.prepare("SELECT key FROM kv WHERE ns = ?1 AND substr(key, 1, length(?2)) = ?2 ORDER BY key")?;
            st.query_map(params![ns, prefix], |r| r.get(0))?.collect()
        })
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional())
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
        })?;
        Ok(())
    }

    pub fn instance_folder(&self, instance: &str) -> Result<Option<InstanceFolder>> {
        self.with(|c| {
            c.query_row(
                "SELECT instance, driver, folder, at FROM instance_folders WHERE instance = ?1",
                [instance],
                |r| Ok(InstanceFolder { instance: r.get(0)?, driver: r.get(1)?, folder: r.get(2)?, at: r.get(3)? }),
            )
            .optional()
        })
    }

    pub fn instance_folders(&self) -> Result<Vec<InstanceFolder>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT instance, driver, folder, at FROM instance_folders ORDER BY instance")?;
            st.query_map([], |r| {
                Ok(InstanceFolder { instance: r.get(0)?, driver: r.get(1)?, folder: r.get(2)?, at: r.get(3)? })
            })?
            .collect()
        })
    }

    pub fn instance_folder_set(&self, f: &InstanceFolder) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO instance_folders (instance, driver, folder, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (instance) DO UPDATE SET driver = excluded.driver, folder = excluded.folder, at = excluded.at",
                params![f.instance, f.driver, f.folder, f.at],
            )
        })?;
        Ok(())
    }

    pub fn slot_resolution(&self, key: &str) -> Result<Option<(String, Value)>> {
        let row: Option<(String, String)> = self.with(|c| {
            c.query_row("SELECT fingerprint, data FROM slot_resolutions WHERE key = ?1", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
        })?;
        Ok(match row {
            Some((fp, data)) => Some((fp, serde_json::from_str(&data)?)),
            None => None,
        })
    }

    pub fn slot_resolution_set(&self, key: &str, fingerprint: &str, data: &Value, at: &str) -> Result<()> {
        let s = data.to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO slot_resolutions (key, fingerprint, data, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (key) DO UPDATE SET fingerprint = excluded.fingerprint, data = excluded.data, at = excluded.at",
                params![key, fingerprint, s, at],
            )
        })?;
        Ok(())
    }

    pub fn token_get(&self) -> Result<Option<StoredToken>> {
        self.with(|c| {
            c.query_row("SELECT access_token, client_id, expires_at, obtained_at FROM token WHERE id = 1", [], |r| {
                Ok(StoredToken {
                    access_token: r.get(0)?,
                    client_id: r.get(1)?,
                    expires_at: r.get(2)?,
                    obtained_at: r.get(3)?,
                })
            })
            .optional()
        })
    }

    pub fn token_set(&self, t: &StoredToken) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO token (id, access_token, client_id, expires_at, obtained_at) VALUES (1, ?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET access_token = excluded.access_token, client_id = excluded.client_id,
                 expires_at = excluded.expires_at, obtained_at = excluded.obtained_at",
                params![t.access_token, t.client_id, t.expires_at, t.obtained_at],
            )
        })?;
        Ok(())
    }

    pub fn token_clear(&self) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM token", []))?;
        Ok(())
    }

    pub fn node_role(&self, thread: &str) -> Result<Option<String>> {
        self.with(|c| c.query_row("SELECT role FROM nodes WHERE thread = ?1", [thread], |r| r.get(0)).optional())
    }

    pub fn node_set(&self, thread: &str, role: &str, at: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO nodes (thread, role, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (thread) DO UPDATE SET role = excluded.role, at = excluded.at",
                params![thread, role, at],
            )
        })?;
        Ok(())
    }

    fn links_where(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Link>> {
        let raws: Vec<String> = self.with(|c| {
            let mut st = c.prepare(&format!("SELECT data FROM links {sql} ORDER BY rowid"))?;
            st.query_map(p, |r| r.get(0))?.collect()
        })?;
        raws.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    pub fn link_get(&self, child: &str) -> Result<Option<Link>> {
        Ok(self.links_where("WHERE child = ?1", [child])?.pop())
    }

    pub fn links_open(&self) -> Result<Vec<Link>> {
        self.links_where("WHERE closed = 0", [])
    }

    pub fn children(&self, parent: &str, open_only: bool) -> Result<Vec<Link>> {
        if open_only {
            self.links_where("WHERE parent = ?1 AND closed = 0", [parent])
        } else {
            self.links_where("WHERE parent = ?1", [parent])
        }
    }

    pub fn link_delete(&self, child: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM links WHERE child = ?1", [child]))? > 0)
    }

    pub fn link_save(&self, ln: &Link) -> Result<()> {
        let data = serde_json::to_string(ln)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO links (child, parent, closed, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (child) DO UPDATE SET parent = excluded.parent, closed = excluded.closed, data = excluded.data",
                params![ln.child, ln.parent, ln.closed as i64, data],
            )
        })?;
        Ok(())
    }

    pub fn link_bind(
        &self,
        child: &str,
        clear_pending: bool,
        build: impl FnOnce(Option<Link>) -> Bind,
    ) -> Result<Bind> {
        self.link_bind_launched(child, clear_pending, None, |current, _| build(current))
    }

    pub fn link_bind_launched(
        &self,
        child: &str,
        clear_pending: bool,
        launched_by: Option<&str>,
        build: impl FnOnce(Option<Link>, Option<&Pending>) -> Bind,
    ) -> Result<Bind> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: Option<String> =
            tx.query_row("SELECT data FROM links WHERE child = ?1", [child], |r| r.get(0)).optional()?;
        let current = raw.map(|s| serde_json::from_str(&s)).transpose()?;
        let launch = match launched_by {
            Some(parent) => {
                let mut st = tx.prepare("SELECT data FROM pending WHERE parent = ?1 ORDER BY rowid")?;
                let raws: Vec<String> = st.query_map([parent], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                drop(st);
                raws.iter()
                    .map(|s| serde_json::from_str::<Pending>(s))
                    .collect::<serde_json::Result<Vec<_>>>()?
                    .into_iter()
                    .find(|p| p.thread.as_deref() == Some(child))
            }
            None => None,
        };
        let bind = build(current, launch.as_ref());
        if let Bind::Saved(ln) = &bind {
            tx.execute(
                "INSERT INTO links (child, parent, closed, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (child) DO UPDATE SET parent = excluded.parent, closed = excluded.closed, data = excluded.data",
                params![ln.child, ln.parent, ln.closed as i64, serde_json::to_string(ln)?],
            )?;
            tx.execute(
                "INSERT INTO nodes (thread, role, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (thread) DO UPDATE SET role = excluded.role, at = excluded.at",
                params![ln.child, ln.role, ln.last_up],
            )?;
            if clear_pending {
                tx.execute("DELETE FROM pending WHERE parent = ?1 AND label = ?2", params![ln.parent, ln.label])?;
            }
            if let Some(p) = &launch {
                tx.execute("DELETE FROM pending WHERE parent = ?1 AND label = ?2", params![p.parent, p.label])?;
            }
        }
        tx.commit()?;
        Ok(bind)
    }

    pub fn pending_put(&self, p: &Pending) -> Result<()> {
        let data = serde_json::to_string(p)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO pending (parent, label, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (parent, label) DO UPDATE SET data = excluded.data",
                params![p.parent, p.label, data],
            )
        })?;
        Ok(())
    }

    pub fn pending_remove(&self, parent: &str, label: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM pending WHERE parent = ?1 AND label = ?2", params![parent, label]))?
            > 0)
    }

    pub fn pending_all(&self) -> Result<Vec<Pending>> {
        let raws: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT data FROM pending ORDER BY rowid")?;
            st.query_map([], |r| r.get(0))?.collect()
        })?;
        raws.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    pub fn merge_wait_put(&self, w: &MergeWait) -> Result<()> {
        let data = serde_json::to_string(w)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO merge_waits (repo, pr, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (repo, pr) DO UPDATE SET data = excluded.data",
                params![w.repo, w.pr as i64, data],
            )
        })?;
        Ok(())
    }

    pub fn merge_wait_remove(&self, repo: &str, pr: u64) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM merge_waits WHERE repo = ?1 AND pr = ?2", params![repo, pr as i64]))?
            > 0)
    }

    pub fn merge_waits(&self) -> Result<Vec<MergeWait>> {
        let raws: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT data FROM merge_waits ORDER BY rowid")?;
            st.query_map([], |r| r.get(0))?.collect()
        })?;
        raws.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    pub fn merge_wait_get(&self, repo: &str, pr: u64) -> Result<Option<MergeWait>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT data FROM merge_waits WHERE repo = ?1 AND pr = ?2", params![repo, pr as i64], |r| {
                r.get(0)
            })
            .optional()
        })?;
        raw.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn merge_wait_swap(&self, w: &MergeWait) -> Result<bool> {
        let data = serde_json::to_string(w)?;
        let sql = "UPDATE merge_waits SET data = ?3 WHERE repo = ?1 AND pr = ?2 \
                   AND COALESCE(json_extract(data, '$.approval'), '') = ?4";
        Ok(self.with(|c| c.execute(sql, params![w.repo, w.pr as i64, data, w.approval]))? > 0)
    }

    pub fn merge_wait_retire(&self, w: &MergeWait) -> Result<bool> {
        let sql = "DELETE FROM merge_waits WHERE repo = ?1 AND pr = ?2 \
                   AND COALESCE(json_extract(data, '$.approval'), '') = ?3";
        Ok(self.with(|c| c.execute(sql, params![w.repo, w.pr as i64, w.approval]))? > 0)
    }

    pub fn pr_seen_put(&self, p: &PrSeen) -> Result<()> {
        let data = serde_json::to_string(p)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO pr_seen (repo, pr, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (repo, pr) DO UPDATE SET data = excluded.data",
                params![p.repo, p.pr as i64, data],
            )
        })?;
        Ok(())
    }

    pub fn pr_seen_all(&self) -> Result<Vec<PrSeen>> {
        let raws: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT data FROM pr_seen ORDER BY rowid")?;
            st.query_map([], |r| r.get(0))?.collect()
        })?;
        raws.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    pub fn pr_seen_get(&self, repo: &str, pr: u64) -> Result<Option<PrSeen>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT data FROM pr_seen WHERE repo = ?1 AND pr = ?2", params![repo, pr as i64], |r| r.get(0))
                .optional()
        })?;
        raw.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn pr_seen_any_queued(&self) -> Result<bool> {
        let sql = "SELECT EXISTS (SELECT 1 FROM pr_seen WHERE json_extract(data, '$.queued') = 1 \
                   AND json_extract(data, '$.state') = 'OPEN')";
        self.with(|c| c.query_row(sql, [], |r| r.get(0)))
    }

    pub fn pr_seen_remove(&self, repo: &str, pr: u64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM pr_seen WHERE repo = ?1 AND pr = ?2", params![repo, pr as i64]))?;
        Ok(())
    }

    pub fn pr_event_seen(&self, repo: &str, pr: u64, event: &str, ticket: &str) -> Result<bool> {
        let sql = "SELECT EXISTS (SELECT 1 FROM pr_events WHERE repo = ?1 AND pr = ?2 AND event = ?3 AND ticket = ?4)";
        self.with(|c| c.query_row(sql, params![repo, pr as i64, event, ticket], |r| r.get(0)))
    }

    pub fn pr_event_mark(&self, repo: &str, pr: u64, event: &str, ticket: &str, at: &str) -> Result<bool> {
        let sql = "INSERT OR IGNORE INTO pr_events (repo, pr, event, ticket, at) VALUES (?1, ?2, ?3, ?4, ?5)";
        Ok(self.with(|c| c.execute(sql, params![repo, pr as i64, event, ticket, at]))? > 0)
    }

    pub fn pr_event_wake(&self, w: &Wake, text: impl FnOnce(&str) -> String) -> Result<Option<String>> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(event) = w.event {
            let fresh = tx.execute(
                "INSERT OR IGNORE INTO pr_events (repo, pr, event, ticket, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![w.repo, w.pr as i64, event, w.ticket, w.at],
            )? > 0;
            if !fresh {
                return Ok(None);
            }
        }
        let handle = Self::msg_insert(&tx, w.from, w.ticket, w.kind, w.body, w.at)?;
        tx.execute(
            "INSERT INTO outbox (to_id, text, handle, queued) VALUES (?1, ?2, ?3, ?4)",
            params![w.ticket, text(&handle), handle, w.at],
        )?;
        tx.commit()?;
        Ok(Some(handle))
    }

    pub fn msg_new(&self, from: &str, to: &str, kind: &str, body: &str, at: &str) -> Result<String> {
        self.with(|c| Self::msg_insert(c, from, to, kind, body, at))
    }

    fn msg_insert(c: &Connection, from: &str, to: &str, kind: &str, body: &str, at: &str) -> rusqlite::Result<String> {
        c.execute(
            "INSERT INTO messages (from_id, to_id, kind, body, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![from, to, kind, body, at],
        )?;
        let seq = c.last_insert_rowid();
        if seq % 100 == 0 {
            c.execute("DELETE FROM messages WHERE seq <= ?1", [seq - KEEP_MESSAGES])?;
        }
        Ok(format!("m{seq}"))
    }

    fn msg_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
        Ok(Message {
            handle: format!("m{}", r.get::<_, i64>(0)?),
            from: r.get(1)?,
            to: r.get(2)?,
            kind: r.get(3)?,
            body: r.get(4)?,
            at: r.get(5)?,
        })
    }

    pub fn msg_get(&self, handle: &str) -> Result<Option<Message>> {
        let Some(seq) = handle_seq(handle) else { return Ok(None) };
        self.with(|c| {
            c.query_row("SELECT seq, from_id, to_id, kind, body, at FROM messages WHERE seq = ?1", [seq], Self::msg_row)
                .optional()
        })
    }

    pub fn msg_last_from(&self, from: &str) -> Result<Option<Message>> {
        self.with(|c| {
            c.query_row(
                "SELECT seq, from_id, to_id, kind, body, at FROM messages WHERE from_id = ?1 ORDER BY seq DESC LIMIT 1",
                [from],
                Self::msg_row,
            )
            .optional()
        })
    }

    pub fn msgs_recent(&self, kinds: &[&str], to: &[String], limit: usize) -> Result<Vec<Message>> {
        if kinds.is_empty() || to.is_empty() {
            return Ok(Vec::new());
        }
        let marks = |n: usize, from: usize| (from..from + n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT seq, from_id, to_id, kind, body, at FROM messages WHERE kind IN ({}) AND to_id IN ({}) \
             ORDER BY seq DESC LIMIT {limit}",
            marks(kinds.len(), 1),
            marks(to.len(), kinds.len() + 1)
        );
        let args: Vec<&str> = kinds.iter().copied().chain(to.iter().map(String::as_str)).collect();
        self.with(|c| {
            let mut st = c.prepare(&sql)?;
            st.query_map(rusqlite::params_from_iter(args), Self::msg_row)?.collect()
        })
    }

    pub fn outbox_push(&self, to: &str, text: &str, handle: &str, queued: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO outbox (to_id, text, handle, queued) VALUES (?1, ?2, ?3, ?4)",
                params![to, text, handle, queued],
            )
        })?;
        Ok(())
    }

    pub fn outbox_all(&self) -> Result<Vec<OutboxItem>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id, to_id, text, handle, queued FROM outbox ORDER BY id")?;
            st.query_map([], |r| {
                Ok(OutboxItem { id: r.get(0)?, to: r.get(1)?, text: r.get(2)?, handle: r.get(3)?, queued: r.get(4)? })
            })?
            .collect()
        })
    }

    pub fn outbox_drop_to(&self, to: &str) -> Result<usize> {
        self.with(|c| c.execute("DELETE FROM outbox WHERE to_id = ?1", [to]))
    }

    pub fn outbox_delete(&self, ids: &[i64]) -> Result<()> {
        self.with(|c| {
            for id in ids {
                c.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
            }
            Ok(())
        })
    }

    pub fn outbox_retarget(&self, from: &str, to: &str) -> Result<usize> {
        self.with(|c| c.execute("UPDATE outbox SET to_id = ?2 WHERE to_id = ?1", params![from, to]))
    }

    pub fn pending_retarget(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let moved: Vec<Pending> = self.pending_all()?.into_iter().filter(|p| p.parent == from).collect();
        for p in &moved {
            self.pending_remove(&p.parent, &p.label)?;
            self.pending_put(&Pending { parent: to.to_string(), ..p.clone() })?;
        }
        Ok(moved.into_iter().map(|p| p.label).collect())
    }

    pub fn waiting_open(&self, owner: &str) -> Result<Vec<WaitingItem>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, text, from_label, link, opened_at FROM waiting
                 WHERE owner = ?1 AND resolved_at IS NULL ORDER BY seq",
            )?;
            st.query_map([owner], |r| {
                Ok(WaitingItem { id: r.get(0)?, text: r.get(1)?, from: r.get(2)?, link: r.get(3)?, at: r.get(4)? })
            })?
            .collect()
        })
    }

    pub fn waiting_open_data(&self, owner: &str) -> Result<Vec<(WaitingItem, Option<Value>)>> {
        Ok(self.waiting_data(owner, false)?.into_iter().map(|(w, d, _, _)| (w, d)).collect())
    }

    pub fn waiting_data(&self, owner: &str, all: bool) -> Result<Vec<WaitingRow>> {
        type Row = (WaitingItem, Option<String>, Option<String>, Option<String>, i64);
        let rows: Vec<Row> = self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, text, from_label, link, opened_at, data, resolved_at, note, seq FROM waiting
                 WHERE owner = ?1 AND (?2 OR resolved_at IS NULL) ORDER BY seq",
            )?;
            st.query_map(params![owner, all], |r| {
                Ok((
                    WaitingItem { id: r.get(0)?, text: r.get(1)?, from: r.get(2)?, link: r.get(3)?, at: r.get(4)? },
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect()
        })?;
        Ok(rows
            .into_iter()
            .map(|(w, d, at, note, seq)| (w, d.and_then(|s| serde_json::from_str(&s).ok()), at.map(|a| (a, note)), seq))
            .collect())
    }

    pub fn waiting_insert(&self, owner: &str, item: &WaitingItem, data: Option<&Value>) -> Result<()> {
        let data = data.map(Value::to_string);
        self.with(|c| {
            c.execute(
                "INSERT INTO waiting (owner, id, text, from_label, link, opened_at, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![owner, item.id, item.text, item.from, item.link, item.at, data],
            )
        })?;
        Ok(())
    }

    pub fn waiting_set_data(&self, owner: &str, seq: i64, data: &Value) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE waiting SET data = ?3 WHERE owner = ?1 AND seq = ?2",
                params![owner, seq, data.to_string()],
            )
        })?;
        Ok(())
    }

    pub fn waiting_resolve_seq(&self, owner: &str, seq: i64, note: Option<&str>, at: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE waiting SET resolved_at = ?3, note = ?4 WHERE owner = ?1 AND seq = ?2 AND resolved_at IS NULL",
                params![owner, seq, at, note],
            )
        })
        .map(|n| n > 0)
    }

    pub fn waiting_resolve(&self, owner: &str, id: &str, note: Option<&str>, at: &str) -> Result<Option<String>> {
        self.with(|c| {
            let text: Option<String> = c
                .query_row(
                    "SELECT text FROM waiting WHERE owner = ?1 AND id = ?2 AND resolved_at IS NULL",
                    params![owner, id],
                    |r| r.get(0),
                )
                .optional()?;
            if text.is_some() {
                c.execute(
                    "UPDATE waiting SET resolved_at = ?3, note = ?4 WHERE owner = ?1 AND id = ?2 AND resolved_at IS NULL",
                    params![owner, id, at, note],
                )?;
            }
            Ok(text)
        })
    }

    pub fn ask_insert(&self, thread: &str, data: &Value) -> Result<i64> {
        self.with(|c| {
            c.execute("INSERT INTO asks (thread, open, data) VALUES (?1, 1, ?2)", params![thread, data.to_string()])?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn ask_update(&self, seq: i64, open: bool, data: &Value) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE asks SET open = ?2, data = ?3 WHERE seq = ?1",
                params![seq, open as i64, data.to_string()],
            )
        })?;
        Ok(())
    }

    fn asks_where(&self, sql: &str) -> Result<Vec<(i64, Value)>> {
        let rows: Vec<(i64, String)> = self.with(|c| {
            let mut st = c.prepare(&format!("SELECT seq, data FROM asks {sql}"))?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })?;
        rows.into_iter().map(|(seq, d)| Ok((seq, serde_json::from_str(&d)?))).collect()
    }

    pub fn ask_get(&self, seq: i64) -> Result<Option<(bool, Value)>> {
        let row: Option<(bool, String)> = self.with(|c| {
            c.query_row("SELECT open, data FROM asks WHERE seq = ?1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).optional()
        })?;
        row.map(|(open, d)| Ok((open, serde_json::from_str(&d)?))).transpose()
    }

    pub fn ask_thread(&self, thread: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row("SELECT EXISTS (SELECT 1 FROM asks WHERE thread = ?1)", params![thread], |r| r.get(0))
        })
    }

    pub fn asks_open(&self) -> Result<Vec<(i64, Value)>> {
        self.asks_where("WHERE open = 1 ORDER BY seq")
    }

    pub fn asks_closed_unsettled(&self) -> Result<Vec<(i64, Value)>> {
        self.asks_where(&format!("WHERE open = 0 AND {UNSETTLED} ORDER BY seq"))
    }

    pub fn asks_unsettled(&self, limit: usize) -> Result<Vec<(i64, Value)>> {
        self.asks_where(&format!(
            "WHERE {UNSETTLED} ORDER BY COALESCE(json_extract(data, '$.restoredAt'), json_extract(data, '$.at')) DESC, \
             seq DESC LIMIT {limit}"
        ))
    }

    pub fn asks_settled(&self, limit: usize) -> Result<Vec<(i64, Value)>> {
        self.asks_where(&format!(
            "WHERE NOT {UNSETTLED} ORDER BY json_extract(data, '$.settledAt') DESC, seq DESC LIMIT {limit}"
        ))
    }

    pub fn asks_settled_count(&self) -> Result<usize> {
        let n: i64 =
            self.with(|c| c.query_row(&format!("SELECT COUNT(*) FROM asks WHERE NOT {UNSETTLED}"), [], |r| r.get(0)))?;
        Ok(n as usize)
    }

    fn contexts_in(c: &Connection) -> rusqlite::Result<Vec<BoardContext>> {
        let mut st = c.prepare("SELECT data FROM contexts")?;
        let raws: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut all: Vec<BoardContext> = raws.iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
        all.sort_by(|a, b| crate::contexts::name_order(&a.name, &b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(all)
    }

    pub fn contexts_all(&self) -> Result<Vec<BoardContext>> {
        self.with(Self::contexts_in)
    }

    pub fn context_put(
        &self,
        id: &str,
        name: Option<&str>,
        spec: Option<&ContextSpec>,
        at: &str,
        max: usize,
    ) -> Result<ContextPut> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let all = Self::contexts_in(&tx)?;
        let old = all.iter().find(|c| c.id == id);
        let (name, spec) = match (old, name, spec) {
            (_, Some(n), Some(s)) => (n.to_string(), s.clone()),
            (Some(o), n, s) => (n.unwrap_or(&o.name).to_string(), s.unwrap_or(&o.spec).clone()),
            (None, _, _) => return Ok(ContextPut::Missing),
        };
        if let Some(other) = all.iter().find(|c| c.id != id && crate::contexts::same_name(&c.name, &name)) {
            return Ok(ContextPut::NameTaken(other.name.clone()));
        }
        if old.is_none() && all.len() >= max {
            return Ok(ContextPut::Full);
        }
        let saved = BoardContext {
            id: id.to_string(),
            name,
            spec,
            created_at: old.map_or(at, |o| &o.created_at).to_string(),
            updated_at: at.to_string(),
        };
        tx.execute(
            "INSERT INTO contexts (id, data) VALUES (?1, ?2) ON CONFLICT (id) DO UPDATE SET data = excluded.data",
            params![saved.id, serde_json::to_string(&saved)?],
        )?;
        tx.commit()?;
        Ok(if old.is_some() { ContextPut::Updated(saved) } else { ContextPut::Created(saved) })
    }

    pub fn context_delete(&self, id: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM contexts WHERE id = ?1", [id]))? > 0)
    }

    pub fn limit_set(&self, instance: &str, limit: &Limit) -> Result<()> {
        let s = serde_json::to_string(limit)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO instance_limits (instance, data) VALUES (?1, ?2)
                 ON CONFLICT (instance) DO UPDATE SET data = excluded.data",
                params![instance, s],
            )
        })?;
        Ok(())
    }

    pub fn limit_clear(&self, instance: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM instance_limits WHERE instance = ?1", [instance]))? > 0)
    }

    pub fn limits(&self) -> Result<BTreeMap<String, Limit>> {
        let rows: Vec<(String, String)> = self.with(|c| {
            let mut st = c.prepare("SELECT instance, data FROM instance_limits ORDER BY instance")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })?;
        rows.into_iter().map(|(k, v)| Ok((k, serde_json::from_str(&v)?))).collect()
    }

    pub fn limit_stop(&self, thread: &str, run: &str) -> Result<Option<LimitStop>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT data FROM limit_stops WHERE thread = ?1 AND run = ?2", [thread, run], |r| r.get(0))
                .optional()
        })?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn limit_stop_put(&self, thread: &str, run: &str, stop: &LimitStop) -> Result<()> {
        let s = serde_json::to_string(stop)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO limit_stops (thread, run, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (thread, run) DO UPDATE SET data = excluded.data",
                params![thread, run, s],
            )
        })?;
        Ok(())
    }

    pub fn pool_credits(&self, pool: &str) -> Result<BTreeMap<String, i64>> {
        let rows: Vec<(String, i64)> = self.with(|c| {
            let mut st = c.prepare("SELECT instance, credit FROM pool_members WHERE pool = ?1")?;
            st.query_map([pool], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })?;
        Ok(rows.into_iter().collect())
    }

    pub fn pool_credits_put(&self, pool: &str, credits: &BTreeMap<String, i64>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        for (instance, credit) in credits {
            tx.execute(
                "INSERT INTO pool_members (pool, instance, credit) VALUES (?1, ?2, ?3)
                 ON CONFLICT (pool, instance) DO UPDATE SET credit = excluded.credit",
                params![pool, instance, credit],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn pick_add(&self, p: &PickRecord) -> Result<i64> {
        let s = serde_json::to_string(p)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO pool_picks (pool, instance, at, handle, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![p.pool, p.instance_id, p.at, p.handle, s],
            )?;
            let seq = c.last_insert_rowid();
            if seq % 100 == 0 {
                c.execute("DELETE FROM pool_picks WHERE seq <= ?1", [seq - KEEP_PICKS])?;
            }
            Ok(seq)
        })
    }

    pub fn pick_launched(&self, seq: i64, handle: Option<&str>, thread: &str) -> Result<()> {
        self.pick_bind(seq, handle, thread, None)
    }

    pub fn pick_bind(&self, seq: i64, handle: Option<&str>, thread: &str, moved_from: Option<&str>) -> Result<()> {
        let Some(mut p) = self.pick_get(seq)? else { return Ok(()) };
        p.handle = handle.map(String::from);
        p.thread = Some(thread.to_string());
        p.moved_from = moved_from.map(String::from);
        let s = serde_json::to_string(&p)?;
        self.with(|c| {
            c.execute("UPDATE pool_picks SET handle = ?2, data = ?3 WHERE seq = ?1", params![seq, handle, s])
        })?;
        Ok(())
    }

    pub fn pick_get(&self, seq: i64) -> Result<Option<PickRecord>> {
        let raw: Option<String> =
            self.with(|c| c.query_row("SELECT data FROM pool_picks WHERE seq = ?1", [seq], |r| r.get(0)).optional())?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn pick_by_thread(&self, thread: &str) -> Result<Option<PickRecord>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row(
                "SELECT data FROM pool_picks WHERE json_extract(data, '$.thread') = ?1 ORDER BY seq DESC LIMIT 1",
                [thread],
                |r| r.get(0),
            )
            .optional()
        })?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn picks(&self, pool: &str, limit: usize) -> Result<Vec<PickRecord>> {
        let raws: Vec<String> = self.with(|c| {
            let mut st = c.prepare("SELECT data FROM pool_picks WHERE pool = ?1 ORDER BY seq DESC LIMIT ?2")?;
            st.query_map(params![pool, limit as i64], |r| r.get(0))?.collect()
        })?;
        raws.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }

    pub fn pick_by_handle(&self, handle: &str) -> Result<Option<PickRecord>> {
        let raw: Option<String> = self.with(|c| {
            c.query_row("SELECT data FROM pool_picks WHERE handle = ?1", [handle], |r| r.get(0)).optional()
        })?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn pick_stats(&self, pool: &str) -> Result<BTreeMap<String, (u64, Option<String>)>> {
        let rows: Vec<(String, i64, Option<String>)> = self.with(|c| {
            let mut st =
                c.prepare("SELECT instance, COUNT(*), MAX(at) FROM pool_picks WHERE pool = ?1 GROUP BY instance")?;
            st.query_map([pool], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()
        })?;
        Ok(rows.into_iter().map(|(i, n, at)| (i, (n.max(0) as u64, at))).collect())
    }

    pub fn new_issue_add(&self, key: &str, issue: &NewIssue) -> Result<Option<i64>> {
        let s = serde_json::to_string(issue)?;
        self.with(|c| {
            let n = c.execute("INSERT OR IGNORE INTO new_issues (key, data) VALUES (?1, ?2)", params![key, s])?;
            Ok((n > 0).then(|| c.last_insert_rowid()))
        })
    }

    fn new_issue_where<P: rusqlite::Params>(&self, sql: &str, p: P) -> Result<Vec<(i64, NewIssue)>> {
        let rows: Vec<(i64, String)> = self.with(|c| {
            let mut st = c.prepare(&format!("SELECT seq, data FROM new_issues {sql}"))?;
            st.query_map(p, |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })?;
        rows.into_iter().map(|(seq, d)| Ok((seq, serde_json::from_str(&d)?))).collect()
    }

    pub fn new_issue_by_key(&self, key: &str) -> Result<Option<(i64, NewIssue)>> {
        Ok(self.new_issue_where("WHERE key = ?1", [key])?.into_iter().next())
    }

    pub fn new_issue_get(&self, seq: i64) -> Result<Option<NewIssue>> {
        Ok(self.new_issue_where("WHERE seq = ?1", [seq])?.into_iter().next().map(|(_, i)| i))
    }

    pub fn new_issues(&self) -> Result<Vec<(i64, NewIssue)>> {
        self.new_issue_where("ORDER BY seq", [])
    }

    pub fn new_issue_update(&self, seq: i64, issue: &NewIssue) -> Result<()> {
        let s = serde_json::to_string(issue)?;
        self.with(|c| c.execute("UPDATE new_issues SET data = ?2 WHERE seq = ?1", params![seq, s]))?;
        Ok(())
    }

    pub fn new_issue_remove(&self, key: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM new_issues WHERE key = ?1", [key]))? > 0)
    }
}
