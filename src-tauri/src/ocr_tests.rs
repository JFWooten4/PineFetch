//! OCR-specific rule, storage, migration and isolated process regressions.
use crate::database::{self, Database, MigrationPoint};
use crate::models::{OcrProgress, OcrResult};
use crate::ocr::{self, OcrState, OcrTools};
use crate::ocr_rules::{self, Observation, OcrBox, SegmentBuilder};
use crate::ocr_store;
use crate::test_support::TempRoot;
use rusqlite::Connection;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn db() -> Database {
    let conn = Connection::open_in_memory().unwrap();
    database::initialize(&conn).unwrap();
    conn.execute("INSERT INTO history_entries (id,url,created_at) VALUES ('video','https://example.com/video',1)", []).unwrap();
    Database::new(conn)
}
fn result(text: &str) -> OcrResult {
    let mut builder = SegmentBuilder::default();
    if !text.is_empty() {
        builder.push(3020, vec![observation(text, 0.1)]).unwrap();
    }
    OcrResult {
        text: ocr_rules::readable_text(&builder.segments),
        segments: builder.segments,
        source_sha256: "a".repeat(64),
        engine_version:
            r#"{"vision_request_revision":3,"macos_version":"synthetic","helper_version":"2"}"#
                .into(),
        settings_json: "{}".into(),
    }
}
fn observation(text: &str, x: f64) -> Observation {
    Observation {
        text: text.into(),
        bounds: OcrBox {
            x,
            y: 0.1,
            width: 0.2,
            height: 0.1,
        },
        confidence: 0.9,
    }
}
fn progress(attempt: &str) -> OcrProgress {
    OcrProgress {
        history_id: "video".into(),
        attempt_id: attempt.into(),
        attempt_at: 1,
        status: "running".into(),
        frames_checked: 0,
        observed_at_ms: None,
        total_duration_ms: None,
        total_frames: None,
        total_frames_is_estimate: false,
        message: None,
    }
}

fn selection(
    content: &crate::models::OcrContent,
    index: usize,
    time: u64,
) -> crate::models::OcrSelection {
    crate::models::OcrSelection {
        result_revision: content.result_revision.clone().unwrap(),
        segment_index: index,
        observed_at_ms: time,
    }
}

#[test]
fn deleting_ocr_observations_preserves_other_times_and_rejects_stale_or_running_edits() {
    let db = db();
    let mut saved = result("Repeated text");
    saved.segments[0].observed_at_ms.push(4030);
    saved.text = ocr_rules::readable_text(&saved.segments);
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &saved).unwrap();
    let original = ocr_store::get(&db, "video").unwrap().unwrap();
    let stale = selection(&original, 0, 3020);
    let mut invalid = stale.clone();
    invalid.segment_index = usize::MAX;
    assert!(ocr_store::delete_observation(&db, "video", &invalid).is_err());
    invalid = stale.clone();
    invalid.observed_at_ms = 4000;
    assert!(ocr_store::delete_observation(&db, "video", &invalid).is_err());
    ocr_store::begin(&db, "video", "rerun").unwrap();
    assert!(ocr_store::delete_observation(&db, "video", &stale)
        .unwrap_err()
        .contains("finish"));
    ocr_store::fail(&db, "video", "rerun", "cancelled", "Cancelled").unwrap();
    db.lock().unwrap().execute_batch("CREATE TRIGGER reject_ocr_delete BEFORE UPDATE OF segments_json ON history_ocr BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;").unwrap();
    assert!(ocr_store::delete_observation(&db, "video", &stale).is_err());
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .result_revision,
        original.result_revision
    );
    db.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_ocr_delete;")
        .unwrap();
    let edited = ocr_store::delete_observation(&db, "video", &stale).unwrap();
    assert_eq!(
        edited.segments.as_ref().unwrap()[0].observed_at_ms,
        vec![4030]
    );
    assert_eq!(edited.text.as_deref(), Some("[00:00:04.030] Repeated text"));
    assert_eq!(edited.source_sha256, original.source_sha256);
    assert_eq!(edited.result_created_at, original.result_created_at);
    assert_eq!(edited.engine_version, original.engine_version);
    assert_ne!(edited.result_revision, original.result_revision);
    assert!(ocr_store::delete_observation(&db, "video", &stale)
        .unwrap_err()
        .contains("changed"));
    let empty = ocr_store::delete_observation(&db, "video", &selection(&edited, 0, 4030)).unwrap();
    assert!(empty.segments.unwrap().is_empty());
    assert_eq!(empty.text.as_deref(), Some(""));
    assert_eq!(empty.last_attempt_status, "cancelled");
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .text
            .as_deref(),
        Some("")
    );
}

