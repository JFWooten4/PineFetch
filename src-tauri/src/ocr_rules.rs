//! Conservative grouping and native protocol/media parsing, without I/O.
use serde::{Deserialize, Serialize};

pub(crate) const SAMPLE_INTERVAL_SECONDS: f64 = 1.0;
pub(crate) const MIN_TEXT_CHARACTERS: usize = 4;
pub(crate) const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;

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
    pub(crate) observed_at_ms: Vec<u64>,
    // Vision normalized coordinates, origin bottom-left, in the oriented full frame.
    pub(crate) bounds: OcrBox,
}

#[derive(Default)]
pub(crate) struct SegmentBuilder {
    pub(crate) segments: Vec<OcrSegment>,
    previous: Vec<usize>,
    bytes: usize,
}

impl SegmentBuilder {
    pub(crate) fn push(
        &mut self,
        time: u64,
        mut observations: Vec<Observation>,
    ) -> Result<(), String> {
        // Reading order within a frame: top to bottom, then left to right.
        observations.sort_by(|a, b| {
            b.bounds
                .y
                .total_cmp(&a.bounds.y)
                .then(a.bounds.x.total_cmp(&b.bounds.x))
        });
        let mut next = Vec::new();
        for observation in observations {
            let text = observation
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.chars().take(MIN_TEXT_CHARACTERS).count() < MIN_TEXT_CHARACTERS {
                continue;
            }
            let found = self.previous.iter().copied().find(|index| {
                !next.contains(index)
                    && self.segments[*index].text == text
                    && comparable_position(&self.segments[*index].bounds, &observation.bounds)
            });
            self.bytes += if found.is_some() { 8 } else { text.len() + 128 };
            if self.bytes > MAX_RESULT_BYTES {
                return Err("OCR result exceeds the 32 MiB limit.".into());
            }
            let index = if let Some(index) = found {
                self.segments[index].observed_at_ms.push(time);
                index
            } else {
                self.segments.push(OcrSegment {
                    text,
                    observed_at_ms: vec![time],
                    bounds: observation.bounds,
                });
                self.segments.len() - 1
            };
            next.push(index);
        }
        self.previous = next;
        Ok(())
    }
}

fn comparable_position(a: &OcrBox, b: &OcrBox) -> bool {
    let intersection = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let vertical = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    let overlap = intersection.max(0.0) * vertical.max(0.0);
    let union = a.width * a.height + b.width * b.height - overlap;
    union > 0.0 && overlap / union >= 0.6
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

pub(crate) fn readable_text(segments: &[OcrSegment]) -> String {
    segments
        .iter()
        .map(|s| {
            format!(
                "[{}] {}",
                s.observed_at_ms
                    .iter()
                    .map(|t| timestamp(*t))
                    .collect::<Vec<_>>()
                    .join(", "),
                s.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
