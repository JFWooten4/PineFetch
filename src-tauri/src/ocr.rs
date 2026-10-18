//! One local OCR run, with its own process registry and cancellation boundary.
use crate::database::Database;
use crate::models::OcrProgress;
use crate::models::OcrResult;
use crate::ocr_rules::{self, FrameBuilder};
use crate::ocr_store;
use crate::process::{self, ProcessState};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{atomic::Ordering, mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_PROTOCOL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FRAME_LOG_LINE_BYTES: u64 = 65536;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct OcrState {
    active: Mutex<Option<OcrProgress>>,
    processes: ProcessState,
    worker: Mutex<Option<JoinHandle<()>>>,
    owner: Option<OcrOwner>,
}

struct OcrOwner(File);
impl Drop for OcrOwner {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            // Explicit unlock also releases descriptors inherited by a fork
            // that has not yet exec'ed. The owning state outlives its worker.
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = &self.0;
    }
}

impl Default for OcrState {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            processes: ProcessState::default(),
            worker: Mutex::new(None),
            owner: None,
        }
    }
}

impl OcrState {
    pub(crate) fn initialize(directory: &Path, db: &Database) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        let mut state = Self::default();
        #[cfg(not(target_os = "macos"))]
        let state = Self::default();
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::OpenOptionsExt;
            // Keep the inode and lock for the lifetime of the app/worker. Never
            // delete this lockfile: deleting it would let a second inode be locked.
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(directory.join("ocr-owner.lock"))
                .map_err(|e| format!("OCR ownership lock failed: {e}"))?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                state.owner = Some(OcrOwner(file));
                ocr_store::interrupt_orphans(db)?;
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(format!("OCR ownership lock failed: {error}"));
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (directory, db);
        Ok(state)
    }

    pub(crate) fn progress(&self, id: &str) -> Result<Option<OcrProgress>, String> {
        Ok(self
            .active
            .lock()
            .map_err(|_| "OCR state lock poisoned")?
            .as_ref()
            .filter(|p| p.history_id == id)
            .cloned())
    }

    pub(crate) fn cancel(&self, db: &Database, id: &str, attempt: &str) -> Result<(), String> {
        let mut active = self.active.lock().map_err(|_| "OCR state lock poisoned")?;
        let current = active
            .as_mut()
            .filter(|p| p.history_id == id && p.attempt_id == attempt)
            .ok_or("This OCR attempt is no longer running.")?;
        // The same lock covers the final commit: a cancellation accepted before
        // completion prevents both replacement and a completed event.
        self.processes.shutting_down.store(true, Ordering::SeqCst);
        current.status = "cancelled".into();
        terminate_children(&self.processes);
        ocr_store::fail(db, id, attempt, "cancelled", "OCR was cancelled.")
    }

    pub(crate) fn stop(&self) {
        self.processes.shutting_down.store(true, Ordering::SeqCst);
        terminate_children(&self.processes);
        if let Ok(mut slot) = self.worker.lock() {
            if let Some(worker) = slot.take() {
                let _ = worker.join();
            }
        }
    }
}

fn terminate_children(state: &ProcessState) {
    if let Ok(children) = state.utility_children.lock() {
        for child in children.iter() {
            if let Ok(mut child) = child.lock() {
                let _ = process::terminate_child_process_tree(&mut child);
            }
        }
    }
}

