//! OCR-specific rule, storage, migration and isolated process regressions.
use crate::database::{self, Database, MigrationPoint};
use crate::models::{OcrProgress, OcrResult};
use crate::ocr::{self, OcrState, OcrTools};
use crate::ocr_rules::{self, FrameBuilder, Observation, OcrBox};
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
    let mut builder = FrameBuilder::default();
    if !text.is_empty() {
        builder.push(0, 3020, vec![observation(text, 0.1)]).unwrap();
    }
    OcrResult {
        frames: builder.frames,
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
        frame_id: content
            .frames
            .get(index)
            .filter(|frame| frame.timestamp_ms == time)
            .or_else(|| {
                content
                    .frames
                    .iter()
                    .find(|frame| frame.timestamp_ms == time)
            })
            .unwrap()
            .id
            .clone(),
    }
}

#[test]
fn deleting_one_frame_removes_all_its_blocks_but_preserves_other_times_and_rejects_stale_edits() {
    let db = db();
    let mut saved = result("Repeated text");
    let duplicate = saved.frames[0].blocks[0].clone();
    saved.frames[0].blocks.push(duplicate);
    saved.frames[0].full_text = ocr_rules::frame_text(&saved.frames[0].blocks);
    let mut later = saved.frames[0].clone();
    later.id = "later-frame".into();
    later.timestamp_ms = 4030;
    later.frame_key = Some(1);
    saved.frames.push(later);
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &saved).unwrap();
    let original = ocr_store::get(&db, "video").unwrap().unwrap();
    let stale = selection(&original, 0, 3020);
    let mut invalid = stale.clone();
    invalid.frame_id = "missing".into();
    assert!(ocr_store::delete_frame(&db, "video", &invalid).is_err());
    ocr_store::begin(&db, "video", "rerun").unwrap();
    assert!(ocr_store::delete_frame(&db, "video", &stale)
        .unwrap_err()
        .contains("finish"));
    ocr_store::fail(&db, "video", "rerun", "cancelled", "Cancelled").unwrap();
    db.lock().unwrap().execute_batch("CREATE TRIGGER reject_ocr_delete BEFORE UPDATE OF result_revision ON history_ocr BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;").unwrap();
    assert!(ocr_store::delete_frame(&db, "video", &stale).is_err());
    assert_eq!(
        ocr_store::get(&db, "video").unwrap().unwrap().frames,
        original.frames
    );
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .result_revision,
        original.result_revision
    );
    db.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_ocr_delete")
        .unwrap();
    let edited = ocr_store::delete_frame(&db, "video", &stale).unwrap();
    assert_eq!(edited.frames.len(), 1);
    assert_eq!(edited.frames[0].id, "later-frame");
    assert_eq!(
        ocr_store::export_text(&db, "video", None).unwrap(),
        "[00:00:04.030]\nRepeated text\nRepeated text"
    );
    assert_eq!(edited.source_sha256, original.source_sha256);
    assert_eq!(edited.result_created_at, original.result_created_at);
    assert_eq!(edited.engine_version, original.engine_version);
    assert_ne!(edited.result_revision, original.result_revision);
    assert!(ocr_store::delete_frame(&db, "video", &stale)
        .unwrap_err()
        .contains("changed"));
    assert!(ocr_store::export_text(&db, "video", stale.result_revision.as_str().into()).is_err());
    assert!(ocr_store::page(&db, "video", 100, 0, "", Some(&stale.result_revision)).is_err());
    let empty = ocr_store::delete_frame(&db, "video", &selection(&edited, 0, 4030)).unwrap();
    assert_eq!(empty.total_count, 0);
    assert!(empty.frames.is_empty());
    assert_eq!(ocr_store::export_text(&db, "video", None).unwrap(), "");
    assert_eq!(empty.last_attempt_status, "cancelled");
}