#[test]
fn integration_ocr_deleted_text_survives_reopening_without_a_schema_change() {
    let root = TempRoot::new("ocr-delete-store");
    let path = root.path("history.sqlite");
    let conn = Connection::open(&path).unwrap();
    database::initialize(&conn).unwrap();
    conn.execute("INSERT INTO history_entries (id,url,created_at) VALUES ('video','https://example.com/video',1)", []).unwrap();
    let db = Database::new(conn);
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &result("Unwanted OCR text")).unwrap();
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    ocr_store::delete_observation(&db, "video", &selection(&content, 0, 3020)).unwrap();
    drop(db);
    let conn = Connection::open(&path).unwrap();
    database::initialize(&conn).unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let content = ocr_store::get(&Database::new(conn), "video")
        .unwrap()
        .unwrap();
    assert_eq!(content.text.as_deref(), Some(""));
    assert!(content.segments.unwrap().is_empty());
    assert_eq!(content.last_attempt_status, "completed");
}

#[cfg(target_os = "macos")]
#[test]
fn integration_ocr_preview_matches_vfr_frames_with_a_nonzero_origin_and_detects_changed_sources() {
    use base64::Engine;
    use std::path::Path;
    use std::process::Command;
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/ffmpeg-runtime/bin");
    let ffmpeg = directory.join("ffmpeg");
    let ffprobe = directory.join("ffprobe");
    if !ffmpeg.is_file() || !ffprobe.is_file() {
        eprintln!("Native preview smoke requires npm run prepare:ffmpeg-runtime.");
        return;
    }
    let root = TempRoot::new("ocr-preview");
    let video = root.path("vfr.mkv");
    let made = Command::new(&ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=25:duration=4",
            "-vf",
            "select='eq(n,0)+eq(n,10)+eq(n,28)+eq(n,60)+eq(n,88)',setpts=PTS+5/TB",
            "-fps_mode",
            "vfr",
            "-c:v",
            "ffv1",
        ])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let db = db();
    db.lock()
        .unwrap()
        .execute(
            "UPDATE history_entries SET output_path=?1 WHERE id='video'",
            [video.to_str().unwrap()],
        )
        .unwrap();
    let mut saved = result("Text");
    saved.segments[0].observed_at_ms = vec![0, 1120, 2400, 3520];
    saved.source_sha256 = crate::hashing::sha256_from_path(video.to_str())
        .unwrap()
        .unwrap();
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &saved).unwrap();
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    let preview = crate::ocr_preview::OcrPreviewState::default();
    let processes = crate::process::ProcessState::default();
    for (time, frame_index) in [(0, 0), (1120, 2), (2400, 3), (3520, 4)] {
        let image = preview
            .frame(
                &db,
                &processes,
                "video",
                &selection(&content, 0, time),
                &ffmpeg,
                &ffprobe,
            )
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        // Independent full decode by frame number must match the fast PTS seek.
        let expected = Command::new(&ffmpeg)
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&video)
            .arg("-vf")
            .arg(format!("select='eq(n,{frame_index})',setsar=1"))
            .args([
                "-frames:v",
                "1",
                "-fps_mode",
                "passthrough",
                "-threads",
                "1",
                "-c:v",
                "png",
                "-f",
                "image2pipe",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(expected.status.success());
        assert_eq!(bytes, expected.stdout, "Wrong preview at {time} ms");
        assert!(processes.utility_children.lock().unwrap().is_empty());
    }
    let stale = selection(&content, 0, 1120);
    ocr_store::delete_observation(&db, "video", &stale).unwrap();
    assert!(preview
        .frame(&db, &processes, "video", &stale, &ffmpeg, &ffprobe)
        .unwrap_err()
        .contains("changed"));
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    fs::write(&video, b"changed video").unwrap();
    assert!(preview
        .frame(
            &db,
            &processes,
            "video",
            &selection(&content, 0, 2400),
            &ffmpeg,
            &ffprobe
        )
        .unwrap_err()
        .contains("changed"));
}

