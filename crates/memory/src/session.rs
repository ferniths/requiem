//! Session persistence via SQLite.
//!
//! A `Session` wraps a `rusqlite::Connection` and provides typed
//! insert/query operations for session records. The schema is created
//! on open (idempotent `CREATE TABLE IF NOT EXISTS`).

use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A record of one completed (or in-progress) agent task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The session UUID.
    pub session_id: Uuid,
    /// The task description.
    pub task_description: String,
    /// The agent role that executed the task.
    pub role: String,
    /// JSON-encoded final response.
    pub final_response_json: Option<String>,
    /// Tokens consumed.
    pub tokens_input: u32,
    pub tokens_output: u32,
    /// Whether the task completed successfully.
    pub success: bool,
    /// ISO-8601 timestamp of when the record was written.
    pub timestamp_rfc3339: String,
}

/// A persistent SQLite session store.
pub struct Session {
    conn: Connection,
}

impl Session {
    /// Open (or create) a session database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let conn = Connection::open(path).map_err(|e| MemoryError::Db(e.to_string()))?;
        // WAL mode: crash-safe concurrent reads, atomic single-writer
        // commits. A torn write in the main DB file can never corrupt
        // committed data; the WAL file recovers on the next open.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        // NORMAL is the recommended sync level for WAL: every commit is
        // still durable, but fsync is only on checkpoint, not every
        // transaction. Safe on modern file systems.
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        // Create the schema idempotently.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS session_records (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id      TEXT NOT NULL,
                task_description TEXT NOT NULL,
                role            TEXT NOT NULL,
                final_response_json TEXT,
                tokens_input    INTEGER NOT NULL DEFAULT 0,
                tokens_output   INTEGER NOT NULL DEFAULT 0,
                success         INTEGER NOT NULL DEFAULT 0,
                timestamp_rfc3339 TEXT NOT NULL,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            )",
            [],
        )
        .map_err(|e| MemoryError::Db(e.to_string()))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_id ON session_records (session_id)",
            [],
        )
        .map_err(|e| MemoryError::Db(e.to_string()))?;

        // Task-graph checkpoints: one JSON blob per session, overwritten on
        // each checkpoint. Used to resume in-flight multi-agent state after
        // a crash.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_checkpoints (
                session_id      TEXT PRIMARY KEY,
                graph_json      TEXT NOT NULL,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            )",
            [],
        )
        .map_err(|e| MemoryError::Db(e.to_string()))?;

        Ok(Self { conn })
    }

    /// Persist the current task graph as a checkpoint for the session.
    ///
    /// Overwrites any prior checkpoint for this session. The graph is
    /// serialized to JSON so the checkpoint is independent of the Rust
    /// type layout (forward-compatible).
    pub fn save_task_graph(
        &self,
        session_id: &Uuid,
        graph: &requiem_core::task::TaskGraph,
    ) -> Result<(), MemoryError> {
        let json = serde_json::to_string(graph).map_err(|e| MemoryError::Db(e.to_string()))?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO task_checkpoints (session_id, graph_json)
                 VALUES (?1, ?2)",
                params![session_id.to_string(), json],
            )
            .map(|_| ())
            .map_err(|e| MemoryError::Db(e.to_string()))
    }

    /// Load the most recent task-graph checkpoint for a session.
    ///
    /// Returns `Ok(None)` if no checkpoint exists yet.
    pub fn load_task_graph(
        &self,
        session_id: &Uuid,
    ) -> Result<Option<requiem_core::task::TaskGraph>, MemoryError> {
        let json: Result<String, rusqlite::Error> = self.conn.query_row(
            "SELECT graph_json FROM task_checkpoints WHERE session_id = ?1",
            [session_id.to_string()],
            |row| row.get(0),
        );

        match json {
            Ok(s) => {
                let graph: requiem_core::task::TaskGraph =
                    serde_json::from_str(&s).map_err(|e| MemoryError::Db(e.to_string()))?;
                Ok(Some(graph))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(MemoryError::Db(e.to_string())),
        }
    }

    /// Insert a completed task record.
    pub fn insert_record(&self, rec: &SessionRecord) -> Result<(), MemoryError> {
        self.conn
            .execute(
                "INSERT INTO session_records (
                    session_id, task_description, role, final_response_json,
                    tokens_input, tokens_output, success, timestamp_rfc3339
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    rec.session_id.to_string(),
                    rec.task_description,
                    rec.role,
                    rec.final_response_json,
                    rec.tokens_input,
                    rec.tokens_output,
                    rec.success as i32,
                    rec.timestamp_rfc3339,
                ],
            )
            .map(|_| ())
            .map_err(|e| MemoryError::Db(e.to_string()))
    }

    /// Query all records for a given session.
    pub fn records_for_session(
        &self,
        session_id: &Uuid,
    ) -> Result<Vec<SessionRecord>, MemoryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, task_description, role, final_response_json,
                        tokens_input, tokens_output, success, timestamp_rfc3339
                 FROM session_records WHERE session_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|e| MemoryError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([session_id.to_string()], |row| {
                let sid: String = row.get(0)?;
                Ok(SessionRecord {
                    session_id: Uuid::parse_str(&sid).unwrap_or(Uuid::new_v4()),
                    task_description: row.get(1)?,
                    role: row.get(2)?,
                    final_response_json: row.get(3)?,
                    tokens_input: row.get(4)?,
                    tokens_output: row.get(5)?,
                    success: row.get::<_, i32>(6)? != 0,
                    timestamp_rfc3339: row.get(7)?,
                })
            })
            .map_err(|e| MemoryError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| MemoryError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Total token usage across all records in a session.
    pub fn session_token_totals(&self, session_id: &Uuid) -> Result<(u32, u32), MemoryError> {
        let (in_sum, out_sum): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(tokens_input), 0), COALESCE(SUM(tokens_output), 0)
                 FROM session_records WHERE session_id = ?1",
                [session_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        Ok((in_sum as u32, out_sum as u32))
    }

    /// All sessions with at least one record, newest activity first.
    ///
    /// Returns `(session_id, record_count, last_activity_rfc3339)`.
    pub fn list_sessions(&self) -> Result<Vec<(Uuid, usize, String)>, MemoryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, COUNT(*), MAX(created_at)
                 FROM session_records
                 GROUP BY session_id
                 ORDER BY MAX(created_at) DESC
                 LIMIT 50",
            )
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let sid: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                let last: String = row.get(2)?;
                Ok((
                    Uuid::parse_str(&sid).unwrap_or_else(|_| Uuid::nil()),
                    count as usize,
                    last,
                ))
            })
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| MemoryError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Delete all records for a session.
    pub fn delete_session(&self, session_id: &Uuid) -> Result<u64, MemoryError> {
        let n = self
            .conn
            .execute(
                "DELETE FROM session_records WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(|e| MemoryError::Db(e.to_string()))?;
        Ok(n as u64)
    }
}

