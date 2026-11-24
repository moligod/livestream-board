// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! Read per-generation model names from Antigravity's native conversation database.
use prost::Message;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{collections::HashMap, path::Path, time::Duration};

// Only the fields needed for the step -> generation -> model association are decoded.
#[derive(Clone, PartialEq, Message)]
struct StepMetadata {
    #[prost(message, optional, tag = "20")]
    generation: Option<GenerationReference>,
}

#[derive(Clone, PartialEq, Message)]
struct GenerationReference {
    // Protobuf omits the scalar when the generation index is zero.
    #[prost(uint64, tag = "3")]
    index: u64,
}

#[derive(Clone, PartialEq, Message)]
struct GenerationMetadata {
    #[prost(message, optional, tag = "1")]
    response: Option<ResponseMetadata>,
}

#[derive(Clone, PartialEq, Message)]
struct ResponseMetadata {
    #[prost(string, tag = "19")]
    model: String,
}

// Field 21 carries the tiered ID (`gemini-4-argon-high`) that matches `Model Selection`;
// older builds leave it out or store a display name. It is decoded on its own so an
// unexpected value there can never hide field 19.
#[derive(Clone, PartialEq, Message)]
struct GenerationModelId {
    #[prost(message, optional, tag = "1")]
    response: Option<ResponseModelId>,
}

#[derive(Clone, PartialEq, Message)]
struct ResponseModelId {
    #[prost(string, tag = "21")]
    model_id: String,
}

// Field 11 is the wait for the first token and field 12 the rest of the stream; both match the
// step's own created / first-output / done timestamps to the millisecond. Decoded on their own
// so an unexpected wire type there can never hide the model name.
#[derive(Clone, PartialEq, Message)]
struct GenerationTiming {
    #[prost(message, optional, tag = "1")]
    response: Option<ResponseTiming>,
}

#[derive(Clone, PartialEq, Message)]
struct ResponseTiming {
    #[prost(message, optional, tag = "11")]
    first_token: Option<DurationValue>,
    #[prost(message, optional, tag = "12")]
    streaming: Option<DurationValue>,
}

/// `google.protobuf.Duration`.
#[derive(Clone, PartialEq, Message)]
struct DurationValue {
    #[prost(int64, tag = "1")]
    seconds: i64,
    #[prost(int32, tag = "2")]
    nanos: i32,
}

impl DurationValue {
    fn millis(&self) -> Option<i64> {
        let ms = self
            .seconds
            .checked_mul(1000)?
            .checked_add(i64::from(self.nanos) / 1_000_000)?;
        (ms >= 0).then_some(ms)
    }
}

/// Exact timing recorded by agy for one generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Timing {
    pub latency_ms: i64,
    pub first_token_ms: i64,
}

#[derive(Default)]
pub(super) struct Generation {
    pub model: Option<String>,
    pub timing: Option<Timing>,
}

fn timing_of(bytes: &[u8]) -> Option<Timing> {
    let response = GenerationTiming::decode(bytes).ok()?.response?;
    let first_token_ms = response.first_token?.millis()?;
    let latency_ms = first_token_ms.checked_add(response.streaming?.millis()?)?;
    (latency_ms > 0).then_some(Timing {
        latency_ms,
        first_token_ms,
    })
}

fn native_database_path(transcript: &Path) -> Result<Option<std::path::PathBuf>, String> {
    // brain/<id>/.system_generated/logs/transcript_full.jsonl and conversations/<id>.db
    // share the same CLI/IDE/private-profile root.
    let session = transcript
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("Missing transcript session directory")?;
    let root = session
        .parent()
        .filter(|brain| brain.file_name().is_some_and(|name| name == "brain"))
        .and_then(Path::parent)
        .ok_or("Missing Antigravity root")?;
    let id = session.file_name().ok_or("Missing session id")?;
    let path = root.join("conversations").join(id).with_extension("db");
    if !path.is_file() {
        return Ok(None);
    }
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(root) {
        return Err("Conversation database is outside the Antigravity root".into());
    }
    Ok(Some(path))
}

/// Fingerprint of the companion `conversations/<id>.db` (and its WAL file when present) so
/// `session_log_sync` cursors stay valid across steady-state syncs while still re-importing
/// when the native database is created or updated after the transcript.
pub(crate) fn native_state_fingerprint(transcript: &Path) -> String {
    let Ok(Some(path)) = native_database_path(transcript) else {
        return "none".to_string();
    };
    let file_sig = |target: &Path| -> (i64, u64) {
        std::fs::metadata(target)
            .map(|meta| {
                (
                    crate::services::session_usage::metadata_modified_nanos(&meta),
                    meta.len(),
                )
            })
            .unwrap_or((0, 0))
    };
    let (db_mtime, db_len) = file_sig(&path);
    let mut wal_os = path.as_os_str().to_os_string();
    wal_os.push("-wal");
    let (wal_mtime, wal_len) = file_sig(Path::new(&wal_os));
    format!("{db_mtime}:{db_len}:{wal_mtime}:{wal_len}")
}

pub(super) fn generations_for_transcript(
    transcript: &Path,
    steps: &[u64],
) -> Result<HashMap<u64, Generation>, String> {
    let Some(path) = native_database_path(transcript)? else {
        return Ok(HashMap::new());
    };
    read_generations(&path, steps).map_err(|error| error.to_string())
}

