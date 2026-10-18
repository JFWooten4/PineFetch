//! Resolve and validate local OCR video sources without opening transcript text.
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    "webm", "mkv", "mp4", "mov", "m4v", "avi", "wmv", "asf", "flv", "f4v", "mpg", "mpeg", "mpe",
    "m1v", "m2v", "ts", "mts", "m2ts", "vob", "ogv", "3gp", "3g2", "rm", "rmvb", "nut",
];

pub(crate) const NO_VIDEO: &str = "No downloaded video is available for OCR. Download the video or enable 'Keep video with transcript' and download it again.";
const INVALID_EXTENSION: &str = "OCR requires a downloaded video file with a supported video extension. Transcript, subtitle, audio and image files cannot be used.";

pub(crate) fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

pub(crate) fn validate(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("The saved video path is not an unambiguous absolute path.".into());
    }
    // Reject text/audio/images before opening, hashing or passing them to a tool.
    if !has_video_extension(path) {
        return Err(INVALID_EXTENSION.into());
    }
    let metadata = fs::metadata(path).map_err(|_| "The saved video is missing or unreadable.")?;
    if !metadata.is_file() {
        return Err("The saved path is not a readable video file.".into());
    }
    File::open(path).map_err(|_| "The saved video is unreadable.")?;
    Ok(())
}

pub(crate) fn resolve(output: &Path, is_transcript: bool) -> Result<PathBuf, String> {
    if !output.is_absolute() {
        return Err("The saved video path is not an unambiguous absolute path.".into());
    }
    if has_video_extension(output) {
        validate(output)?;
        return Ok(output.to_path_buf());
    }
    if !is_transcript
        || !output
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        return Err(INVALID_EXTENSION.into());
    }
    // Transcription publishes media_path.with_extension("txt"). Only the exact
    // same stem in that directory is eligible; never strip suffixes or pick a
    // similarly named file. A deleted transcript does not invalidate its video.
    let mut video = None;
    for entry in fs::read_dir(output.parent().ok_or(NO_VIDEO)?).map_err(|_| NO_VIDEO)? {
        let path = entry
            .map_err(|_| "The saved video directory could not be read.")?
            .path();
        if path.file_stem() != output.file_stem() || !has_video_extension(&path) || !path.is_file()
        {
            continue;
        }
        if video.is_some() {
            return Err("Several downloaded videos match this transcript. Open the video's own History entry or download it again before using OCR.".into());
        }
        video = Some(path);
    }
    let video = video.ok_or(NO_VIDEO)?;
    validate(&video)?;
    Ok(video)
}
