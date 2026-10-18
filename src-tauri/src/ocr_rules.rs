//! Frame-local reading order and native protocol/media parsing, without I/O.
use serde::{Deserialize, Serialize};

pub(crate) const SAMPLE_INTERVAL_SECONDS: f64 = 1.0;
pub(crate) const MIN_TEXT_CHARACTERS: usize = 4;
pub(crate) const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;

/// One chronological pass. Compare against the last kept text, but advance the
/// continuity cursor even when deleting a duplicate so long runs stay connected.
#[derive(Default)]
pub(crate) struct DuplicateRun {
    kept_text: String,
    previous: Option<(u64, Option<u64>)>,
}

impl DuplicateRun {
    pub(crate) fn push(&mut self, time: u64, key: Option<u64>, text: &str) -> bool {
        if text.split_whitespace().next().is_none() {
            self.previous = None;
            self.kept_text.clear();
            return false;
        }
        let consecutive = self.previous.is_some_and(|(previous_time, previous_key)| {
            let Some(gap) = time.checked_sub(previous_time).filter(|gap| *gap > 0) else {
                return false;
            };
            match (previous_key, key) {
                // Allow modest timestamp variation, never bridge a missing sample.
                (Some(previous), Some(current)) => {
                    previous.checked_add(1) == Some(current)
                        && gap as f64 <= SAMPLE_INTERVAL_SECONDS * 1500.0
                }
                // Unknown legacy identity: at most one sampling interval.
                (None, None) => gap as f64 <= SAMPLE_INTERVAL_SECONDS * 1000.0,
                _ => false,
            }
        });
        let duplicate = consecutive
            && self
                .kept_text
                .split_whitespace()
                .eq(text.split_whitespace());
        self.previous = Some((time, key));
        if !duplicate {
            self.kept_text.clear();
            self.kept_text.push_str(text);
        }
        duplicate
    }
}