#[test]
fn successful_replacement_is_atomic_and_invalid_or_oversized_frames_preserve_previous_results() {
    let db = db();
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &result("Previous text")).unwrap();
    let previous = ocr_store::get(&db, "video").unwrap().unwrap();
    ocr_store::begin(&db, "video", "retry").unwrap();
    db.lock().unwrap().execute_batch("CREATE TRIGGER reject_frame_insert BEFORE INSERT ON history_ocr_frames BEGIN SELECT RAISE(ABORT, 'synthetic disk failure'); END;").unwrap();
    assert!(ocr_store::finish(&db, "video", "retry", &result("New text")).is_err());
    assert_eq!(
        ocr_store::get(&db, "video").unwrap().unwrap().frames,
        previous.frames
    );
    assert_eq!(
        ocr_store::get(&db, "video")
            .unwrap()
            .unwrap()
            .result_revision,
        previous.result_revision
    );
    db.lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_frame_insert")
        .unwrap();
    let mut invalid = result("Good text");
    invalid.frames[0].full_text = "Invented text".into();
    assert!(ocr_store::finish(&db, "video", "retry", &invalid)
        .unwrap_err()
        .contains("blocks"));
    let mut huge = result("x");
    huge.frames = result("text").frames;
    huge.frames[0].blocks[0].text = "x".repeat(ocr_rules::MAX_RESULT_BYTES);
    huge.frames[0].full_text = huge.frames[0].blocks[0].text.clone();
    assert!(ocr_store::finish(&db, "video", "retry", &huge)
        .unwrap_err()
        .contains("32 MiB"));
    assert_eq!(
        ocr_store::get(&db, "video").unwrap().unwrap().frames,
        previous.frames
    );
    ocr_store::finish(&db, "video", "retry", &result("New text")).unwrap();
    let next = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_ne!(next.frames[0].id, previous.frames[0].id);
    assert_ne!(next.result_revision, previous.result_revision);
    assert!(ocr_store::selected_time(&db, "video", &selection(&previous, 0, 3020)).is_err());
    assert_eq!(
        ocr_store::export_text(&db, "video", None).unwrap(),
        "[00:00:03.020]\nNew text"
    );
}