pub(crate) struct OcrTools {
    pub(crate) helper: PathBuf,
    pub(crate) ffmpeg: PathBuf,
    pub(crate) ffprobe: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AnalysisProgress {
    Prepared { duration_ms: Option<u64> },
    Frame { count: u64, observed_at_ms: u64 },
    Finishing { count: u64 },
}

pub(crate) fn start(
    db: Arc<Database>,
    state: Arc<OcrState>,
    id: String,
    tools: OcrTools,
    emit: impl Fn(OcrProgress) + Send + 'static,
) -> Result<OcrProgress, String> {
    if !cfg!(target_os = "macos") {
        return Err("Apple Vision OCR is available on macOS only.".into());
    }
    if state.owner.is_none() {
        return Err("OCR is owned by another PineFetch instance. Close that instance before starting an analysis here.".into());
    }
    // Reopening or clicking Start twice returns the existing attempt, including
    // while source preparation is running. The lock also guards job creation.
    let mut active = state.active.lock().map_err(|_| "OCR state lock poisoned")?;
    if let Some(current) = active.as_ref() {
        if current.history_id == id && current.status == "running" {
            return Ok(current.clone());
        }
        return Err(
            "An OCR analysis is already running. Cancel it or wait for it to finish.".into(),
        );
    }
    let path = PathBuf::from(ocr_store::saved_path(&db, &id)?);
    validate_source(&path)?;
    let progress = OcrProgress {
        history_id: id,
        attempt_id: uuid::Uuid::new_v4().to_string(),
        attempt_at: crate::files::current_timestamp_millis(),
        status: "running".into(),
        frames_checked: 0,
        observed_at_ms: None,
        total_duration_ms: None,
        total_frames: None,
        total_frames_is_estimate: false,
        message: None,
    };
    state.processes.shutting_down.store(false, Ordering::SeqCst);
    ocr_store::begin(&db, &progress.history_id, &progress.attempt_id)?;
    *active = Some(progress.clone());
    let worker_state = state.clone();
    let worker_progress = progress.clone();
    let worker_db = db.clone();
    let worker = thread::Builder::new()
        .name("pinefetch-ocr".into())
        .spawn(move || {
            emit(worker_progress.clone());
            let identity = fingerprint(&path);
            let result = analyze(&worker_state, &path, &tools, |update| {
                if let Ok(mut active) = worker_state.active.lock() {
                    if let Some(p) = active.as_mut().filter(|p| p.status == "running") {
                        match update {
                            AnalysisProgress::Prepared { duration_ms } => {
                                p.total_duration_ms = duration_ms;
                                p.total_frames = duration_ms.map(ocr_rules::estimated_frame_count);
                                p.total_frames_is_estimate = p.total_frames.is_some();
                            }
                            AnalysisProgress::Frame {
                                count,
                                observed_at_ms,
                            } => {
                                p.frames_checked = count;
                                p.observed_at_ms = Some(observed_at_ms);
                                p.total_frames = p.total_frames.map(|total| total.max(count));
                            }
                            AnalysisProgress::Finishing { count } => {
                                p.total_frames = Some(count);
                                p.total_frames_is_estimate = false;
                            }
                        }
                        emit(p.clone());
                    }
                }
            });
            let final_event = finalize(&worker_db, &worker_state, result, || {
                if fingerprint(&path)? != identity? {
                    return Err(
                        "The saved video changed before the OCR result could be saved.".into(),
                    );
                }
                if Path::new(&ocr_store::saved_path(
                    &worker_db,
                    &worker_progress.history_id,
                )?) != path
                {
                    return Err("The history entry's saved video path changed during OCR.".into());
                }
                Ok(())
            });
            if let Some(event) = final_event {
                emit(event);
            }
        });
    match worker {
        Ok(worker) => {
            *state
                .worker
                .lock()
                .map_err(|_| "OCR worker lock poisoned")? = Some(worker);
        }
        Err(error) => {
            let message = format!("OCR worker could not start: {error}");
            ocr_store::fail(
                &db,
                &progress.history_id,
                &progress.attempt_id,
                "error",
                &message,
            )?;
            *active = None;
            return Err(message);
        }
    }
    Ok(progress)
}

pub(super) fn finalize(
    db: &Database,
    state: &OcrState,
    result: Result<OcrResult, String>,
    verify_source: impl FnOnce() -> Result<(), String>,
) -> Option<OcrProgress> {
    let mut active = state.active.lock().ok()?;
    let current = active.as_mut()?;
    let stopped = state.processes.shutting_down.load(Ordering::SeqCst);
    let outcome = if stopped {
        Err(if current.status == "cancelled" {
            "OCR was cancelled.".into()
        } else {
            "OCR was interrupted when the app stopped.".into()
        })
    } else {
        result.and_then(|result| {
            verify_source()?;
            ocr_store::finish(db, &current.history_id, &current.attempt_id, &result)
        })
    };
    match outcome {
        Ok(()) => current.status = "completed".into(),
        Err(error) => {
            current.status = if current.status == "cancelled" {
                "cancelled"
            } else if stopped {
                "interrupted"
            } else {
                "error"
            }
            .into();
            let saved = ocr_store::fail(
                db,
                &current.history_id,
                &current.attempt_id,
                &current.status,
                &error,
            );
            current.message = Some(match saved {
                Ok(()) => error,
                Err(e) => format!("{error} Last attempt status could not be saved: {e}"),
            });
        }
    }
    active.take()
}

#[cfg(test)]
pub(super) fn test_active(state: &OcrState, progress: OcrProgress) {
    *state.active.lock().unwrap() = Some(progress);
}

fn check_cancel(state: &OcrState) -> Result<(), String> {
    if state.processes.shutting_down.load(Ordering::SeqCst) {
        Err("OCR was stopped.".into())
    } else {
        Ok(())
    }
}

pub(crate) use crate::ocr_source::validate as validate_source;

#[derive(PartialEq, Debug, Clone)]
pub(crate) struct Fingerprint {
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let m = fs::metadata(path).map_err(|e| format!("OCR source changed or disappeared: {e}"))?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Fingerprint {
        size: m.len(),
        modified: m.modified().map_err(|e| e.to_string())?,
        #[cfg(unix)]
        identity: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
    })
}

