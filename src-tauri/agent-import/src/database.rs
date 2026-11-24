use crate::{app_config::AppType, Record};
use rusqlite::Connection;
use std::{collections::HashSet, sync::Mutex};

macro_rules! lock_conn {
    ($conn:expr) => {
        $conn
            .lock()
            .map_err(|e| crate::error::AppError::Database(e.to_string()))?
    };
}
pub(crate) use lock_conn;

pub struct Database {
    pub conn: Mutex<Connection>,
}

impl Database {
    // Private ephemeral staging for upstream request-identity logic, not a persisted copy.
    pub fn memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(include_str!("stage.sql"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn get_provider_ids(&self, _: &AppType) -> rusqlite::Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    pub fn records(&self, source: &str) -> rusqlite::Result<Vec<Record>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let mut stmt = conn.prepare(
            "SELECT request_id, session_id, created_at, model, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens, total_cost_usd, input_token_semantics
             FROM proxy_request_logs WHERE app_type = ?1 ORDER BY created_at, request_id",
        )?;
        let rows = stmt.query_map([source], |row| {
            let input: i64 = row.get(4)?;
            let output: i64 = row.get(5)?;
            let read: i64 = row.get(6)?;
            let write: i64 = row.get(7)?;
            let cost: String = row.get(8)?;
            let semantics: Option<String> = row.get(9)?;
            let total_input = if semantics.as_deref() == Some("fresh") {
                input.saturating_add(read).saturating_add(write)
            } else {
                input.max(read.saturating_add(write))
            };
            let fresh_input = total_input.saturating_sub(read).saturating_sub(write);
            Ok(Record {
                source: source.into(),
                record_id: row.get(0)?,
                session_id: row.get(1)?,
                timestamp: row.get(2)?,
                model: row.get(3)?,
                project: "(unknown)".into(),
                input_tokens: fresh_input,
                output_tokens: output,
                reasoning_tokens: 0,
                cache_read_tokens: read,
                cache_write_tokens: write,
                total_tokens: total_input.saturating_add(output),
                cost: cost
                    .parse::<f64>()
                    .ok()
                    .filter(|cost| cost.is_finite() && *cost > 0.0)
                    .unwrap_or(0.0),
            })
        })?;
        rows.collect()
    }
}