pub(crate) fn validate_ready(ready: &serde_json::Value) -> Result<(), String> {
    let settings = &ready["settings"];
    let languages = settings["recognition_languages"].as_array();
    let supported = settings["supported_languages"].as_array();
    if ready["type"] != "ready"
        || ready["protocol_version"] != 1
        || ready["engine"] != "apple_vision"
        || ready["engine_version"]["vision_request_revision"]
            .as_u64()
            .unwrap_or(0)
            == 0
        || ready["engine_version"]["macos_version"]
            .as_str()
            .is_none_or(str::is_empty)
        || ready["engine_version"]["helper_version"].as_str() != Some("2")
        || settings["recognition_level"] != "accurate"
        || settings["uses_language_correction"] != false
        || languages.is_none_or(Vec::is_empty)
        || supported.is_none_or(Vec::is_empty)
        || languages.into_iter().flatten().any(|language| {
            language.as_str().is_none()
                || !supported.is_some_and(|values| values.contains(language))
        })
    {
        return Err("Invalid OCR helper startup response or recognition settings.".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OcrBox {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    pub(crate) text: String,
    pub(crate) bounds: OcrBox,
    pub(crate) confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct OcrSegment {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) observed_at_ms: Vec<u64>,
    // Read-only legacy format. Older imports may lack geometry entirely.
    #[serde(default)]
    pub(crate) bounds: Option<OcrBox>,
}

#[derive(Default)]
pub(crate) struct FrameBuilder {
    pub(crate) frames: Vec<OcrFrame>,
    bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct OcrBlock {
    // Preserve the delivered text, including internal line breaks.
    pub(crate) text: String,
    pub(crate) bounds: Option<OcrBox>,
    pub(crate) confidence: Option<f64>,
    pub(crate) metadata_source: String,
    // Legacy runs only retained their first box. Never claim it is this frame's box.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) legacy_segment_bounds: Option<OcrBox>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OcrFrame {
    pub(crate) id: String,
    pub(crate) timestamp_ms: u64,
    pub(crate) frame_key: Option<u64>,
    pub(crate) full_text: String,
    pub(crate) blocks: Vec<OcrBlock>,
}

// Vision coordinates have a bottom-left origin. Top edges anchor approximate
// text lines; within a line, x determines order. This is a geometric heuristic,
// not semantic reading order. Stable sorting preserves equal-position repeats.
pub(crate) fn order_blocks(blocks: &mut [OcrBlock]) {
    fn bounds(block: &OcrBlock) -> Option<&OcrBox> {
        block
            .bounds
            .as_ref()
            .or(block.legacy_segment_bounds.as_ref())
    }
    blocks.sort_by(|a, b| match (bounds(a), bounds(b)) {
        (Some(a), Some(b)) => (b.y + b.height).total_cmp(&(a.y + a.height)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    let mut start = 0;
    while start < blocks.len() {
        let Some(anchor) = bounds(&blocks[start]).cloned() else {
            break;
        };
        let mut end = start + 1;
        while end < blocks.len() {
            let Some(next) = bounds(&blocks[end]) else {
                break;
            };
            let overlap =
                (anchor.y + anchor.height).min(next.y + next.height) - anchor.y.max(next.y);
            if overlap < anchor.height.min(next.height) * 0.5 {
                break;
            }
            end += 1;
        }
        blocks[start..end].sort_by(|a, b| bounds(a).unwrap().x.total_cmp(&bounds(b).unwrap().x));
        start = end;
    }
}

pub(crate) fn frame_text(blocks: &[OcrBlock]) -> String {
    blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn frame_storage_bytes(frame: &OcrFrame) -> Result<usize, String> {
    Ok(serde_json::to_string(&frame.blocks)
        .map_err(|e| e.to_string())?
        .len()
        .saturating_add(frame.full_text.len())
        .saturating_add(256))
}

impl FrameBuilder {
    pub(crate) fn push(
        &mut self,
        frame_key: u64,
        time: u64,
        observations: Vec<Observation>,
    ) -> Result<(), String> {
        let mut blocks: Vec<_> = observations
            .into_iter()
            .filter(|o| {
                o.text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(MIN_TEXT_CHARACTERS)
                    .count()
                    >= MIN_TEXT_CHARACTERS
            })
            .map(|o| OcrBlock {
                text: o.text,
                bounds: Some(o.bounds),
                confidence: Some(o.confidence),
                metadata_source: "native_frame".into(),
                legacy_segment_bounds: None,
            })
            .collect();
        if blocks.is_empty() {
            return Ok(());
        }
        order_blocks(&mut blocks);
        let frame = OcrFrame {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp_ms: time,
            frame_key: Some(frame_key),
            full_text: frame_text(&blocks),
            blocks,
        };
        let bytes = self.bytes.saturating_add(frame_storage_bytes(&frame)?);
        if bytes > MAX_RESULT_BYTES {
            return Err("OCR result exceeds the 32 MiB limit.".into());
        }
        self.bytes = bytes;
        self.frames.push(frame);
        Ok(())
    }
}

pub(crate) fn validate_observations(
    value: &serde_json::Value,
    frame_id: u64,
) -> Result<Vec<Observation>, String> {
    if value.get("type").and_then(|v| v.as_str()) != Some("frame")
        || value.get("frame_id").and_then(|v| v.as_u64()) != Some(frame_id)
    {
        return Err("Invalid OCR helper frame response.".into());
    }
    let observations: Vec<Observation> = serde_json::from_value(
        value
            .get("observations")
            .cloned()
            .ok_or("OCR observations missing.")?,
    )
    .map_err(|_| "Invalid OCR observations.")?;
    if observations.len() > 10000
        || observations.iter().any(|o| {
            let b = &o.bounds;
            o.text.len() > 65536
                || !o.confidence.is_finite()
                || !(0.0..=1.0).contains(&o.confidence)
                || [b.x, b.y, b.width, b.height]
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || b.width == 0.0
                || b.height == 0.0
                || b.x + b.width > 1.001
                || b.y + b.height > 1.001
        })
    {
        return Err("Invalid OCR text or bounding box.".into());
    }
    Ok(observations)
}

pub(crate) fn parse_frame_time(line: &str) -> Result<Option<(u64, u64)>, String> {
    if !line.contains("Parsed_showinfo_") || !line.contains("pts_time:") {
        return Ok(None);
    }
    let number = line
        .split(" n:")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Invalid extracted frame number.")?;
    let seconds = line
        .split("pts_time:")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("Invalid extracted media timestamp.")?;
    if !seconds.is_finite() || seconds < 0.0 || seconds > u64::MAX as f64 / 1000.0 {
        return Err("Invalid extracted media timestamp.".into());
    }
    Ok(Some((number, (seconds * 1000.0).round() as u64)))
}

pub(crate) fn video_stream_index(bytes: &[u8]) -> Result<u64, String> {
    let probe: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid video probe response.")?;
    // FFmpeg can expose text (tty/ansi), still images and playlists as video.
    // A video extension alone is insufficient, including for renamed TXT files.
    let supported_container = probe
        .pointer("/format/format_name")
        .and_then(|v| v.as_str())
        .is_some_and(|formats| {
            formats.split(',').any(|format| {
                matches!(
                    format,
                    "matroska"
                        | "webm"
                        | "mov"
                        | "mp4"
                        | "avi"
                        | "asf"
                        | "flv"
                        | "live_flv"
                        | "mpeg"
                        | "mpegvideo"
                        | "mpegts"
                        | "m4v"
                        | "ogg"
                        | "rm"
                        | "nut"
                )
            })
        });
    if !supported_container {
        return Err("OCR requires a supported video container. Transcript, audio and image files cannot be used as videos.".into());
    }
    probe.get("streams").and_then(|v| v.as_array()).into_iter().flatten()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video")
            && s.pointer("/disposition/attached_pic").and_then(|v| v.as_u64()).unwrap_or(0) == 0
            && s.get("width").and_then(|v| v.as_u64()).unwrap_or(0) > 0
            && s.get("height").and_then(|v| v.as_u64()).unwrap_or(0) > 0)
        .and_then(|s| s.get("index").and_then(|v| v.as_u64()))
        .ok_or_else(|| "The saved file has no usable video track. Audio and transcript files cannot be checked for OCR.".into())
}

// Use the local file's duration, including cuts; remote History metadata may
// describe a different timeline. Missing/invalid duration leaves progress open.
pub(crate) fn video_duration_ms(bytes: &[u8], index: u64) -> Option<u64> {
    let probe: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let parse = |value: &serde_json::Value| {
        let seconds = value
            .as_f64()
            .or_else(|| value.as_str()?.parse::<f64>().ok())?;
        seconds.is_finite().then_some(seconds)
    };
    let origin = probe
        .pointer("/format/start_time")
        .and_then(parse)
        .unwrap_or(0.0);
    let stream = probe["streams"].as_array().and_then(|streams| {
        streams
            .iter()
            .find(|stream| stream["index"].as_u64() == Some(index))
    });
    let seconds = stream
        .and_then(|stream| {
            let offset =
                (stream.get("start_time").and_then(parse).unwrap_or(origin) - origin).max(0.0);
            stream
                .get("duration")
                .and_then(parse)
                .filter(|v| *v > 0.0)
                .map(|v| v + offset)
        })
        // Matroska DURATION tags contain an end timestamp, including the
        // original offset. FFmpeg -start_at_zero removes that same origin.
        .or_else(|| {
            let tags = stream?.get("tags")?;
            let tag = tags
                .get("DURATION")
                .or_else(|| tags.get("duration"))?
                .as_str()?;
            let parts: Vec<_> = tag.split(':').collect();
            if parts.len() != 3 {
                return None;
            }
            let hours = parts[0].parse::<u64>().ok()?;
            let minutes = parts[1].parse::<u32>().ok()?;
            let seconds = parts[2].parse::<f64>().ok()?;
            if minutes >= 60 || !(0.0..60.0).contains(&seconds) {
                return None;
            }
            let duration = hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds - origin;
            (duration > 0.0).then_some(duration)
        })
        .or_else(|| {
            let mut duration = probe.pointer("/format/duration").and_then(parse)?;
            let matroska = probe
                .pointer("/format/format_name")
                .and_then(|v| v.as_str())
                .is_some_and(|formats| {
                    formats.split(',').any(|f| matches!(f, "matroska" | "webm"))
                });
            if matroska {
                duration -= origin;
            }
            Some(duration)
        })?;
    let millis = seconds * 1000.0;
    (millis.is_finite() && millis >= 1.0 && millis < u64::MAX as f64).then(|| millis.round() as u64)
}

pub(crate) fn estimated_frame_count(duration_ms: u64) -> u64 {
    ((duration_ms as f64 / (SAMPLE_INTERVAL_SECONDS * 1000.0)).ceil() as u64).max(1)
}

pub(crate) fn timestamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
pub(crate) fn readable_text(frames: &[OcrFrame]) -> String {
    frames
        .iter()
        .map(|frame| format!("[{}]\n{}", timestamp(frame.timestamp_ms), frame.full_text))
        .collect::<Vec<_>>()
        .join("\n\n")
}
