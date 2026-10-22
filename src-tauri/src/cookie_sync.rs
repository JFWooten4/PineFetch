use crate::files::OwnedTemporaryFile;
use crate::platform::Platform;
use crate::process::ProcessState;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub(crate) struct CookieSyncState(Mutex<Session>);
#[derive(Default)]
struct Session {
    waiting_since: Option<Instant>,
    cookies: Option<Arc<CookieData>>,
    fallback_browser: Option<String>,
}
struct CookieData {
    contents: String,
    count: usize,
    expires_at: Option<f64>,
}
pub(crate) struct SyncedCookies {
    file: OwnedTemporaryFile,
}
impl Drop for SyncedCookies {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = std::fs::remove_file(&self.file.path);
    }
}
#[derive(Serialize)]
pub(crate) struct Status {
    pub(crate) state: &'static str,
    pub(crate) cookie_count: usize,
    pub(crate) fallback_browser: Option<String>,
    pub(crate) source: &'static str,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserCookie {
    domain: String,
    path: String,
    name: String,
    value: String,
    secure: bool,
    #[serde(default)]
    http_only: bool,
    #[serde(default)]
    host_only: bool,
    expiration_date: Option<f64>,
}
impl Session {
    fn status(&mut self) -> Status {
        if self.cookies.as_ref().is_some_and(|cookies| {
            cookies.expires_at.is_some_and(|expiry| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(true, |now| now.as_secs_f64() >= expiry)
            })
        }) {
            self.cookies = None;
        }
        if self
            .waiting_since
            .is_some_and(|time| time.elapsed() > Duration::from_secs(300))
        {
            self.waiting_since = None;
        }
        Status {
            state: if self.waiting_since.is_some() {
                "waiting"
            } else if self.cookies.is_some() {
                "ready"
            } else if self.fallback_browser.is_some() {
                "reader"
            } else {
                "off"
            },
            cookie_count: self.cookies.as_ref().map_or(0, |cookies| cookies.count),
            fallback_browser: self.fallback_browser.clone(),
            source: if self.cookies.is_some() {
                "extension"
            } else if self.fallback_browser.is_some() {
                "browser"
            } else {
                "none"
            },
        }
    }
}
impl CookieSyncState {
    pub(crate) fn status(&self) -> Result<Status, String> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "Cookie sync unavailable")?
            .status())
    }
    pub(crate) fn begin(&self, browser: Option<String>) -> Result<Status, String> {
        if browser
            .as_deref()
            .is_some_and(|browser| !supported_browser(browser))
        {
            return Err("Choose a supported fallback browser".into());
        }
        let mut session = self.0.lock().map_err(|_| "Cookie sync unavailable")?;
        session.waiting_since = Some(Instant::now());
        session.fallback_browser = browser;
        Ok(session.status())
    }
    pub(crate) fn disconnect(&self) -> Result<Status, String> {
        let mut session = self.0.lock().map_err(|_| "Cookie sync unavailable")?;
        *session = Session::default();
        Ok(session.status())
    }
    pub(crate) fn receive(&self, cookies: Vec<BrowserCookie>) -> Result<Status, String> {
        let mut session = self.0.lock().map_err(|_| "Cookie sync unavailable")?;
        if session.status().state != "waiting" {
            return Err("Enable Cookie Sync in PineFetch first".into());
        }
        let (contents, count, expires_at) = netscape_cookies(cookies)?;
        session.cookies = Some(Arc::new(CookieData {
            contents,
            count,
            expires_at,
        }));
        session.waiting_since = None;
        Ok(session.status())
    }
}
pub(crate) fn supported_browser(browser: &str) -> bool {
    matches!(
        browser,
        "brave" | "chrome" | "chromium" | "edge" | "firefox" | "opera" | "safari" | "vivaldi"
    )
}
fn netscape_cookies(cookies: Vec<BrowserCookie>) -> Result<(String, usize, Option<f64>), String> {
    if cookies.is_empty() || cookies.len() > 4096 {
        return Err("No valid YouTube cookies received".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Clock unavailable")?
        .as_secs_f64();
    let mut output = String::from("# Netscape HTTP Cookie File\n");
    let mut count = 0;
    let mut expires_at: Option<f64> = None;
    for cookie in cookies {
        let domain = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
        if !(domain == "youtube.com" || domain.ends_with(".youtube.com"))
            || domain.split('.').any(|part| {
                part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || !cookie.path.starts_with('/')
            || cookie.name.is_empty()
            || [&cookie.domain, &cookie.path, &cookie.name, &cookie.value]
                .iter()
                .any(|s| s.chars().any(char::is_control))
        {
            return Err("Invalid YouTube cookies".into());
        }
        let expiry = match cookie.expiration_date {
            Some(time) if !time.is_finite() || !(0.0..=253402300799.0).contains(&time) => {
                return Err("Invalid cookie expiry".into())
            }
            Some(time) if time <= now => continue,
            Some(time) => time as u64,
            None => 0,
        };
        if expiry != 0 {
            expires_at =
                Some(expires_at.map_or(expiry as f64, |current| current.min(expiry as f64)));
        }
        let prefix = if cookie.http_only { "#HttpOnly_" } else { "" };
        let domain = if cookie.host_only {
            domain
        } else {
            format!(".{domain}")
        };
        output.push_str(&format!(
            "{prefix}{domain}\t{}\t{}\t{}\t{expiry}\t{}\t{}\n",
            if cookie.host_only { "FALSE" } else { "TRUE" },
            cookie.path,
            if cookie.secure { "TRUE" } else { "FALSE" },
            cookie.name,
            cookie.value
        ));
        count += 1;
    }
    if count == 0 {
        return Err("No current YouTube cookies received".into());
    }
    Ok((output, count, expires_at))
}
// The guard keeps the private cookie file alive for the full child-process lifetime,
// even when the user disconnects or syncs another browser while it is running.
pub(crate) fn apply(
    command: &mut Command,
    url: &str,
    processes: &ProcessState,
) -> Result<Option<Arc<SyncedCookies>>, String> {
    if Platform::from_url(url) != Some(Platform::YouTube) {
        return Ok(None);
    }
    let (cookies, browser) = {
        let mut session = processes
            .cookies
            .0
            .lock()
            .map_err(|_| "Cookie sync unavailable")?;
        session.status();
        (session.cookies.clone(), session.fallback_browser.clone())
    };
    if let Some(cookies) = cookies {
        match prepare_cookie_file(command, &cookies) {
            Ok(file) => return Ok(Some(file)),
            Err(error) if browser.is_none() => return Err(error),
            Err(_) => (),
        }
    }
    if let Some(browser) = browser {
        command.args(["--cookies-from-browser", &browser]);
    }
    Ok(None)
}
fn prepare_cookie_file(
    command: &mut Command,
    cookies: &CookieData,
) -> Result<Arc<SyncedCookies>, String> {
    // yt-dlp writes its cookie jar on exit. Give each child its own file so
    // simultaneous metadata and download requests cannot overwrite each other.
    let path = std::env::temp_dir().join(format!(
        "pinefetch-youtube-cookies-{}.txt",
        uuid::Uuid::new_v4()
    ));
    let mut file =
        OwnedTemporaryFile::create_at(path).map_err(|_| "Cookie sync could not be prepared")?;
    file.write_all(cookies.contents.as_bytes())
        .map_err(|_| "Cookie sync could not be prepared")?;
    file.sync()
        .map_err(|_| "Cookie sync could not be prepared")?;
    let synced = Arc::new(SyncedCookies { file });
    command.arg("--cookies").arg(&synced.file.path);
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn cookie(domain: &str, value: &str) -> BrowserCookie {
        serde_json::from_value(json!({"domain":domain,"path":"/","name":"synthetic","value":value,"secure":true,"httpOnly":true})).unwrap()
    }
    #[test]
    fn rejects_other_domains_injection_and_expired_cookies() {
        for domain in [
            "youtube.com.evil.test",
            "google.com",
            "bad\t.youtube.com",
            "a..youtube.com",
        ] {
            assert!(netscape_cookies(vec![cookie(domain, "synthetic")]).is_err());
        }
        assert!(netscape_cookies(vec![cookie(".youtube.com", "synthetic\nline")]).is_err());
        let mut expired = cookie("youtube.com", "synthetic");
        expired.expiration_date = Some(1.0);
        assert!(netscape_cookies(vec![expired]).is_err());
        let (jar, count, _) = netscape_cookies(vec![cookie(".youtube.com", "synthetic")]).unwrap();
        assert_eq!(count, 1);
        assert!(jar.contains("#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tsynthetic\tsynthetic"));
    }
    #[test]
    fn integration_cookie_sync_requires_enable_and_keeps_active_requests_alive_until_finished() {
        let processes = ProcessState::default();
        assert!(processes
            .cookies
            .receive(vec![cookie(".youtube.com", "synthetic")])
            .is_err());
        assert_eq!(processes.cookies.begin(None).unwrap().state, "waiting");
        assert_eq!(
            processes
                .cookies
                .receive(vec![cookie(".youtube.com", "synthetic")])
                .unwrap()
                .state,
            "ready"
        );
        assert!(processes
            .cookies
            .receive(vec![cookie(".youtube.com", "unsolicited")])
            .is_err());
        let mut unrelated = Command::new("unused");
        assert!(
            apply(&mut unrelated, "https://youtube.com.evil.test", &processes)
                .unwrap()
                .is_none()
        );
        assert_eq!(unrelated.get_args().count(), 0);
        let mut youtube = Command::new("unused");
        let guard = apply(&mut youtube, "https://youtu.be/synthetic", &processes)
            .unwrap()
            .unwrap();
        assert_eq!(youtube.get_args().next().unwrap(), "--cookies");
        let path = guard.file.path.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        processes.cookies.disconnect().unwrap();
        assert!(path.exists());
        assert!(apply(
            &mut Command::new("unused"),
            "https://youtube.com",
            &processes
        )
        .unwrap()
        .is_none());
        drop(guard);
        assert!(!path.exists());
        processes.cookies.begin(None).unwrap();
        processes.cookies.0.lock().unwrap().waiting_since =
            Some(Instant::now() - Duration::from_secs(301));
        assert_eq!(processes.cookies.status().unwrap().state, "off");
    }
    #[test]
    fn integration_reader_fallback_prefers_extension_handles_expiry_and_stops_on_disconnect() {
        let processes = ProcessState::default();
        let url = "https://youtube.com/watch?v=synthetic";
        let mut off = Command::new("unused");
        apply(&mut off, url, &processes).unwrap();
        assert_eq!(off.get_args().count(), 0);
        assert!(processes
            .cookies
            .begin(Some("chrome:arbitrary-path".into()))
            .is_err());
        processes.cookies.begin(Some("firefox".into())).unwrap();
        let mut reader = Command::new("unused");
        assert!(apply(&mut reader, url, &processes).unwrap().is_none());
        assert_eq!(
            reader.get_args().collect::<Vec<_>>(),
            ["--cookies-from-browser", "firefox"]
        );
        let mut unrelated = Command::new("unused");
        apply(&mut unrelated, "https://youtube.com.evil.test", &processes).unwrap();
        assert_eq!(unrelated.get_args().count(), 0);
        processes
            .cookies
            .receive(vec![cookie(".youtube.com", "synthetic")])
            .unwrap();
        let mut extension = Command::new("unused");
        let first = apply(&mut extension, url, &processes).unwrap().unwrap();
        assert_eq!(extension.get_args().next().unwrap(), "--cookies");
        assert!(!extension
            .get_args()
            .any(|arg| arg == "--cookies-from-browser"));
        let second = apply(&mut Command::new("unused"), url, &processes)
            .unwrap()
            .unwrap();
        assert_ne!(first.file.path, second.file.path);
        {
            let mut session = processes.cookies.0.lock().unwrap();
            Arc::get_mut(session.cookies.as_mut().unwrap())
                .unwrap()
                .expires_at = Some(1.0);
        }
        let mut expired = Command::new("unused");
        assert!(apply(&mut expired, url, &processes).unwrap().is_none());
        assert_eq!(
            expired.get_args().collect::<Vec<_>>(),
            ["--cookies-from-browser", "firefox"]
        );
        assert_eq!(processes.cookies.status().unwrap().source, "browser");
        processes.cookies.0.lock().unwrap().waiting_since =
            Some(Instant::now() - Duration::from_secs(301));
        assert_eq!(processes.cookies.status().unwrap().state, "reader");
        processes.cookies.disconnect().unwrap();
        let mut disconnected = Command::new("unused");
        apply(&mut disconnected, url, &processes).unwrap();
        assert_eq!(disconnected.get_args().count(), 0);
    }
}