fn hash_source(state: &OcrState, path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("OCR source is unreadable: {e}"))?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        check_cancel(state)?;
        let count = file
            .read(&mut bytes)
            .map_err(|e| format!("OCR source hash failed: {e}"))?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    Ok(crate::hashing::bytes_to_hex(&digest.finalize()))
}

struct ManagedChild<'a> {
    child: Arc<Mutex<Child>>,
    state: &'a OcrState,
}
impl<'a> ManagedChild<'a> {
    fn spawn(
        state: &'a OcrState,
        mut command: Command,
    ) -> Result<
        (
            Self,
            std::process::ChildStdout,
            std::process::ChildStderr,
            Option<ChildStdin>,
        ),
        String,
    > {
        check_cancel(state)?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        process::configure_child_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| format!("OCR process could not start: {e}"))?;
        let stdout = child.stdout.take().ok_or("OCR stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("OCR stderr unavailable")?;
        let stdin = child.stdin.take();
        let managed = Self {
            child: Arc::new(Mutex::new(child)),
            state,
        };
        process::register_utility_child(&state.processes, &managed.child)?;
        check_cancel(state)?;
        Ok((managed, stdout, stderr, stdin))
    }
    fn wait(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            check_cancel(self.state)?;
            if let Some(status) = self
                .child
                .lock()
                .map_err(|_| "OCR child lock poisoned")?
                .try_wait()
                .map_err(|e| e.to_string())?
            {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("OCR process ended with {status}."))
                };
            }
            if Instant::now() >= deadline {
                return Err("OCR process did not exit after its output closed.".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for ManagedChild<'_> {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = process::terminate_child_process_tree(&mut child);
            let _ = child.wait();
        }
        process::clear_utility_child(&self.state.processes, &self.child);
    }
}

fn protocol_reader(
    reader: impl Read + Send + 'static,
) -> mpsc::Receiver<Result<serde_json::Value, String>> {
    let (tx, rx) = mpsc::sync_channel(2);
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            let count = (&mut reader)
                .take(MAX_PROTOCOL_BYTES + 1)
                .read_until(b'\n', &mut line);
            let response = match count {
                Ok(0) => break,
                Ok(n) if n as u64 <= MAX_PROTOCOL_BYTES && line.last() == Some(&b'\n') => {
                    serde_json::from_slice(&line)
                        .map_err(|_| "Invalid JSON from OCR helper.".into())
                }
                _ => Err("Invalid or oversized OCR helper response.".into()),
            };
            let failed = response.is_err();
            if tx.send(response).is_err() || failed {
                break;
            }
        }
    });
    rx
}

fn receive<T>(state: &OcrState, receiver: &mpsc::Receiver<Result<T, String>>) -> Result<T, String> {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        check_cancel(state)?;
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(value) => return value,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("OCR process ended before returning all required responses.".into())
            }
            Err(_) if Instant::now() >= deadline => {
                return Err("OCR process response timed out.".into())
            }
            Err(_) => {}
        }
    }
}

fn diagnostic_reader(reader: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let log = Arc::new(Mutex::new(String::new()));
    let output = log.clone();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        let mut reader = reader;
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            if let Ok(mut log) = output.lock() {
                if log.len() < 65536 {
                    log.push_str(&String::from_utf8_lossy(&buffer[..count]));
                }
            }
        }
    });
    log
}

struct Workspace {
    path: PathBuf,
    identity: Fingerprint,
}
impl Workspace {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("pinefetch-ocr-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|e| format!("OCR temporary directory could not be created: {e}"))?;
        let identity = fingerprint(&path)?;
        Ok(Self { path, identity })
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        // Directory mtime changes as frames are created; compare only its inode.
        #[cfg(unix)]
        if let Ok(current) = fingerprint(&self.path) {
            if current.identity.0 == self.identity.identity.0
                && current.identity.1 == self.identity.identity.1
                && fs::symlink_metadata(&self.path).is_ok_and(|m| !m.file_type().is_symlink())
            {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.path, &self.identity);
        }
    }
}

