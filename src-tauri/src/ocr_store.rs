//! Short, guarded SQLite operations. Result fields survive every failed attempt.
use crate::database::{self, Database};
use crate::files::current_timestamp_millis;
use crate::models::{OcrContent, OcrResult, OcrSelection};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) fn saved_path(db: &Database, id: &str) -> Result<String, String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let path: Option<Option<String>> = conn
        .query_row(
            "SELECT output_path FROM history_entries WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    path.ok_or("This history entry no longer exists.")?
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            "No saved video path is available for this entry. OCR will not guess another filename."
                .into()
        })
}

pub(crate) fn begin(db: &Database, id: &str, attempt: &str) -> Result<(), String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let tx = database::write_transaction(&conn)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_entries WHERE id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("This history entry no longer exists.".into());
    }
    tx.execute("INSERT INTO history_ocr (history_entry_id,last_attempt_id,last_attempt_status,last_attempt_at)
        VALUES (?1,?2,'running',?3) ON CONFLICT(history_entry_id) DO UPDATE SET
        last_attempt_id=excluded.last_attempt_id,last_attempt_status='running',last_attempt_at=excluded.last_attempt_at,last_error=NULL",
        params![id, attempt, current_timestamp_millis() as i64]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn finish(
    db: &Database,
    id: &str,
    attempt: &str,
    result: &OcrResult,
) -> Result<(), String> {
    let segments = serde_json::to_string(&result.segments).map_err(|e| e.to_string())?;
    if segments.len().saturating_add(result.text.len()) > crate::ocr_rules::MAX_RESULT_BYTES {
        return Err("OCR result exceeds the 32 MiB storage limit.".into());
    }
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let tx = database::write_transaction(&conn)?;
    let changed = tx
        .execute(
            "UPDATE history_ocr SET text=?3,segments_json=?4,source_sha256=?5,
        engine='apple_vision',engine_version=?6,settings_json=?7,result_created_at=?8,
        last_attempt_status='completed',last_error=NULL
        WHERE history_entry_id=?1 AND last_attempt_id=?2 AND last_attempt_status='running'",
            params![
                id,
                attempt,
                result.text,
                segments,
                result.source_sha256,
                result.engine_version,
                result.settings_json,
                current_timestamp_millis() as i64
            ],
        )
        .map_err(|e| format!("OCR result could not be saved: {e}"))?;
    if changed != 1 {
        return Err("OCR attempt is no longer current or its history entry was deleted.".into());
    }
    tx.commit()
        .map_err(|e| format!("OCR result could not be committed: {e}"))
}

pub(crate) fn fail(
    db: &Database,
    id: &str,
    attempt: &str,
    status: &str,
    error: &str,
) -> Result<(), String> {
    if !["error", "cancelled", "interrupted"].contains(&status) {
        return Err("Invalid OCR failure status.".into());
    }
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let tx = database::write_transaction(&conn)?;
    tx.execute(
        "UPDATE history_ocr SET last_attempt_status=?3,last_error=?4
        WHERE history_entry_id=?1 AND last_attempt_id=?2 AND last_attempt_status='running'",
        params![id, attempt, status, error],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn interrupt_orphans(db: &Database) -> Result<(), String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let running: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_ocr WHERE last_attempt_status='running')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !running {
        return Ok(());
    }
    let tx = database::write_transaction(&conn)?;
    tx.execute("UPDATE history_ocr SET last_attempt_status='interrupted',last_error='The app stopped before OCR finished.' WHERE last_attempt_status='running'", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn get(db: &Database, id: &str) -> Result<Option<OcrContent>, String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    read(&conn, id)
}

fn read(conn: &Connection, id: &str) -> Result<Option<OcrContent>, String> {
    let row = conn.query_row("SELECT history_entry_id,text,segments_json,source_sha256,engine,engine_version,settings_json,
        result_created_at,last_attempt_id,last_attempt_status,last_attempt_at,last_error FROM history_ocr WHERE history_entry_id=?1", [id], |r| {
        let segments: Option<String> = r.get(2)?;
        Ok((OcrContent { history_entry_id:r.get(0)?,text:r.get(1)?,segments:None,result_revision:None,source_sha256:r.get(3)?,engine:r.get(4)?,engine_version:r.get(5)?,settings_json:r.get(6)?,result_created_at:r.get(7)?,last_attempt_id:r.get(8)?,last_attempt_status:r.get(9)?,last_attempt_at:r.get(10)?,last_error:r.get(11)? }, segments))
    }).optional().map_err(|e| format!("OCR could not be loaded: {e}"))?;
    row.map(|(mut content, json)| {
        content.result_revision = json.as_ref().map(|s| {
            crate::hashing::sha256_hex(
                format!(
                    "{}:{}:{s}",
                    content.source_sha256.as_deref().unwrap_or(""),
                    content.result_created_at.unwrap_or(0)
                )
                .as_bytes(),
            )
        });
        content.segments = json
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| "Stored OCR segments are invalid.")?;
        Ok(content)
    })
    .transpose()
}

pub(crate) fn validate_selection(
    content: &OcrContent,
    selection: &OcrSelection,
) -> Result<(), String> {
    if content.result_revision.as_deref() != Some(&selection.result_revision) {
        return Err("OCR results changed. Reload the detected text and try again.".into());
    }
    if !content
        .segments
        .as_ref()
        .and_then(|segments| segments.get(selection.segment_index))
        .is_some_and(|segment| segment.observed_at_ms.contains(&selection.observed_at_ms))
    {
        return Err("This OCR observation is no longer available.".into());
    }
    Ok(())
}

pub(crate) fn delete_observation(
    db: &Database,
    id: &str,
    selection: &OcrSelection,
) -> Result<OcrContent, String> {
    delete_selection(db, id, selection, false)
}

pub(crate) fn delete_segment(
    db: &Database,
    id: &str,
    selection: &OcrSelection,
) -> Result<OcrContent, String> {
    delete_selection(db, id, selection, true)
}

fn delete_selection(
    db: &Database,
    id: &str,
    selection: &OcrSelection,
    whole_segment: bool,
) -> Result<OcrContent, String> {
    let conn = db.lock().map_err(|_| "SQLite lock poisoned")?;
    let tx = database::write_transaction(&conn)?;
    let content = read(&tx, id)?.ok_or("No saved OCR result is available.")?;
    if content.last_attempt_status == "running" {
        return Err("Wait for the OCR analysis to finish before deleting detected text.".into());
    }
    validate_selection(&content, selection)?;
    let mut segments = content
        .segments
        .ok_or("No saved OCR result is available.")?;
    if whole_segment {
        segments.remove(selection.segment_index);
    } else {
        let segment = &mut segments[selection.segment_index];
        segment
            .observed_at_ms
            .retain(|time| *time != selection.observed_at_ms);
        if segment.observed_at_ms.is_empty() {
            segments.remove(selection.segment_index);
        }
    }
    let text = crate::ocr_rules::readable_text(&segments);
    let json = serde_json::to_string(&segments).map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE history_ocr SET text=?2,segments_json=?3 WHERE history_entry_id=?1",
        params![id, text, json],
    )
    .map_err(|e| format!("OCR text could not be deleted: {e}"))?;
    let updated = read(&tx, id)?.ok_or("The OCR result is no longer available.")?;
    tx.commit()
        .map_err(|e| format!("OCR deletion could not be committed: {e}"))?;
    Ok(updated)
}