fn read_generations(path: &Path, steps: &[u64]) -> rusqlite::Result<HashMap<u64, Generation>> {
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // A busy native writer must not hold up the usage refresh. Normal SQLite reads
    // include the WAL; immutable mode would miss live generations.
    conn.busy_timeout(Duration::ZERO)?;
    let tx = conn.transaction()?;
    let mut step_query = tx.prepare("SELECT metadata FROM steps WHERE idx=?1 AND step_type=15")?;
    let mut generation_query = tx.prepare("SELECT data FROM gen_metadata WHERE idx=?1")?;
    let mut generations = HashMap::new();
    for &step in steps {
        let Ok(index) = i64::try_from(step) else {
            continue;
        };
        let metadata: Option<Vec<u8>> = step_query
            .query_row([index], |row| row.get(0))
            .optional()?
            .flatten();
        let Some(reference) = metadata
            .and_then(|bytes| StepMetadata::decode(bytes.as_slice()).ok())
            .and_then(|metadata| metadata.generation)
        else {
            continue;
        };
        let Ok(generation) = i64::try_from(reference.index) else {
            continue;
        };
        let data: Option<Vec<u8>> = generation_query
            .query_row([generation], |row| row.get(0))
            .optional()?;
        let Some(bytes) = data else {
            continue;
        };
        let tiered = GenerationModelId::decode(bytes.as_slice())
            .ok()
            .and_then(|metadata| metadata.response)
            .map(|response| response.model_id);
        let base = GenerationMetadata::decode(bytes.as_slice())
            .ok()
            .and_then(|metadata| metadata.response)
            .map(|response| response.model);
        // Orchestrator's display field names the mode (or just "Gemini Flash"),
        // while field 19 identifies the planner/worker that produced this step.
        // Only these exact AGY aliases are mapped; unknown IDs keep their configured pricing.
        // https://antigravity.google/docs/models#orchestrator
        let orchestrator_model = match base.as_deref() {
            Some("gemini-pro-orchestrator") => Some("gemini-4-carbon"),
            Some("gemini-pro-orchestrator-argon" | "gemini-pro-orchestrator-g1-pro") => {
                Some("gemini-4-argon")
            }
            Some("gemini-pro-orchestrator-neon") => Some("gemini-4-neon"),
            Some("gemini-flash-worker-agy") => Some("gemini-4-hydrogen"),
            _ => None,
        };
        let tiered = tiered.filter(|model| {
            let id = super::model_id_of(model);
            // Keep a concrete field-21 model/tier if a newer AGY build supplies one.
            !(orchestrator_model.is_some()
                && (super::is_orchestrator(&id)
                    || matches!(id.as_str(), "gemini-flash" | "gemini-flash-worker-agy")))
        });
        let model = [tiered, orchestrator_model.map(str::to_string), base]
            .into_iter()
            .flatten()
            .map(|model| model.trim().to_string())
            .find(|model| !model.is_empty() && !model.eq_ignore_ascii_case("unknown"));
        let timing = timing_of(&bytes);
        if model.is_some() || timing.is_some() {
            generations.insert(step, Generation { model, timing });
        }
    }
    Ok(generations)
}

#[cfg(test)]
fn read_models(path: &Path, steps: &[u64]) -> rusqlite::Result<HashMap<u64, String>> {
    Ok(read_generations(path, steps)?
        .into_iter()
        .filter_map(|(step, generation)| Some((step, generation.model?)))
        .collect())
}

#[cfg(test)]
pub(super) fn write_test_database(path: &Path, entries: &[(i64, u64, &str)]) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);
         CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    for &(step, generation, model) in entries {
        let metadata = StepMetadata {
            generation: Some(GenerationReference { index: generation }),
        };
        let data = GenerationMetadata {
            response: Some(ResponseMetadata {
                model: model.into(),
            }),
        };
        conn.execute(
            "INSERT INTO steps VALUES (?1,15,?2)",
            rusqlite::params![step, metadata.encode_to_vec()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata VALUES (?1,?2)",
            rusqlite::params![generation, data.encode_to_vec()],
        )
        .unwrap();
    }
    conn
}

/// Rewrites a generation with field 19 and an optional raw field 21 (any wire type).
#[cfg(test)]
pub(super) fn set_test_generation(conn: &Connection, generation: i64, base: &str, field21: &[u8]) {
    let mut response = ResponseMetadata { model: base.into() }.encode_to_vec();
    response.extend_from_slice(field21);
    let mut data = Vec::new();
    prost::encoding::bytes::encode(1, &response, &mut data);
    conn.execute(
        "UPDATE gen_metadata SET data=?1 WHERE idx=?2",
        rusqlite::params![data, generation],
    )
    .unwrap();
}

/// Field 21 encoded as a string.
#[cfg(test)]
pub(super) fn test_model_id(id: &str) -> Vec<u8> {
    let mut field = Vec::new();
    prost::encoding::string::encode(21, &id.to_string(), &mut field);
    field
}

/// Fields 11 and 12 as durations of the given milliseconds.
#[cfg(test)]
pub(super) fn test_timing(first_token_ms: i64, streaming_ms: i64) -> Vec<u8> {
    let duration = |ms: i64| DurationValue {
        seconds: ms / 1000,
        nanos: i32::try_from(ms % 1000 * 1_000_000).unwrap(),
    };
    let mut fields = Vec::new();
    prost::encoding::message::encode(11, &duration(first_token_ms), &mut fields);
    prost::encoding::message::encode(12, &duration(streaming_ms), &mut fields);
    fields
}