#[test]
fn grouping_preserves_positions_reappearances_numbers_and_single_frames() {
    let mut builder = SegmentBuilder::default();
    builder
        .push(
            3020,
            vec![
                observation("12  Kilometer", 0.1),
                observation("12 Kilometer", 0.6),
            ],
        )
        .unwrap();
    builder
        .push(
            4030,
            vec![
                observation("12 Kilometer", 0.1),
                observation("13 Kilometer", 0.6),
            ],
        )
        .unwrap();
    builder.push(5040, vec![]).unwrap();
    builder
        .push(
            6100,
            vec![
                observation("12 Kilometer", 0.1),
                observation("not 12 Kilometer!", 0.6),
            ],
        )
        .unwrap();
    assert_eq!(builder.segments.len(), 5);
    assert_eq!(builder.segments[0].observed_at_ms, vec![3020, 4030]);
    assert_eq!(builder.segments[1].observed_at_ms, vec![3020]);
    assert_eq!(builder.segments[2].text, "13 Kilometer");
    assert_eq!(builder.segments[3].observed_at_ms, vec![6100]);
    assert!(ocr_rules::readable_text(&builder.segments)
        .contains("[00:00:03.020, 00:00:04.030] 12 Kilometer"));
}

#[test]
fn vision_edge_boxes_are_clipped_before_strict_protocol_validation() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../../test/fixtures/ocr-bounds.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let Some(bounds) = case.get("visible").filter(|v| !v.is_null()) else {
            continue;
        };
        let raw = serde_json::json!({"type":"frame","frame_id":9,"observations":[{
            "text":"edge text / 12!", "confidence":0.3, "bounds":case["bounds"]
        }]});
        // These native observations used to abort the entire OCR run. The
        // helper now clips them before emitting the strictly validated reply.
        assert!(ocr_rules::validate_observations(&raw, 9).is_err());
        let mut clipped = raw;
        clipped["observations"][0]["bounds"] = bounds.clone();
        let valid = ocr_rules::validate_observations(&clipped, 9).unwrap();
        assert_eq!(valid[0].text, "edge text / 12!");
        assert_eq!(valid[0].confidence, 0.3);
        assert_eq!(
            valid[0].bounds,
            serde_json::from_value::<OcrBox>(bounds.clone()).unwrap()
        );
    }
}

#[test]
fn local_video_duration_and_sample_estimates_handle_unknown_metadata() {
    let local_cut = br#"{"format":{"duration":"3.521"},"streams":[{"index":0}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(local_cut, 0), Some(3521));
    assert_eq!(ocr_rules::estimated_frame_count(3521), 4);
    assert_eq!(ocr_rules::estimated_frame_count(1000), 1);
    assert_eq!(ocr_rules::estimated_frame_count(1001), 2);
    let stream_only = br#"{"format":{"duration":"N/A"},"streams":[{"index":0,"duration":"9"},{"index":2,"duration":1.31}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(stream_only, 2), Some(1310));
    assert_eq!(ocr_rules::video_duration_ms(stream_only, 1), None);
    // Real FFprobe shapes from two-second files shifted five seconds forward.
    let matroska = br#"{"format":{"format_name":"matroska,webm","duration":"7.0","start_time":"5.0"},"streams":[{"index":0,"start_time":"5.0","tags":{"DURATION":"00:00:07.000000000"}}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(matroska, 0), Some(2000));
    let matroska_without_tags = br#"{"format":{"format_name":"matroska,webm","duration":"7.0","start_time":"5.0"},"streams":[{"index":0}]}"#;
    assert_eq!(
        ocr_rules::video_duration_ms(matroska_without_tags, 0),
        Some(2000)
    );
    let mp4 = br#"{"format":{"format_name":"mov,mp4","duration":"2.0","start_time":"5.0"},"streams":[{"index":0,"duration":"2.0","start_time":"5.0"}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(mp4, 0), Some(2000));
    let ts = br#"{"format":{"format_name":"mpegts","duration":"2.0","start_time":"6.4"},"streams":[{"index":0,"duration":"2.0","start_time":"6.4"}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(ts, 0), Some(2000));
    let delayed_video = br#"{"format":{"duration":"2.0","start_time":"5.0"},"streams":[{"index":0,"duration":"1.0","start_time":"5.5"}]}"#;
    assert_eq!(ocr_rules::video_duration_ms(delayed_video, 0), Some(1500));
    for duration in ["N/A", "NaN", "inf", "-1", "0", "1e300"] {
        let probe = serde_json::json!({"format":{"duration":duration}}).to_string();
        assert_eq!(ocr_rules::video_duration_ms(probe.as_bytes(), 0), None);
    }
    assert_eq!(ocr_rules::video_duration_ms(b"not json", 0), None);
    assert_eq!(ocr_rules::video_duration_ms(b"{}", 0), None);
}