#[test]
fn integration_deleted_frame_text_survives_reopening_schema_four() {
    let root = TempRoot::new("ocr-delete-store");
    let path = root.path("history.sqlite");
    let db = Database::new(database::open(&path).unwrap());
    db.lock().unwrap().execute("INSERT INTO history_entries (id,url,created_at) VALUES ('video','https://example.com/video',1)", []).unwrap();
    ocr_store::begin(&db, "video", "first").unwrap();
    ocr_store::finish(&db, "video", "first", &result("Unwanted OCR text")).unwrap();
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    let deleted = ocr_store::delete_frame(&db, "video", &selection(&content, 0, 3020)).unwrap();
    drop(db);
    let reopened = Database::new(database::open(&path).unwrap());
    let content = ocr_store::get(&reopened, "video").unwrap().unwrap();
    assert!(content.frames.is_empty());
    assert_eq!(content.total_count, 0);
    assert_eq!(content.result_revision, deleted.result_revision);
    assert_eq!(
        ocr_store::export_text(&reopened, "video", None).unwrap(),
        ""
    );
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
    saved.frames = [0, 1120, 2400, 3520]
        .into_iter()
        .enumerate()
        .map(|(index, time)| {
            let mut frame = saved.frames[0].clone();
            frame.id = format!("frame-{index}");
            frame.timestamp_ms = time;
            frame.frame_key = Some(index as u64);
            frame
        })
        .collect();
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
    ocr_store::delete_frame(&db, "video", &stale).unwrap();
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
fn short_ocr_text_is_filtered_per_block_using_unicode_without_rewriting_original_text() {
    let mut builder = FrameBuilder::default();
    builder
        .push(
            0,
            1000,
            [
                "",
                " \n\t ",
                "A",
                "AB",
                "ABC",
                " äöü ",
                "😀😁😂",
                " \t A \n B \t ",
            ]
            .into_iter()
            .map(|text| observation(text, 0.1))
            .collect(),
        )
        .unwrap();
    assert!(builder.frames.is_empty());
    builder
        .push(
            1,
            2000,
            ["ABCD", " äöüß ", "😀😁😂😃", "  Text \t hier\nZeile  "]
                .into_iter()
                .map(|text| observation(text, 0.1))
                .collect(),
        )
        .unwrap();
    builder
        .push(2, 3000, vec![observation("ABC", 0.1)])
        .unwrap();
    builder
        .push(3, 4000, vec![observation("ABCD", 0.1)])
        .unwrap();
    assert_eq!(builder.frames.len(), 2);
    assert_eq!(builder.frames[0].frame_key, Some(1));
    assert_eq!(
        builder.frames[0].full_text,
        "ABCD\n äöüß \n😀😁😂😃\n  Text \t hier\nZeile  "
    );
    let db = db();
    let mut saved = result("");
    saved.frames = builder.frames;
    ocr_store::begin(&db, "video", "filtered").unwrap();
    ocr_store::finish(&db, "video", "filtered", &saved).unwrap();
    let stored = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(stored.frames.len(), 2);
    assert_eq!(stored.frames[0].full_text, saved.frames[0].full_text);
    assert_eq!(stored.frames[1].timestamp_ms, 4000);
}

#[test]
fn one_frame_keeps_all_blocks_in_geometric_order_and_different_observation_times_remain_separate() {
    let mut builder = FrameBuilder::default();
    let mut upper_right = observation("Right text\nSecond line", 0.6);
    upper_right.bounds.y = 0.81;
    let mut upper_left = observation("Same text", 0.1);
    upper_left.bounds.y = 0.8;
    let lower = observation("Same text", 0.1);
    builder
        .push(
            12,
            8020,
            vec![lower.clone(), upper_right.clone(), upper_left.clone()],
        )
        .unwrap();
    builder
        .push(13, 8030, vec![lower, upper_right, upper_left])
        .unwrap();
    builder.push(14, 9000, vec![]).unwrap();
    assert_eq!(builder.frames.len(), 2);
    assert_eq!(
        builder.frames[0].full_text,
        "Same text\nRight text\nSecond line\nSame text"
    );
    assert_ne!(builder.frames[0].id, builder.frames[1].id);
    assert_eq!(builder.frames[0].blocks.len(), 3);
    assert_eq!(builder.frames[0].blocks[0].confidence, Some(0.9));
    assert_eq!(builder.frames[0].blocks[0].metadata_source, "native_frame");
    assert!(builder.frames[0].blocks[0].legacy_segment_bounds.is_none());
    let db = db();
    let mut saved = result("");
    saved.frames = builder.frames;
    ocr_store::begin(&db, "video", "frames").unwrap();
    ocr_store::finish(&db, "video", "frames", &saved).unwrap();
    let stored = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(stored.total_count, 2);
    assert_eq!(stored.frames[0].timestamp_ms, 8020);
    assert_eq!(stored.frames[1].timestamp_ms, 8030);
    assert_eq!(
        ocr_store::export_text(&db, "video", None).unwrap(),
        ocr_rules::readable_text(&saved.frames)
    );
    let (cache, json): (Option<String>, Option<String>) = db
        .lock()
        .unwrap()
        .query_row("SELECT text,segments_json FROM history_ocr", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(cache.is_none() && json.is_none());
}

#[test]
fn sqlite_pages_search_unloaded_frames_and_export_every_frame_in_stable_time_order() {
    let db = db();
    let mut builder = FrameBuilder::default();
    // Two hours of repeated captions must remain distinct without a video JSON.
    for index in 0..7201 {
        let text = if index == 7150 {
            "Grüße %_ am Ende".into()
        } else {
            format!("Repeated caption {index}")
        };
        builder
            .push(index, index * 1000, vec![observation(&text, 0.1)])
            .unwrap();
    }
    let mut saved = result("");
    saved.frames = builder.frames;
    // SQL's id tie-breaker remains deterministic for equal precise timestamps.
    saved.frames[1].timestamp_ms = 0;
    saved.frames[0].id = "tie-a".into();
    saved.frames[1].id = "tie-b".into();
    ocr_store::begin(&db, "video", "long").unwrap();
    ocr_store::finish(&db, "video", "long", &saved).unwrap();
    let first = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(first.frames.len(), 100);
    assert_eq!(first.total_count, 7201);
    assert_eq!(first.matched_count, 7201);
    assert_eq!(first.frames[0].id, "tie-a");
    assert_eq!(first.frames[1].id, "tie-b");
    assert!(first.has_more);
    assert!(first.text.is_none());
    let revision = first.result_revision.as_deref();
    let next = ocr_store::page(&db, "video", 100, 100, "", revision)
        .unwrap()
        .unwrap();
    assert_eq!(next.frames[0].timestamp_ms, 100000);
    assert_eq!(next.offset, 100);
    let found = ocr_store::page(&db, "video", 100, 0, "GRÜẞE %_", revision)
        .unwrap()
        .unwrap();
    assert_eq!(found.total_count, 7201);
    assert_eq!(found.matched_count, 1);
    assert_eq!(found.frames[0].timestamp_ms, 7150000);
    assert!(!found.has_more);
    assert_eq!(
        ocr_store::selected_time(&db, "video", &selection(&found, 0, 7150000)).unwrap(),
        7150000
    );
    let exported = ocr_store::export_text(&db, "video", revision).unwrap();
    assert!(exported
        .starts_with("[00:00:00.000]\nRepeated caption 0\n\n[00:00:00.000]\nRepeated caption 1"));
    assert!(exported.contains("[01:59:10.000]\nGrüße %_ am Ende"));
    assert!(exported.ends_with("[02:00:00.000]\nRepeated caption 7200"));
    assert_eq!(exported.matches("]\n").count(), 7201);
    for (limit, offset) in [(0, 0), (101, 0), (100, u64::MAX)] {
        assert!(ocr_store::page(&db, "video", limit, offset, "", revision).is_err());
    }
    let deleted = ocr_store::delete_frame(&db, "video", &selection(&found, 0, 7150000)).unwrap();
    assert_eq!(deleted.total_count, 7200);
    assert!(!ocr_store::export_text(&db, "video", None)
        .unwrap()
        .contains("Grüße"));
    assert_eq!(
        ocr_store::page(&db, "video", 100, 0, "GRÜẞE", None)
            .unwrap()
            .unwrap()
            .matched_count,
        0
    );
}

#[test]
fn repeated_frame_storage_hits_the_existing_limit_before_exceeding_it() {
    let mut builder = FrameBuilder::default();
    let text = "A repeated caption with line breaks\n".repeat(60);
    let mut index = 0;
    loop {
        let before = builder.frames.len();
        match builder.push(index, index * 1000, vec![observation(&text, 0.1)]) {
            Ok(()) => index += 1,
            Err(error) => {
                assert!(error.contains("32 MiB"));
                assert_eq!(builder.frames.len(), before);
                assert!(index > 1000 && index < 10000);
                break;
            }
        }
    }
    let size: usize = builder
        .frames
        .iter()
        .map(|frame| ocr_rules::frame_storage_bytes(frame).unwrap())
        .sum();
    assert!(size <= ocr_rules::MAX_RESULT_BYTES);
}

fn schema_three(path: &std::path::Path) -> Connection {
    let conn = database::open(path).unwrap();
    conn.execute_batch("DROP TABLE history_ocr_frames;
        ALTER TABLE history_ocr DROP COLUMN result_revision;
        ALTER TABLE history_ocr DROP COLUMN result_format;
        PRAGMA user_version=3;
        INSERT INTO history_entries (id,url,created_at) VALUES ('video','https://example.com/video',1);").unwrap();
    conn
}

#[test]
fn integration_frame_migration_preserves_json_with_high_precision_legacy_bounds() {
    let root = TempRoot::new("ocr-frame-precision");
    let path = root.path("state.sqlite");
    let conn = schema_three(&path);
    // This valid coordinate changes by one ULP if the default JSON parser reads
    // its shortest serialized form a second time. SQLite stores the JSON intact.
    let json = r#"[{"text":"Original text\nGrüße","observed_at_ms":[8020,8030],"bounds":{"x":0.90090990900909906,"y":0.1,"width":0.05,"height":0.04}}]"#;
    let segments: Vec<ocr_rules::OcrSegment> = serde_json::from_str(json).unwrap();
    let expected = serde_json::to_string(&vec![ocr_rules::OcrBlock {
        text: segments[0].text.clone(),
        bounds: None,
        confidence: None,
        metadata_source: "legacy_segment".into(),
        legacy_segment_bounds: segments[0].bounds.clone(),
    }])
    .unwrap();
    conn.execute("INSERT INTO history_ocr (history_entry_id,text,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at) VALUES ('video','old export',?1,123,'old','completed',1)",[json]).unwrap();
    drop(conn);

    let db = Database::new(database::open(&path).unwrap());
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(content.total_count, 2);
    assert_eq!(
        content
            .frames
            .iter()
            .map(|f| f.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![8020, 8030]
    );
    for frame in &content.frames {
        assert_eq!(frame.full_text, "Original text\nGrüße");
        let stored: String = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT blocks_json FROM history_ocr_frames WHERE id=?1",
                [&frame.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, expected);
    }
    assert!(db.lock().unwrap().query_row("SELECT segments_json IS NULL AND text IS NULL FROM history_ocr WHERE history_entry_id='video'", [], |r| r.get::<_,bool>(0)).unwrap());
    drop(db);
    let reopened = Database::new(database::open(&path).unwrap());
    let restarted = ocr_store::get(&reopened, "video").unwrap().unwrap();
    assert_eq!(restarted.frames, content.frames);
    assert_eq!(restarted.result_revision, content.result_revision);
}

#[test]
fn integration_schema_three_migrates_all_exact_legacy_times_without_inventing_frame_metadata() {
    let root = TempRoot::new("ocr-frame-migration");
    let path = root.path("state.sqlite");
    let conn = schema_three(&path);
    let segment = |text: &str, times: Vec<u64>, x: f64| ocr_rules::OcrSegment {
        text: text.into(),
        observed_at_ms: times,
        bounds: Some(observation(text, x).bounds),
    };
    let segments = vec![
        segment("Same word", vec![8020, 8030, 10000], 0.1),
        segment("Other block", vec![8020, 9000], 0.4),
        segment("Same word", vec![8020], 0.7),
    ];
    conn.execute("INSERT INTO history_ocr (history_entry_id,text,segments_json,source_sha256,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at)
        VALUES ('video','old grouped export',?1,'legacy-hash',123,'legacy','error',456)", [serde_json::to_string(&segments).unwrap()]).unwrap();
    for (id, json, text) in [
        ("plain", None, Some("[00:00:08] uncertain legacy text")),
        ("empty", Some("[]"), Some("")),
        (
            "untimed",
            Some(
                r#"[{"text":"Unknown time","observed_at_ms":[],"bounds":{"x":0.1,"y":0.1,"width":0.2,"height":0.1}}]"#,
            ),
            Some("Unknown time"),
        ),
    ] {
        conn.execute("INSERT INTO history_entries (id,url,created_at) VALUES (?1,'https://example.com/video',1)", [id]).unwrap();
        conn.execute("INSERT INTO history_ocr (history_entry_id,text,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at)
            VALUES (?1,?2,?3,123,'legacy','completed',456)", rusqlite::params![id,text,json]).unwrap();
    }
    conn.execute("INSERT INTO history_entries (id,url,created_at) VALUES ('missing-metadata','https://example.com/video',1)",[]).unwrap();
    conn.execute_batch("INSERT INTO history_ocr (history_entry_id,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at) VALUES ('missing-metadata','[{\"text\":\"Saved without geometry\",\"observed_at_ms\":[8077]},{\"text\":\"Saved without a timestamp\"}]',123,'legacy','completed',456)").unwrap();
    drop(conn);
    let db = Database::new(database::open(&path).unwrap());
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(
        content
            .frames
            .iter()
            .map(|frame| frame.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![8020, 8030, 9000, 10000]
    );
    assert_eq!(
        content.frames[0].full_text,
        "Same word\nOther block\nSame word"
    );
    assert_eq!(content.frames[1].full_text, "Same word");
    assert_eq!(content.frames[2].full_text, "Other block");
    assert!(content.frames.iter().all(|frame| frame.frame_key.is_none()));
    assert_eq!(content.last_attempt_status, "error");
    assert_eq!(content.result_created_at, Some(123));
    let blocks: String = db
        .lock()
        .unwrap()
        .query_row(
            "SELECT blocks_json FROM history_ocr_frames WHERE id=?1",
            [&content.frames[0].id],
            |r| r.get(0),
        )
        .unwrap();
    let blocks: Vec<ocr_rules::OcrBlock> = serde_json::from_str(&blocks).unwrap();
    assert_eq!(blocks.len(), 3);
    assert!(blocks.iter().all(|block| block.bounds.is_none()
        && block.confidence.is_none()
        && block.metadata_source == "legacy_segment"
        && block.legacy_segment_bounds.is_some()));
    assert!(db.lock().unwrap().query_row("SELECT segments_json IS NULL AND text IS NULL FROM history_ocr WHERE history_entry_id='video'", [], |r| r.get::<_,bool>(0)).unwrap());
    let plain = ocr_store::get(&db, "plain").unwrap().unwrap();
    assert!(plain.frames.is_empty());
    assert_eq!(plain.result_format.as_deref(), Some("legacy_text"));
    assert_eq!(
        ocr_store::export_text(&db, "plain", None).unwrap(),
        "[00:00:08] uncertain legacy text"
    );
    assert!(ocr_store::get(&db, "empty")
        .unwrap()
        .unwrap()
        .frames
        .is_empty());
    assert_eq!(ocr_store::export_text(&db, "empty", None).unwrap(), "");
    assert_eq!(
        ocr_store::export_text(&db, "untimed", None).unwrap(),
        "Unknown time"
    );
    let incomplete = ocr_store::get(&db, "missing-metadata").unwrap().unwrap();
    assert_eq!(incomplete.frames.len(), 1);
    assert_eq!(incomplete.frames[0].timestamp_ms, 8077);
    assert!(incomplete.frames[0].frame_key.is_none());
    assert_eq!(
        incomplete.text.as_deref(),
        Some("Saved without a timestamp")
    );
    let unknown: String = db
        .lock()
        .unwrap()
        .query_row(
            "SELECT blocks_json FROM history_ocr_frames WHERE id=?1",
            [&incomplete.frames[0].id],
            |r| r.get(0),
        )
        .unwrap();
    let unknown: Vec<ocr_rules::OcrBlock> = serde_json::from_str(&unknown).unwrap();
    assert!(
        unknown[0].bounds.is_none()
            && unknown[0].confidence.is_none()
            && unknown[0].legacy_segment_bounds.is_none()
    );
    assert_eq!(ocr_store::export_text(&db,"missing-metadata",None).unwrap(),"[00:00:08.077]\nSaved without geometry\n\n[Unmapped legacy text]\nSaved without a timestamp");
    let first_id = content.frames[0].id.clone();
    let deleted = ocr_store::delete_frame(&db, "video", &selection(&content, 0, 8020)).unwrap();
    drop(db);
    let db = Database::new(database::open(&path).unwrap());
    let restarted = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(restarted.frames, deleted.frames);
    assert_eq!(restarted.result_revision, deleted.result_revision);
    assert!(!restarted.frames.iter().any(|frame| frame.id == first_id));
    db.lock()
        .unwrap()
        .execute("DELETE FROM history_entries WHERE id='video'", [])
        .unwrap();
    assert_eq!(
        db.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM history_ocr_frames WHERE history_entry_id='video'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn integration_frame_migration_rolls_back_source_retirement_and_can_retry_without_duplicates() {
    let root = TempRoot::new("ocr-frame-rollback");
    let path = root.path("state.sqlite");
    let conn = schema_three(&path);
    let json = serde_json::to_string(&vec![ocr_rules::OcrSegment {
        text: "Retained text".into(),
        observed_at_ms: vec![1000, 2000],
        bounds: Some(observation("text", 0.1).bounds),
    }])
    .unwrap();
    conn.execute("INSERT INTO history_ocr (history_entry_id,text,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at) VALUES ('video','old export',?1,123,'old','completed',1)",[&json]).unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    assert!(
        database::open_with_hook(&path, |point| if point == MigrationPoint::OcrFrames {
            Err("synthetic migration interruption".into())
        } else {
            Ok(())
        })
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row("SELECT segments_json FROM history_ocr", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        json
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='history_ocr_frames'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    let db = Database::new(database::open(&path).unwrap());
    let migrated = ocr_store::get(&db, "video").unwrap().unwrap();
    assert_eq!(migrated.total_count, 2);
    drop(db);
    let reopened = Database::new(database::open(&path).unwrap());
    assert_eq!(
        ocr_store::get(&reopened, "video").unwrap().unwrap().frames,
        migrated.frames
    );
}

#[test]
fn frame_migration_rejects_changed_stored_payloads_without_retiring_source_data() {
    for column in ["full_text", "blocks_json"] {
        let db = db();
        let conn = db.lock().unwrap();
        let json = r#"[{"text":"Retained text","observed_at_ms":[8020],"bounds":{"x":0.90090990900909906,"y":0.1,"width":0.05,"height":0.04}}]"#;
        conn.execute("INSERT INTO history_ocr (history_entry_id,text,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at) VALUES ('video','old export',?1,123,'old','completed',1)", [json]).unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER corrupt_frame AFTER INSERT ON history_ocr_frames BEGIN
             UPDATE history_ocr_frames SET {column}='changed' WHERE id=NEW.id;
             END;"
        ))
        .unwrap();
        let tx = database::write_transaction(&conn).unwrap();
        assert_eq!(
            ocr_store::migrate_frames(&tx).unwrap_err(),
            "Migrated OCR frame verification failed."
        );
        drop(tx);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM history_ocr_frames", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let source: (String, String, Option<String>) = conn.query_row(
            "SELECT text,segments_json,result_revision FROM history_ocr WHERE history_entry_id='video'", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))
        ).unwrap();
        assert_eq!(source, ("old export".into(), json.into(), None));
    }
}

#[test]
fn integration_invalid_legacy_ocr_aborts_upgrade_and_preserves_the_original_data() {
    let root = TempRoot::new("ocr-frame-invalid-migration");
    let path = root.path("state.sqlite");
    let conn = schema_three(&path);
    conn.execute_batch("INSERT INTO history_ocr (history_entry_id,text,segments_json,result_created_at,last_attempt_id,last_attempt_status,last_attempt_at) VALUES ('video','Readable fallback','broken JSON',123,'old','completed',1)").unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    assert!(database::open(&path)
        .unwrap_err()
        .contains("Source data preserved"));
    assert_eq!(fs::read(&path).unwrap(), before);
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
    assert!(empty.text.is_none());
    assert!(empty.frames.is_empty());
    assert_eq!(empty.total_count, 0);
    assert_eq!(ocr_store::export_text(&db, "video", None).unwrap(), "");
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
        assert_eq!(current.frames, saved.frames);
        assert_eq!(current.result_revision, saved.result_revision);
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
fn integration_saved_ocr_and_history_summary_survive_reopening_schema_four() {
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
    assert_eq!(content.frames, saved.frames);
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
    ocr_store::finish(&db, "video", "old", &result("Old result")).unwrap();
    let state = OcrState::default();
    ocr_store::begin(&db, "video", "cancel").unwrap();
    ocr::test_active(&state, progress("cancel"));
    state.cancel(&db, "video", "cancel").unwrap();
    assert_eq!(
        ocr::finalize(&db, &state, Ok(result("New result")), || Ok(()))
            .unwrap()
            .status,
        "cancelled"
    );
    assert!(ocr_store::export_text(&db, "video", None)
        .unwrap()
        .ends_with("Old result"));
    let state = OcrState::default();
    ocr_store::begin(&db, "video", "disk").unwrap();
    ocr::test_active(&state, progress("disk"));
    db.lock().unwrap().execute_batch("CREATE TRIGGER fail_ocr BEFORE UPDATE OF result_revision ON history_ocr BEGIN SELECT RAISE(ABORT,'synthetic disk failure'); END;").unwrap();
    let event = ocr::finalize(&db, &state, Ok(result("New result")), || Ok(())).unwrap();
    assert_eq!(event.status, "error");
    assert!(event.message.unwrap().contains("synthetic disk failure"));
    assert!(ocr_store::export_text(&db, "video", None)
        .unwrap()
        .ends_with("Old result"));
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
    conn.execute_batch("DROP TABLE history_ocr_frames; DROP TABLE history_ocr; PRAGMA user_version=2;
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
        4
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
    conn.pragma_update(None, "user_version", 5).unwrap();
    drop(conn);
    let bytes = fs::read(&path).unwrap();
    assert!(database::open(&path)
        .unwrap_err()
        .contains("schema version 5"));
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
    saved.frames[0].timestamp_ms = 1310;
    saved.source_sha256 = crate::hashing::sha256_from_path(path.to_str())
        .unwrap()
        .unwrap();
    ocr_store::begin(&db, "video", "preview").unwrap();
    ocr_store::finish(&db, "video", "preview", &saved).unwrap();
    let content = ocr_store::get(&db, "video").unwrap().unwrap();
    let selected = selection(&content, 0, 1310);
    let normal = tools(&root, "exit 0");
    let hang = script(&root, "hung-tool", "exec /bin/sleep 60");
    // Allow concurrent test process startup while still bounding stalled tools.
    let preview = crate::ocr_preview::OcrPreviewState::with_timeout(Duration::from_secs(3));
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
    assert_eq!(settings["minimum_text_characters"], 4);
    assert_eq!(result.frames[0].timestamp_ms, 1310);
    assert_eq!(result.frames[0].frame_key, Some(0));
    assert_eq!(result.frames.len(), 1);
    assert_eq!(result.frames[0].full_text, "12 new paths");
    assert_eq!(result.frames[0].blocks[0].confidence, Some(0.9));
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
