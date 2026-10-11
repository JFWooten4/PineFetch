//! On-demand saved-video frames. Preview processes never share OCR cancellation.
use crate::database::Database;
use crate::models::OcrSelection;
use crate::ocr::{fingerprint, validate_source, Fingerprint};
use crate::ocr_store;
use crate::process::{self, ProcessState};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{atomic::Ordering, Mutex};
use std::time::Duration;

struct VerifiedSource {
    path: PathBuf,
    fingerprint: Fingerprint,
    hash: String,
}

#[derive(Default)]
pub(crate) struct OcrPreviewState {
    // Serialize extraction and cache only a verified file identity, never frames.
    source: Mutex<Option<VerifiedSource>>,
}

pub(crate) fn frame_command(tool: &Path, path: &Path, index: u64, time_ms: u64) -> Command {
    let mut command = Command::new(tool);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "info", "-ss"])
        .arg(format!("{:.3}", time_ms.saturating_sub(500) as f64 / 1000.0))
        .args(["-copyts", "-start_at_zero", "-i"]).arg(path)
        .arg("-map").arg(format!("0:{index}"))
        .arg("-vf").arg(format!(
            "select='gte(t,{:.4})',showinfo,scale='min(1280,iw)':'min(720,ih)':force_original_aspect_ratio=decrease,setsar=1",
            (time_ms as f64 / 1000.0 - 0.0005).max(0.0)))
        .args(["-frames:v", "1", "-fps_mode", "passthrough", "-an", "-sn", "-threads", "1", "-c:v", "png", "-f", "image2pipe", "pipe:1"]);
    command
}

impl OcrPreviewState {
    pub(crate) fn frame(
        &self,
        db: &Database,
        processes: &ProcessState,
        id: &str,
        selection: &OcrSelection,
        ffmpeg: &Path,
        ffprobe: &Path,
    ) -> Result<String, String> {
        let mut cached = self.source.lock().map_err(|_| "Preview lock poisoned")?;
        let content = ocr_store::get(db, id)?.ok_or("No saved OCR result is available.")?;
        ocr_store::validate_selection(&content, selection)?;
        let hash = content
            .source_sha256
            .ok_or("The analyzed source identity is unavailable.")?;
        let saved = ocr_store::saved_path(db, id)?;
        let path = Path::new(&saved);
        validate_source(path)?;
        let before = fingerprint(path)?;
        if !cached.as_ref().is_some_and(|source| {
            source.path == path && source.fingerprint == before && source.hash == hash
        }) {
            let mut file = File::open(path).map_err(|e| e.to_string())?;
            let mut digest = Sha256::new();
            let mut bytes = [0u8; 65536];
            loop {
                if processes.shutting_down.load(Ordering::SeqCst) {
                    return Err("Application is shutting down.".into());
                }
                let count = file.read(&mut bytes).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                digest.update(&bytes[..count]);
            }
            if crate::hashing::bytes_to_hex(&digest.finalize()) != hash
                || fingerprint(path)? != before
            {
                return Err("The saved video changed since OCR analysis. Run OCR again.".into());
            }
            *cached = Some(VerifiedSource {
                path: path.to_path_buf(),
                fingerprint: before.clone(),
                hash,
            });
        }
        let mut probe = Command::new(ffprobe);
        probe
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(path);
        let probe = process::run_command_output(
            probe,
            None,
            Some(processes),
            Some(Duration::from_secs(30)),
        )
        .map_err(String::from)?;
        if !probe.status.success() {
            return Err("The saved video could not be opened.".into());
        }
        let index = crate::ocr_rules::video_stream_index(&probe.stdout)?;
        let output = process::run_command_output(
            frame_command(ffmpeg, path, index, selection.observed_at_ms),
            None,
            Some(processes),
            Some(Duration::from_secs(30)),
        )
        .map_err(String::from)?;
        let time = String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| crate::ocr_rules::parse_frame_time(line).ok().flatten())
            .map(|(_, time)| time);
        if !output.status.success()
            || time != Some(selection.observed_at_ms)
            || !output.stdout.starts_with(b"\x89PNG\r\n\x1a\n")
            || output.stdout.len() > 4 * 1024 * 1024
        {
            return Err("The selected video frame could not be extracted.".into());
        }
        if fingerprint(path)? != before || ocr_store::saved_path(db, id)? != saved {
            return Err("The saved video changed while loading its frame.".into());
        }
        let current = ocr_store::get(db, id)?.ok_or("The OCR result is no longer available.")?;
        ocr_store::validate_selection(&current, selection)?;
        Ok(format!(
            "data:image/png;base64,{}",
            STANDARD.encode(output.stdout)
        ))
    }
}