#[test]
fn variable_frame_times_and_invalid_protocol_data_are_explicit() {
    let ready = serde_json::json!({"type":"ready","protocol_version":1,"engine":"apple_vision",
        "engine_version":{"helper_version":"2","vision_request_revision":3,"macos_version":"synthetic"},
        "settings":{"recognition_level":"accurate","uses_language_correction":false,"recognition_languages":["en-US"],"supported_languages":["en-US"]}});
    ocr_rules::validate_ready(&ready).unwrap();
    let mut unsupported = ready.clone();
    unsupported["settings"]["recognition_languages"] = serde_json::json!(["unsupported"]);
    assert!(ocr_rules::validate_ready(&unsupported).is_err());
    for (number, time) in [(0, "0.12"), (1, "1.31"), (2, "2.92")] {
        assert_eq!(
            ocr_rules::parse_frame_time(&format!(
                "[Parsed_showinfo_1 @ x] n: {number} pts: 1 pts_time:{time} fmt:rgba"
            ))
            .unwrap(),
            Some((
                number,
                (time.parse::<f64>().unwrap() * 1000.0).round() as u64
            ))
        );
    }
    for time in ["N/A", "NaN", "-1", "inf"] {
        assert!(
            ocr_rules::parse_frame_time(&format!("[Parsed_showinfo_1] n: 0 pts_time:{time}"))
                .is_err()
        );
    }
    assert!(ocr_rules::parse_frame_time("unrelated FFmpeg diagnostic")
        .unwrap()
        .is_none());
    let valid = serde_json::json!({"type":"frame","frame_id":4,"observations":[{"text":"@name / 12!","confidence":0.8,"bounds":{"x":0.1,"y":0.2,"width":0.3,"height":0.1}}]});
    assert_eq!(
        ocr_rules::validate_observations(&valid, 4).unwrap()[0].text,
        "@name / 12!"
    );
    assert!(ocr_rules::validate_observations(&valid, 5).is_err());
    let mut invalid = valid.clone();
    invalid["observations"][0]["bounds"]["x"] = serde_json::json!(2);
    assert!(ocr_rules::validate_observations(&invalid, 4).is_err());
    assert!(ocr_rules::validate_observations(&serde_json::json!({"frame_id":4}), 4).is_err());
    let probe = br#"{"streams":[{"index":0,"codec_type":"audio"},{"index":1,"codec_type":"video","width":200,"height":100,"disposition":{"attached_pic":1}},{"index":2,"codec_type":"video","width":200,"height":100}]}"#;
    assert_eq!(ocr_rules::video_stream_index(probe).unwrap(), 2);
    assert!(
        ocr_rules::video_stream_index(br#"{"streams":[{"index":0,"codec_type":"audio"}]}"#)
            .is_err()
    );
}

#[test]
fn successful_and_empty_results_have_durable_metadata() {
    let db = db();
    ocr_store::begin(&db, "video", "first").unwrap();
    let initial = ocr_store::get(&db, "video").unwrap().unwrap();
    assert!(initial.result_created_at.is_none());
    assert!(initial.text.is_none());
    ocr_store::finish(
        &db,
        "video",
        "first",
        &result("Text <b> remains plain text"),
    )
    .unwrap();
    let saved = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(saved.engine.as_deref(), Some("apple_vision"));
    assert_eq!(saved.source_sha256, Some("a".repeat(64)));
    assert!(saved.result_created_at.is_some());
    ocr_store::begin(&db, "video", "empty").unwrap();
    ocr_store::finish(&db, "video", "empty", &result("")).unwrap();
    let empty = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(empty.text.as_deref(), Some(""));
    assert_eq!(empty.segments, Some(vec![]));
    assert!(empty.result_created_at.is_some());
    assert_eq!(empty.last_attempt_status, "completed");
}

#[test]
fn failed_cancelled_and_stale_attempts_preserve_the_previous_result() {
    let db = db();
    ocr_store::begin(&db, "video", "good").unwrap();
    ocr_store::finish(&db, "video", "good", &result("Previous")).unwrap();
    let saved = ocr_store::get(&db, "video").unwrap().unwrap();
    for status in ["error", "cancelled", "interrupted"] {
        ocr_store::begin(&db, "video", status).unwrap();
        assert!(ocr_store::finish(&db, "video", "good", &result("Stale")).is_err());
        ocr_store::fail(&db, "video", status, status, "synthetic failure").unwrap();
        assert!(ocr_store::finish(&db, "video", status, &result("Too late")).is_err());
        let current = ocr_store::get(&db, "video").unwrap().unwrap();
        assert_eq!(current.text, saved.text);
        assert_eq!(current.result_created_at, saved.result_created_at);
        assert_eq!(current.last_attempt_status, status);
    }
}

#[test]
fn history_pages_batch_ocr_statuses_without_loading_result_text() {
    let db = db();
    db.lock().unwrap().execute("INSERT INTO history_entries (id,url,created_at) VALUES ('other','https://example.com/other',2)", []).unwrap();
    ocr_store::begin(&db, "video", "good").unwrap();
    ocr_store::finish(&db, "video", "good", &result("Saved text")).unwrap();
    ocr_store::begin(&db, "video", "retry").unwrap();
    ocr_store::fail(&db, "video", "retry", "error", "synthetic failure").unwrap();
    ocr_store::begin(&db, "other", "active").unwrap();
    let page = crate::history::list_history_page_from_db(&db, 20, 0).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.total_count, 2);
    assert_eq!(page.ocr_statuses.len(), 2);
    let active = &page.ocr_statuses[0];
    assert_eq!(active.history_id, "other");
    assert_eq!(active.status, "running");
    assert!(active.result_created_at.is_none());
    let failed = &page.ocr_statuses[1];
    assert_eq!(failed.history_id, "video");
    assert_eq!(failed.status, "error");
    assert!(failed.result_created_at.is_some());
    let first = crate::history::list_history_page_from_db(&db, 1, 0).unwrap();
    assert_eq!(first.ocr_statuses.len(), 1);
    assert_eq!(first.ocr_statuses[0].history_id, first.entries[0].id);
    assert!(first.has_more);
}