/// An error from a memory operation.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("database error: {0}")]
    Db(String),
}

/// A shared, thread-safe session handle.
pub type SharedSession = Arc<Session>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).unwrap();
        let sid = Uuid::new_v4();

        let rec = SessionRecord {
            session_id: sid,
            task_description: "Implement feature X".to_string(),
            role: "implementer".to_string(),
            final_response_json: Some(r#"{"ok":true}"#.to_string()),
            tokens_input: 100,
            tokens_output: 50,
            success: true,
            timestamp_rfc3339: "2026-11-16T00:00:00Z".to_string(),
        };
        session.insert_record(&rec).unwrap();

        let records = session.records_for_session(&sid).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_description, "Implement feature X");
        assert!(records[0].success);
    }

    #[test]
    fn token_totals_aggregate() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("totals.db");
        let session = Session::open(&db_path).unwrap();
        let sid = Uuid::new_v4();

        for i in 0..3 {
            session
                .insert_record(&SessionRecord {
                    session_id: sid,
                    task_description: format!("task {i}"),
                    role: "tester".into(),
                    final_response_json: None,
                    tokens_input: 10,
                    tokens_output: 5,
                    success: i != 2,
                    timestamp_rfc3339: "2026-11-16T00:00:00Z".into(),
                })
                .unwrap();
        }

        let (in_sum, out_sum) = session.session_token_totals(&sid).unwrap();
        assert_eq!(in_sum, 30);
        assert_eq!(out_sum, 15);
    }

    #[test]
    fn delete_session_removes_records() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("del.db");
        let session = Session::open(&db_path).unwrap();
        let sid = Uuid::new_v4();

        session
            .insert_record(&SessionRecord {
                session_id: sid,
                task_description: "x".into(),
                role: "y".into(),
                final_response_json: None,
                tokens_input: 1,
                tokens_output: 1,
                success: false,
                timestamp_rfc3339: "2026-11-16T00:00:00Z".into(),
            })
            .unwrap();

        let deleted = session.delete_session(&sid).unwrap();
        assert_eq!(deleted, 1);
        assert!(session.records_for_session(&sid).unwrap().is_empty());
    }

    #[test]
    fn task_graph_checkpoint_roundtrip() {
        use requiem_core::task::{Task, TaskGraph};

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ckpt.db");
        let session = Session::open(&db_path).unwrap();
        let sid = Uuid::new_v4();

        // Build a graph with two tasks, one completed.
        let mut graph = TaskGraph::default();
        let a = graph.add(Task::new("A"));
        let _b = graph.add(Task::new("B").depends_on(a.clone()));
        let _ = graph.complete(a.clone(), serde_json::json!({"ok": true}));

        // Save, then load in a fresh connection.
        session.save_task_graph(&sid, &graph).unwrap();
        let reloaded = session
            .load_task_graph(&sid)
            .unwrap()
            .expect("checkpoint exists");

        assert_eq!(reloaded.len(), graph.len());
        assert_eq!(reloaded.get(&a).status, graph.get(&a).status);
        assert!(reloaded.get(&a).result.is_some());
    }

    #[test]
    fn load_task_graph_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("none.db");
        let session = Session::open(&db_path).unwrap();
        let sid = Uuid::new_v4();

        let result = session.load_task_graph(&sid).unwrap();
        assert!(result.is_none());
    }
}