// image2pipe preserves selected timestamps without output CFR duplication.
pub(crate) fn frame_command(tool: &Path, path: &Path, index: u64) -> Command {
    let mut command = Command::new(tool);
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "info",
            "-copyts",
            "-start_at_zero",
            "-i",
        ])
        .arg(path)
        .arg("-map")
        .arg(format!("0:{index}"))
        .arg("-vf")
        .arg(format!(
            "select='isnan(prev_selected_t)+gte(t-prev_selected_t,{})',showinfo",
            ocr_rules::SAMPLE_INTERVAL_SECONDS
        ))
        .args([
            "-fps_mode",
            "passthrough",
            "-an",
            "-sn",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
    command
}

fn read_png(reader: &mut impl Read) -> Result<Option<Vec<u8>>, String> {
    let mut signature = [0; 8];
    let read = reader
        .read(&mut signature[..1])
        .map_err(|e| e.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut signature[1..])
        .map_err(|_| "Truncated extracted frame.")?;
    if signature != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("Invalid extracted PNG frame.".into());
    }
    let mut bytes = signature.to_vec();
    loop {
        let mut header = [0; 8];
        reader
            .read_exact(&mut header)
            .map_err(|_| "Truncated extracted frame.")?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        if bytes.len().saturating_add(size).saturating_add(12) > MAX_FRAME_BYTES {
            return Err("Extracted frame exceeds the 128 MiB limit.".into());
        }
        bytes.extend(header);
        let start = bytes.len();
        bytes.resize(start + size + 4, 0);
        reader
            .read_exact(&mut bytes[start..])
            .map_err(|_| "Truncated extracted frame.")?;
        if &header[4..] == b"IEND" {
            return Ok(Some(bytes));
        }
    }
}