#[test]
fn integration_saved_ocr_and_history_summary_survive_reopening_schema_three() {
    let root = TempRoot::new("ocr-detail-persistence");
    let path = root.path("state.sqlite");
    let db = Database::new(database::open(&path).unwrap());
    db.lock().unwrap().execute("INSERT INTO history_entries (id,url,created_at) VALUES ('video','https://example.com/video',1)", []).unwrap();
    ocr_store::begin(&db, "video", "saved").unwrap();
    ocr_store::finish(&db, "video", "saved", &result("Durable result")).unwrap();
    let saved = ocr_store::get(&db, "video").unwrap().unwrap();
    drop(db);
    let reopened = Database::new(database::open(&path).unwrap());
    let content = ocr_store::get(&reopened, "video").unwrap().unwrap();
    assert_eq!(content.text, saved.text);
    assert_eq!(content.segments, saved.segments);
    assert_eq!(content.result_created_at, saved.result_created_at);
    let page = crate::history::list_history_page_from_db(&reopened, 20, 0).unwrap();
    assert_eq!(page.ocr_statuses[0].status, "completed");
    assert_eq!(
        page.ocr_statuses[0].result_created_at,
        saved.result_created_at
    );
}

#[test]
fn cancellation_before_final_commit_and_storage_failure_cannot_report_success() {
    let db = db();
    ocr_store::begin(&db, "video", "old").unwrap();
    ocr_store::finish(&db, "video", "old", &result("Old")).unwrap();
    let state = OcrState::default();
    ocr_store::begin(&db, "video", "cancel").unwrap();
    ocr::test_active(&state, progress("cancel"));
    state.cancel(&db, "video", "cancel").unwrap();
    assert_eq!(
        ocr::finalize(&db, &state, Ok(result("New")), || Ok(()))
            .unwrap()
            .status,
        "cancelled"
    );
    assert!(ocr_store::get(&db, "video")
        .unwrap()
        .unwrap()
        .text
        .unwrap()
        .ends_with("Old"));
    let state = OcrState::default();
    ocr_store::begin(&db, "video", "disk").unwrap();
    ocr::test_active(&state, progress("disk"));
    db.lock().unwrap().execute_batch("CREATE TRIGGER fail_ocr BEFORE UPDATE OF text ON history_ocr BEGIN SELECT RAISE(ABORT,'synthetic disk failure'); END;").unwrap();
    let event = ocr::finalize(&db, &state, Ok(result("New")), || Ok(())).unwrap();
    assert_eq!(event.status, "error");
    assert!(event.message.unwrap().contains("synthetic disk failure"));
    assert!(ocr_store::get(&db, "video")
        .unwrap()
        .unwrap()
        .text
        .unwrap()
        .ends_with("Old"));
}

