use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{
    model::{Client, EndReason, Policy, Presence, QueryLog, Scope, Segment},
    paths::ensure_parent,
};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        ensure_parent(path)?;
        let conn = Connection::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=FULL;
            PRAGMA busy_timeout=5000;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS policy (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                idle_threshold_secs INTEGER NOT NULL,
                locked_is_offline INTEGER NOT NULL,
                online_confirm_secs INTEGER NOT NULL,
                sample_interval_secs INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS presence_segments (
                id INTEGER PRIMARY KEY,
                state TEXT NOT NULL CHECK (state IN ('online', 'offline')),
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                end_reason TEXT,
                last_seen_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mcp_clients (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                scopes TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS mcp_queries (
                id INTEGER PRIMARY KEY,
                client_id TEXT NOT NULL,
                tool TEXT NOT NULL,
                allowed INTEGER NOT NULL,
                args_summary TEXT NOT NULL,
                result_summary TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_segments_started ON presence_segments(started_at);
            CREATE INDEX IF NOT EXISTS idx_queries_created ON mcp_queries(created_at DESC);
            ",
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.ensure_policy()?;
        Ok(db)
    }

    pub fn policy(&self) -> anyhow::Result<Policy> {
        let conn = self.lock();
        conn.query_row(
            "SELECT idle_threshold_secs, locked_is_offline, online_confirm_secs, sample_interval_secs
             FROM policy WHERE id = 1",
            [],
            |row| {
                Ok(Policy {
                    idle_threshold_secs: row.get::<_, i64>(0)? as u64,
                    locked_is_offline: row.get::<_, i64>(1)? != 0,
                    online_confirm_secs: row.get::<_, i64>(2)? as u64,
                    sample_interval_secs: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .context("读取策略失败")
    }

    pub fn save_knobs(
        &self,
        idle_threshold_secs: u64,
        locked_is_offline: bool,
        online_confirm_secs: u64,
    ) -> anyhow::Result<Policy> {
        Policy::validate_knobs(idle_threshold_secs, online_confirm_secs)
            .map_err(|err| anyhow::anyhow!(err))?;
        let conn = self.lock();
        conn.execute(
            "UPDATE policy
             SET idle_threshold_secs = ?1, locked_is_offline = ?2, online_confirm_secs = ?3
             WHERE id = 1",
            params![
                idle_threshold_secs as i64,
                i64::from(locked_is_offline),
                online_confirm_secs as i64
            ],
        )?;
        drop(conn);
        self.policy()
    }

    pub fn current_segment(&self) -> anyhow::Result<Option<Segment>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, state, started_at, ended_at, end_reason, last_seen_at
             FROM presence_segments WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
            [],
            map_segment,
        )
        .optional()
        .context("读取当前时段失败")
    }

    pub fn close_segment(&self, id: i64, ended_at: i64, reason: EndReason) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE presence_segments
             SET ended_at = ?1, end_reason = ?2, last_seen_at = ?1
             WHERE id = ?3 AND ended_at IS NULL",
            params![ended_at, reason.as_str(), id],
        )?;
        Ok(())
    }

    pub fn insert_segment(&self, state: Presence, at_ms: i64) -> anyhow::Result<Segment> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO presence_segments (state, started_at, ended_at, end_reason, last_seen_at)
             VALUES (?1, ?2, NULL, NULL, ?2)",
            params![state.as_str(), at_ms],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Segment {
            id,
            state,
            started_at: at_ms,
            ended_at: None,
            end_reason: None,
            last_seen_at: at_ms,
        })
    }

    pub fn transition(
        &self,
        new_state: Presence,
        at_ms: i64,
        reason: EndReason,
    ) -> anyhow::Result<Segment> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE presence_segments
             SET ended_at = ?1, end_reason = ?2, last_seen_at = ?1
             WHERE ended_at IS NULL",
            params![at_ms, reason.as_str()],
        )?;
        tx.execute(
            "INSERT INTO presence_segments (state, started_at, ended_at, end_reason, last_seen_at)
             VALUES (?1, ?2, NULL, NULL, ?2)",
            params![new_state.as_str(), at_ms],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Segment {
            id,
            state: new_state,
            started_at: at_ms,
            ended_at: None,
            end_reason: None,
            last_seen_at: at_ms,
        })
    }

    pub fn touch_segment(&self, id: i64, at_ms: i64) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE presence_segments SET last_seen_at = ?1 WHERE id = ?2 AND ended_at IS NULL",
            params![at_ms, id],
        )?;
        Ok(())
    }

    pub fn segments_between(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<Segment>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, state, started_at, ended_at, end_reason, last_seen_at
             FROM presence_segments
             WHERE started_at < ?2 AND (ended_at IS NULL OR ended_at > ?1)
             ORDER BY started_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![from_ms, to_ms], map_segment)?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("读取时段失败")
    }

    pub fn create_client(
        &self,
        id: &str,
        name: &str,
        token_hash: &str,
        scopes: &[Scope],
        created_at: i64,
    ) -> anyhow::Result<Client> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mcp_clients (id, name, token_hash, scopes, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![id, name, token_hash, encode_scopes(scopes), created_at],
        )?;
        Ok(Client {
            id: id.to_string(),
            name: name.to_string(),
            scopes: scopes.to_vec(),
            created_at,
            revoked_at: None,
        })
    }

    pub fn list_clients(&self) -> anyhow::Result<Vec<Client>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, scopes, created_at, revoked_at
             FROM mcp_clients ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], map_client)?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("读取客户端失败")
    }

    pub fn update_client(
        &self,
        id: &str,
        name: Option<&str>,
        scopes: Option<&[Scope]>,
    ) -> anyhow::Result<Option<Client>> {
        let conn = self.lock();
        let existing = conn
            .query_row(
                "SELECT id, name, scopes, created_at, revoked_at FROM mcp_clients WHERE id = ?1",
                params![id],
                map_client,
            )
            .optional()?;
        let Some(mut client) = existing else {
            return Ok(None);
        };
        if client.revoked_at.is_some() {
            anyhow::bail!("客户端已撤销");
        }
        if let Some(name) = name {
            client.name = name.to_string();
        }
        if let Some(scopes) = scopes {
            client.scopes = scopes.to_vec();
        }
        conn.execute(
            "UPDATE mcp_clients SET name = ?1, scopes = ?2 WHERE id = ?3",
            params![client.name, encode_scopes(&client.scopes), id],
        )?;
        Ok(Some(client))
    }

    pub fn revoke_client(&self, id: &str, at_ms: i64) -> anyhow::Result<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE mcp_clients SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
            params![at_ms, id],
        )?;
        Ok(changed > 0)
    }

    pub fn client_by_token_hash(&self, token_hash: &str) -> anyhow::Result<Option<Client>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, name, scopes, created_at, revoked_at
             FROM mcp_clients WHERE token_hash = ?1",
            params![token_hash],
            map_client,
        )
        .optional()
        .context("查找客户端失败")
    }

    pub fn insert_query(
        &self,
        client_id: &str,
        tool: &str,
        allowed: bool,
        args_summary: &str,
        result_summary: &str,
        created_at: i64,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mcp_queries
             (client_id, tool, allowed, args_summary, result_summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                client_id,
                tool,
                i64::from(allowed),
                args_summary,
                result_summary,
                created_at
            ],
        )?;
        Ok(())
    }

    pub fn list_queries(
        &self,
        client_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<QueryLog>> {
        let conn = self.lock();
        let limit = i64::from(limit.clamp(1, 500));
        let mut stmt = conn.prepare(
            "SELECT q.id, q.client_id, COALESCE(c.name, ''), q.tool, q.allowed,
                    q.args_summary, q.result_summary, q.created_at
             FROM mcp_queries q
             LEFT JOIN mcp_clients c ON c.id = q.client_id
             WHERE (?1 IS NULL OR q.client_id = ?1)
             ORDER BY q.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![client_id, limit], |row| {
            Ok(QueryLog {
                id: row.get(0)?,
                client_id: row.get(1)?,
                client_name: row.get(2)?,
                tool: row.get(3)?,
                allowed: row.get::<_, i64>(4)? != 0,
                args_summary: row.get(5)?,
                result_summary: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("读取查询记录失败")
    }

    fn ensure_policy(&self) -> anyhow::Result<()> {
        let defaults = Policy::default();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO policy
             (id, idle_threshold_secs, locked_is_offline, online_confirm_secs, sample_interval_secs)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO NOTHING",
            params![
                defaults.idle_threshold_secs as i64,
                i64::from(defaults.locked_is_offline),
                defaults.online_confirm_secs as i64,
                defaults.sample_interval_secs as i64
            ],
        )?;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|err| err.into_inner())
    }
}

fn map_segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    let state: String = row.get(1)?;
    let reason: Option<String> = row.get(4)?;
    Ok(Segment {
        id: row.get(0)?,
        state: Presence::parse(&state).unwrap_or(Presence::Offline),
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        end_reason: reason.as_deref().and_then(EndReason::parse),
        last_seen_at: row.get(5)?,
    })
}

fn map_client(row: &rusqlite::Row<'_>) -> rusqlite::Result<Client> {
    let scopes: String = row.get(2)?;
    Ok(Client {
        id: row.get(0)?,
        name: row.get(1)?,
        scopes: decode_scopes(&scopes),
        created_at: row.get(3)?,
        revoked_at: row.get(4)?,
    })
}

fn encode_scopes(scopes: &[Scope]) -> String {
    let names: Vec<&str> = scopes.iter().copied().map(Scope::as_str).collect();
    json!(names).to_string()
}

fn decode_scopes(raw: &str) -> Vec<Scope> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str())
        .filter_map(|name| Scope::parse_list(&[name.to_string()]).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EndReason, Presence, Scope};

    fn temp_db() -> (Db, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "awake-checker-test-{}-{}-{}.db",
            std::process::id(),
            n,
            crate::model::now_ms()
        ));
        let db = Db::open(&path).unwrap();
        (db, path)
    }

    #[test]
    fn segments_and_queries_survive_reopen() {
        let (db, path) = temp_db();
        db.insert_segment(Presence::Online, 1_000).unwrap();
        db.transition(Presence::Offline, 5_000, EndReason::Idle)
            .unwrap();
        db.create_client("c1", "agent", "hash", &[Scope::Summary], 1_000)
            .unwrap();
        db.insert_query("c1", "summarize_presence", false, "{}", "denied: history", 2_000)
            .unwrap();
        drop(db);

        let db = Db::open(&path).unwrap();
        let segments = db.segments_between(0, 9_000).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].state, Presence::Online);
        assert_eq!(segments[0].end_reason, Some(EndReason::Idle));
        assert!(segments[1].ended_at.is_none());
        let queries = db.list_queries(Some("c1"), 10).unwrap();
        assert_eq!(queries.len(), 1);
        assert!(!queries[0].allowed);
        assert_eq!(queries[0].client_name, "agent");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn revoked_token_stays_revoked() {
        let (db, path) = temp_db();
        db.create_client("c1", "agent", "hash2", &[Scope::Status], 1)
            .unwrap();
        assert!(db.revoke_client("c1", 50).unwrap());
        let client = db.client_by_token_hash("hash2").unwrap().unwrap();
        assert_eq!(client.revoked_at, Some(50));
        let _ = std::fs::remove_file(path);
    }
}