pub(crate) fn analyze(
    state: &OcrState,
    path: &Path,
    tools: &OcrTools,
    progress: impl Fn(AnalysisProgress),
) -> Result<OcrResult, String> {
    validate_source(path)?;
    let before = fingerprint(path)?;
    let hash = hash_source(state, path)?;
    if fingerprint(path)? != before {
        return Err("The saved video changed while it was being hashed.".into());
    }
    let mut probe = Command::new(&tools.ffprobe);
    probe
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path);
    let probe = process::run_command_output(
        probe,
        None,
        Some(&state.processes),
        Some(Duration::from_secs(30)),
    )
    .map_err(String::from)?;
    check_cancel(state)?;
    if !probe.status.success() {
        return Err("The saved file could not be opened as a video.".into());
    }
    let index = ocr_rules::video_stream_index(&probe.stdout)?;
    let duration_ms = ocr_rules::video_duration_ms(&probe.stdout, index);
    let mut helper_command = Command::new(&tools.helper);
    helper_command.stdin(Stdio::piped());
    let (helper, output, errors, stdin) = ManagedChild::spawn(state, helper_command)?;
    let diagnostics = diagnostic_reader(errors);
    let responses = protocol_reader(output);
    let ready = receive(state, &responses).map_err(|e| {
        format!(
            "{e} {}",
            diagnostics.lock().map(|s| s.clone()).unwrap_or_default()
        )
    })?;
    ocr_rules::validate_ready(&ready)?;
    progress(AnalysisProgress::Prepared { duration_ms });
    let workspace = Workspace::new()?;
    let frame_path = workspace.path.join("frame.png");
    let (extractor, output, errors, _) =
        ManagedChild::spawn(state, frame_command(&tools.ffmpeg, path, index))?;
    let (time_tx, times) = mpsc::sync_channel(2);
    let extractor_child = extractor.child.clone();
    let stderr_thread = thread::spawn(move || {
        let mut reader = BufReader::new(errors);
        loop {
            let mut line = Vec::new();
            let count = (&mut reader)
                .take(MAX_FRAME_LOG_LINE_BYTES + 1)
                .read_until(b'\n', &mut line);
            let timestamp = match count {
                Ok(0) => return Ok(()),
                Ok(n) if n as u64 <= MAX_FRAME_LOG_LINE_BYTES => {
                    ocr_rules::parse_frame_time(&String::from_utf8_lossy(&line))
                }
                _ => Err("Invalid or oversized FFmpeg diagnostic line.".into()),
            };
            match timestamp {
                Ok(Some(time)) => {
                    if time_tx.send(Ok(time)).is_err() {
                        return Ok(());
                    }
                }
                Err(error) => {
                    // Stop the pipe producer before reporting: otherwise an
                    // oversized metadata line could block PNG reading forever.
                    if let Ok(mut child) = extractor_child.lock() {
                        let _ = process::terminate_child_process_tree(&mut child);
                    }
                    let _ = time_tx.send(Err(error.clone()));
                    return Err(error);
                }
                _ => {}
            }
        }
    });
    let mut frames = BufReader::new(output);
    let mut stdin = stdin.ok_or("OCR helper stdin unavailable")?;
    let mut builder = FrameBuilder::default();
    let mut frame_id = 0;
    let mut previous_time = None;
    while let Some(png) = read_png(&mut frames)? {
        check_cancel(state)?;
        let (number, time) = receive(state, &times)?;
        if number != frame_id || previous_time.is_some_and(|previous| time <= previous) {
            return Err("Extracted frame timestamps are inconsistent.".into());
        }
        fs::write(&frame_path, &png).map_err(|e| format!("OCR frame could not be saved: {e}"))?;
        drop(png);
        serde_json::to_writer(
            &mut stdin,
            &serde_json::json!({"frame_id":frame_id,"path":frame_path}),
        )
        .map_err(|e| format!("OCR helper request failed: {e}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|e| e.to_string())?;
        let response = receive(state, &responses).map_err(|error| {
            format!(
                "{error} {}",
                diagnostics
                    .lock()
                    .map(|log| log.clone())
                    .unwrap_or_default()
            )
        })?;
        let observations = ocr_rules::validate_observations(&response, frame_id)?;
        fs::remove_file(&frame_path)
            .map_err(|e| format!("OCR temporary frame cleanup failed: {e}"))?;
        builder.push(frame_id, time, observations)?;
        frame_id += 1;
        previous_time = Some(time);
        progress(AnalysisProgress::Frame {
            count: frame_id,
            observed_at_ms: time,
        });
    }
    let extraction_result = extractor.wait();
    if extraction_result.is_err() {
        if let Ok(mut child) = extractor.child.lock() {
            let _ = process::terminate_child_process_tree(&mut child);
        }
    }
    drop(times);
    stderr_thread
        .join()
        .map_err(|_| "OCR timestamp reader failed.")??;
    extraction_result?;
    if frame_id == 0 {
        return Err("No usable video frames could be extracted.".into());
    }
    drop(stdin);
    helper.wait().map_err(|error| {
        format!(
            "{error} {}",
            diagnostics
                .lock()
                .map(|log| log.clone())
                .unwrap_or_default()
        )
    })?;
    // Extra or incomplete replies are protocol errors, including zero-exit helpers.
    match responses.recv_timeout(Duration::from_secs(2)) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        _ => return Err("OCR helper returned unexpected trailing output.".into()),
    }
    progress(AnalysisProgress::Finishing { count: frame_id });
    if fingerprint(path)? != before
        || hash_source(state, path)? != hash
        || fingerprint(path)? != before
    {
        return Err("The saved video changed during OCR. The previous result was kept.".into());
    }
    let mut settings = ready["settings"].clone();
    settings["sample_interval_seconds"] = serde_json::json!(ocr_rules::SAMPLE_INTERVAL_SECONDS);
    settings["minimum_text_characters"] = serde_json::json!(ocr_rules::MIN_TEXT_CHARACTERS);
    settings["sampling"] = serde_json::json!("first frame, then first media PTS at least one second after previous selection; local file timeline");
    settings["frames_checked"] = serde_json::json!(frame_id);
    settings["result_model"] = serde_json::json!("frames");
    settings["reading_order"] =
        serde_json::json!("top-edge line heuristic, then left-to-right; original text");
    settings["video_duration_ms"] = serde_json::json!(duration_ms);
    // Success includes cleanup. An unexpected remaining file or filesystem
    // failure must not silently leave a successful attempt's temporary output.
    fs::remove_dir(&workspace.path)
        .map_err(|e| format!("OCR temporary directory cleanup failed: {e}"))?;
    Ok(OcrResult {
        frames: builder.frames,
        source_sha256: hash,
        engine_version: ready["engine_version"].to_string(),
        settings_json: settings.to_string(),
    })
}