#[test]
fn deletion_and_changed_source_before_commit_do_not_restore_or_replace_results() {
    let db = db();
    let state = OcrState::default();
    ocr_store::begin(&db, "video", "changed").unwrap();
    ocr::test_active(&state, progress("changed"));
    assert_eq!(
        ocr::finalize(&db, &state, Ok(result("Wrong file")), || Err(
            "source replaced".into()
        ))
        .unwrap()
        .status,
        "error"
    );
    assert!(ocr_store::get(&db, "video")
        .unwrap()
        .unwrap()
        .text
        .is_none());
    ocr_store::begin(&db, "video", "deleted").unwrap();
    db.lock()
        .unwrap()
        .execute("DELETE FROM history_entries WHERE id='video'", [])
        .unwrap();
    assert!(ocr_store::finish(&db, "video", "deleted", &result("Late")).is_err());
    assert!(ocr_store::get(&db, "video").unwrap().is_none());
    assert!(ocr_store::begin(&db, "video", "resurrect").is_err());
}

#[test]
fn integration_schema_two_upgrade_failure_rolls_back_and_retry_preserves_rows() {
    let root = TempRoot::new("ocr-migration");
    let path = root.path("state.sqlite");
    let conn = database::open(&path).unwrap();
    conn.execute_batch("DROP TABLE history_ocr; PRAGMA user_version=2;
        INSERT INTO history_entries (id,url,title,created_at) VALUES ('video','https://example.com','Grüße 🌲',123);").unwrap();
    let before: Vec<u8> = fs::read(&path).unwrap();
    drop(conn);
    assert!(
        database::open_with_hook(&path, |point| if point == MigrationPoint::Ocr {
            Err("synthetic migration failure".into())
        } else {
            Ok(())
        })
        .is_err()
    );
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='history_ocr'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    assert_eq!(fs::read(&path).unwrap(), before);
    let conn = database::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row(
            "SELECT title,created_at FROM history_entries WHERE id='video'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        ("Grüße 🌲".into(), 123)
    );
    drop(conn);
    database::open(&path).unwrap();
}

#[test]
fn integration_strict_ocr_layout_refuses_changed_constraints_and_future_versions() {
    let root = TempRoot::new("ocr-layout");
    let path = root.path("state.sqlite");
    let conn = database::open(&path).unwrap();
    let original: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='history_ocr'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch("DROP TABLE history_ocr;").unwrap();
    drop(conn);
    assert!(database::open(&path)
        .unwrap_err()
        .contains("missing history_ocr"));
    let conn = Connection::open(&path).unwrap();
    let without_constraint = original.replace(" CHECK (last_attempt_status IN ('running', 'completed', 'error', 'cancelled', 'interrupted'))", "");
    assert_ne!(without_constraint, original);
    conn.execute_batch(&without_constraint).unwrap();
    drop(conn);
    assert!(database::open(&path)
        .unwrap_err()
        .contains("OCR attempt status constraint"));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE history_ocr").unwrap();
    conn.execute_batch(&original).unwrap();
    conn.pragma_update(None, "user_version", 4).unwrap();
    drop(conn);
    let bytes = fs::read(&path).unwrap();
    assert!(database::open(&path)
        .unwrap_err()
        .contains("schema version 4"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[cfg(target_os = "macos")]
#[test]
fn integration_instance_ownership_only_interrupts_orphans_after_the_owner_stops() {
    let root = TempRoot::new("ocr-owner");
    let db = db();
    let first = OcrState::initialize(root.root(), &db).unwrap();
    ocr_store::begin(&db, "video", "live").unwrap();
    let second = OcrState::initialize(root.root(), &db).unwrap();
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .last_attempt_status,
        "running"
    );
    drop(second);
    drop(first);
    let _third = OcrState::initialize(root.root(), &db).unwrap();
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .last_attempt_status,
        "interrupted"
    );
}

#[cfg(unix)]
fn script(root: &TempRoot, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.path(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[cfg(unix)]
fn tools(root: &TempRoot, helper_body: &str) -> OcrTools {
    let png = root.path("frame.png");
    fs::write(
        &png,
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0, 73, 69, 78, 68, 0, 0, 0, 0,
        ],
    )
    .unwrap();
    OcrTools { helper:script(root,"vision",helper_body),
        ffprobe:script(root,"probe",r#"printf '%s\n' '{"format":{"duration":"3.5"},"streams":[{"index":0,"codec_type":"video","width":100,"height":100}]}'"#),
        ffmpeg:script(root,"frames",&format!("printf '%s\\n' '[Parsed_showinfo_1 @ fixture] n: 0 pts: 1 pts_time:1.31 fmt:rgba' >&2\ncat '{}'",png.display())) }
}
#[cfg(unix)]
const READY: &str = r#"printf '%s\n' '{"type":"ready","protocol_version":1,"engine":"apple_vision","engine_version":{"helper_version":"2","vision_request_revision":3,"macos_version":"synthetic"},"settings":{"recognition_level":"accurate","uses_language_correction":false,"recognition_languages":["en-US"],"supported_languages":["en-US"]}}'"#;
#[cfg(unix)]
const FRAME: &str = r#"printf '%s\n' '{"type":"frame","frame_id":0,"observations":[{"text":"12 new paths","confidence":0.9,"bounds":{"x":0.1,"y":0.1,"width":0.2,"height":0.1}}]}'"#;

#[cfg(unix)]
#[test]
fn integration_ocr_preview_timeout_reaps_tools_and_allows_retry() {
    let root = TempRoot::new("ocr-preview-timeout");
    let path = root.path("saved.mp4");
    fs::write(&path, b"synthetic video").unwrap();
    let db = db();
    db.lock()
        .unwrap()
        .execute(
            "UPDATE history_entries SET output_path=?1 WHERE id='video'",
            [path.to_str().unwrap()],
        )
        .unwrap();
    let mut saved = result("Text");
    saved.segments[0].observed_at_ms = vec![1310];
    saved.source_sha256 = crate::hashing::sha256_from_path(path.to_str())
        .unwrap()
        .unwrap();
    ocr_store::begin(&db, "video", "preview").unwrap();
    ocr_store::finish(&db, "video", "preview", &saved).unwrap();
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    let selected = selection(&content, 0, 1310);
    let normal = tools(&root, "exit 0");
    let hang = script(&root, "hung-tool", "exec /bin/sleep 60");
    let preview = crate::ocr_preview::OcrPreviewState::with_timeout(Duration::from_secs(1));
    let processes = crate::process::ProcessState::default();
    for (extractor, probe) in [(&normal.ffmpeg, &hang), (&hang, &normal.ffprobe)] {
        let started = Instant::now();
        let error = preview
            .frame(&db, &processes, "video", &selected, extractor, probe)
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(6));
        assert!(processes.utility_children.lock().unwrap().is_empty());
        assert_eq!(
            ocr_store::get(&db, "video")
                .unwrap()
                .unwrap()
                .result_revision,
            content.result_revision
        );
    }
    assert!(preview
        .frame(
            &db,
            &processes,
            "video",
            &selected,
            &normal.ffmpeg,
            &normal.ffprobe
        )
        .unwrap()
        .starts_with("data:image/png;base64,"));
    assert!(processes.utility_children.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn integration_frame_processing_detects_changed_files_invalid_replies_and_early_exit() {
    let root = TempRoot::new("ocr-protocol");
    let path = root.path("saved.mp4");
    fs::write(&path, b"synthetic video").unwrap();
    let good = format!("{READY}\nread -r request\n{FRAME}\nwhile read -r request; do :; done");
    let updates = std::cell::RefCell::new(Vec::new());
    let result = ocr::analyze(
        &OcrState::default(),
        &path,
        &tools(&root, &good),
        |update| {
            updates.borrow_mut().push(update);
        },
    )
    .unwrap();
    assert_eq!(
        *updates.borrow(),
        vec![
            ocr::AnalysisProgress::Prepared {
                duration_ms: Some(3500)
            },
            ocr::AnalysisProgress::Frame {
                count: 1,
                observed_at_ms: 1310
            },
            ocr::AnalysisProgress::Finishing { count: 1 },
        ]
    );
    let settings: serde_json::Value = serde_json::from_str(&result.settings_json).unwrap();
    assert_eq!(settings["video_duration_ms"], 3500);
    assert_eq!(result.segments[0].observed_at_ms, vec![1310]);
    assert_eq!(
        result.source_sha256,
        crate::hashing::sha256_hex(b"synthetic video")
    );
    assert!(ocr::analyze(
        &OcrState::default(),
        &path,
        &tools(&root, &good),
        |update| {
            if matches!(update, ocr::AnalysisProgress::Frame { .. }) {
                fs::write(&path, b"changed source").unwrap();
            }
        }
    )
    .unwrap_err()
    .contains("changed during OCR"));
    for bad in [
        "printf 'not json\\n'".to_string(),
        format!("{READY}\nread -r request\nprintf '{{}}\\n'"),
        format!("{READY}\nexit 0"),
        format!("{READY}\nread -r request\n{FRAME}\nexit 23"),
    ] {
        assert!(ocr::analyze(&OcrState::default(), &path, &tools(&root, &bad), |_| {}).is_err());
    }
    let mut oversized_log = tools(&root, &good);
    oversized_log.ffmpeg = script(
        &root,
        "oversized-frame-log",
        "printf '%065540d\\n' 0 >&2\nsleep 30",
    );
    let started = Instant::now();
    assert!(
        ocr::analyze(&OcrState::default(), &path, &oversized_log, |_| {})
            .unwrap_err()
            .contains("oversized FFmpeg diagnostic")
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(ocr::validate_source(&root.path("missing.mp4")).is_err());
    assert!(ocr::validate_source(root.root()).is_err());
    let mut audio = tools(&root, &good);
    audio.ffprobe = script(
        &root,
        "audio-probe",
        r#"printf '%s\n' '{"streams":[{"index":0,"codec_type":"audio"}]}'"#,
    );
    assert!(ocr::analyze(&OcrState::default(), &path, &audio, |_| {})
        .unwrap_err()
        .contains("no usable video"));
}

#[cfg(target_os = "macos")]
#[test]
fn integration_ocr_cancellation_is_separate_from_download_processes_and_double_starts() {
    let root = TempRoot::new("ocr-cancel");
    let db = Arc::new(db());
    let path = root.path("saved.mp4");
    fs::write(&path, b"video").unwrap();
    db.lock()
        .unwrap()
        .execute(
            "UPDATE history_entries SET output_path=?1 WHERE id='video'",
            [path.to_str().unwrap()],
        )
        .unwrap();
    let state = Arc::new(OcrState::initialize(root.root(), &db).unwrap());
    let ready = root.path("ready");
    let helper = format!(
        "{READY}\nread -r request\nprintf ready > '{}'\nsleep 30",
        ready.display()
    );
    let tools = tools(&root, &helper);
    let (sender, receiver) = std::sync::mpsc::channel();
    let started = ocr::start(db.clone(), state.clone(), "video".into(), tools, move |p| {
        let _ = sender.send(p);
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let duplicate = ocr::start(
        db.clone(),
        state.clone(),
        "video".into(),
        self::tools(&root, &helper),
        |_| {},
    )
    .unwrap();
    assert_eq!(duplicate.attempt_id, started.attempt_id);
    assert_eq!(duplicate.history_id, started.history_id);
    let downloads = crate::process::ProcessState::default();
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("30");
    crate::process::configure_child_process_group(&mut command);
    let child = Arc::new(std::sync::Mutex::new(command.spawn().unwrap()));
    crate::process::register_current_child(&downloads, &child).unwrap();
    state.cancel(&db, "video", &started.attempt_id).unwrap();
    state.stop();
    assert!(child.lock().unwrap().try_wait().unwrap().is_none());
    let last = receiver.try_iter().last().unwrap();
    assert_eq!(last.status, "cancelled");
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .last_attempt_status,
        "cancelled"
    );
    crate::process::terminate_child_process_tree(&mut child.lock().unwrap()).unwrap();
    child.lock().unwrap().wait().unwrap();
}
