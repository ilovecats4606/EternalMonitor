//! Automatic support uploads. A bounded channel protects media threads; a disk outbox
//! retains unacknowledged events across launches. No dashboard credential belongs here.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_FILES: usize = 512;
const BATCH_SIZE: usize = 4;
static SENDER: OnceLock<mpsc::SyncSender<String>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static STATUS: Mutex<String> = Mutex::new(String::new());
static IDENTITY: OnceLock<(String, String)> = OnceLock::new();
static OUTBOX: OnceLock<PathBuf> = OnceLock::new();
// Shared by the upload thread and fatal reports, so events never reuse a number.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub ingest_key: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_http: bool,
}
fn default_enabled() -> bool {
    true
}
impl Config {
    fn load() -> Option<Self> {
        let path = crate::settings::app_data_dir()?.join("diagnostics-upload.json");
        let mut config: Self =
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .or_else(|| {
                    Some(Self {
                        endpoint: std::env::var("ETERNAL_DIAGNOSTICS_ENDPOINT").ok().or_else(
                            || option_env!("ETERNAL_DIAGNOSTICS_ENDPOINT").map(str::to_owned),
                        )?,
                        ingest_key: std::env::var("ETERNAL_DIAGNOSTICS_INGEST_KEY")
                            .ok()
                            .or_else(|| {
                                option_env!("ETERNAL_DIAGNOSTICS_INGEST_KEY").map(str::to_owned)
                            })?,
                        enabled: true,
                        allow_http: false,
                    })
                })?;
        if let Ok(value) = std::env::var("ETERNAL_DIAGNOSTICS_ENDPOINT") {
            config.endpoint = value;
        }
        if let Ok(value) = std::env::var("ETERNAL_DIAGNOSTICS_INGEST_KEY") {
            config.ingest_key = value;
        }
        config.allow_http = std::env::var("ETERNAL_DIAGNOSTICS_ALLOW_HTTP").is_ok_and(|x| x == "1");
        config.valid().then_some(config)
    }
    fn valid(&self) -> bool {
        let uri: Result<ureq::http::Uri, _> = self.endpoint.parse();
        uri.is_ok_and(|uri| {
            let scheme = uri.scheme_str().unwrap_or_default();
            let authority = uri.authority().map_or("", |x| x.as_str());
            (scheme == "https" || (scheme == "http" && self.allow_http))
                && !authority.is_empty()
                && !authority.contains('@')
                && uri.query().is_none()
        }) && !self.ingest_key.is_empty()
            && !self.ingest_key.contains(['\r', '\n'])
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Event {
    sequence: u64,
    at_ms: u64,
    level: String,
    category: String,
    message: String,
}
#[derive(Serialize, Deserialize)]
struct Batch {
    schema: u32,
    batch_id: String,
    installation_id: String,
    launch_id: String,
    platform: String,
    app_version: String,
    build: String,
    events: Vec<Event>,
}

fn id() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn set_status(text: impl Into<String>) {
    if let Ok(mut value) = STATUS.lock() {
        *value = text.into();
    }
}
pub fn status() -> String {
    format!(
        "{}{}; queue_dropped={}",
        STATUS.lock().map(|x| x.clone()).unwrap_or_default(),
        IDENTITY
            .get()
            .map_or_else(String::new, |(installation, launch)| {
                format!("; installation_id={installation}; launch_id={launch}")
            }),
        DROPPED.load(Ordering::Relaxed)
    )
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}
pub fn set_enabled(value: bool) {
    let path = crate::settings::app_data_dir().map(|p| p.join("diagnostics-upload-disabled"));
    if let Some(path) = path {
        let result = if value {
            if path.exists() {
                fs::remove_file(path)
            } else {
                Ok(())
            }
        } else {
            fs::write(path, b"disabled")
        };
        if result.is_err() {
            set_status("Could not save automatic upload preference");
            return;
        }
    }
    ENABLED.store(value, Ordering::Relaxed);
    set_status(if value {
        "Automatic upload enabled"
    } else {
        "Automatic upload disabled"
    });
}

pub struct UploadGuard(
    Option<mpsc::Sender<()>>,
    Option<std::thread::JoinHandle<()>>,
);
impl UploadGuard {
    pub fn start() -> Self {
        ENABLED.store(
            !crate::settings::app_data_dir()
                .is_some_and(|p| p.join("diagnostics-upload-disabled").exists()),
            Ordering::Relaxed,
        );
        let Some(config) = Config::load() else {
            set_status("Automatic upload is not configured");
            return Self(None, None);
        };
        if !config.enabled {
            set_status("Automatic upload is disabled");
            return Self(None, None);
        }
        let directory = crate::settings::app_data_dir()
            .unwrap()
            .join("diagnostics-outbox");
        // Ready before any thread runs, so a fatal report can queue at once.
        let identity = match identity(&directory) {
            Ok(identity) => identity,
            Err(_) => {
                set_status("Automatic upload stopped: local outbox unavailable");
                return Self(None, None);
            }
        };
        let (sender, receiver) = mpsc::sync_channel(256);
        if SENDER.set(sender).is_err() {
            return Self(None, None);
        }
        let _ = IDENTITY.set(identity);
        let _ = OUTBOX.set(directory.clone());
        let (stop, stopped) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let result = run(config, directory, receiver, stopped);
            if result.is_err() {
                set_status("Automatic upload stopped: local outbox unavailable");
            }
        });
        Self(Some(stop), Some(thread))
    }
}
impl Drop for UploadGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.1.take() {
            let _ = thread.join();
        }
    }
}
pub fn record(line: &str) {
    if enabled() {
        if let Some(sender) = SENDER.get() {
            let mut end = line.len().min(8192);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if sender.try_send(line[..end].to_owned()).is_err() {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
/// Queue one error event on disk now. The upload thread sends it, or the next
/// launch does if this process ends first.
pub fn queue_now(message: &str) {
    if !enabled() {
        return;
    }
    if let Some(directory) = OUTBOX.get() {
        if queue_into(directory, message).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
fn queue_into(directory: &Path, message: &str) -> std::io::Result<()> {
    persist(directory, &batch(vec![event(message, "error")])?)
}
fn identity(directory: &Path) -> std::io::Result<(String, String)> {
    fs::create_dir_all(directory)?;
    let identity_path = directory.join("installation-id");
    let installation_id = match fs::read_to_string(&identity_path) {
        Ok(id) if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) => id,
        _ => {
            let id = id()?;
            fs::write(identity_path, &id)?;
            id
        }
    };
    Ok((installation_id, id()?))
}
fn level(line: &str) -> &'static str {
    if line.contains(" ERROR ") {
        "error"
    } else if line.contains(" WARN ") {
        "warning"
    } else {
        "info"
    }
}
fn event(line: &str, level: &str) -> Event {
    Event {
        sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
        at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        level: level.into(),
        category: "host".into(),
        message: crate::logging::redact_support_text(line),
    }
}
fn batch(events: Vec<Event>) -> std::io::Result<Batch> {
    let (installation_id, launch_id) = IDENTITY.get().cloned().unwrap_or_default();
    Ok(Batch {
        schema: 1,
        batch_id: id()?,
        installation_id,
        launch_id,
        platform: std::env::consts::OS.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        build: env!("ETERNAL_BUILD_REVISION").into(),
        events,
    })
}
fn files(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|x| x.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}
fn persist(directory: &Path, batch: &Batch) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(batch)?;
    let path = directory.join(format!("{}-{}.json", batch.events[0].at_ms, batch.batch_id));
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    let paths = files(directory)?;
    let mut total: u64 = paths
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    let mut count = paths.len();
    for path in paths.iter() {
        if count <= MAX_FILES && total <= 8 * 1024 * 1024 {
            break;
        }
        total = total.saturating_sub(fs::metadata(path).map_or(0, |m| m.len()));
        count -= 1;
        let lost = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Batch>(&bytes).ok())
            .map_or(1, |batch| batch.events.len());
        fs::remove_file(path)?;
        DROPPED.fetch_add(lost as u64, Ordering::Relaxed);
    }
    Ok(())
}
fn upload(
    agent: &ureq::Agent,
    config: &Config,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let batch: Batch = match serde_json::from_slice(&bytes) {
        Ok(batch) => batch,
        Err(_) => {
            fs::remove_file(path)?;
            DROPPED.fetch_add(1, Ordering::Relaxed);
            set_status("A damaged upload batch was discarded");
            return Ok(());
        }
    };
    let mut response = agent
        .post(&config.endpoint)
        .header("Authorization", format!("Bearer {}", config.ingest_key))
        .header("Content-Type", "application/json")
        .send(bytes)?;
    if response.status() != ureq::http::StatusCode::OK {
        return Err("Upload did not return HTTP 200".into());
    }
    let ack: serde_json::Value = response.body_mut().with_config().limit(4096).read_json()?;
    if ack.get("accepted_batch").and_then(|x| x.as_str()) != Some(&batch.batch_id) {
        return Err("Upload acknowledgement mismatch".into());
    }
    fs::remove_file(path)?;
    Ok(())
}
fn run(
    config: Config,
    directory: PathBuf,
    receiver: mpsc::Receiver<String>,
    stop: mpsc::Receiver<()>,
) -> std::io::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(0)
        .build()
        .into();
    let mut pending = Vec::new();
    let mut next = std::time::Instant::now();
    let mut backoff = 5;
    let mut last_persist = std::time::Instant::now();
    set_status("Automatic upload enabled; waiting for events");
    loop {
        let stopping = stop.try_recv().is_ok();
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => pending.push(event(&line, level(&line))),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !enabled() {
            pending.clear();
            for path in files(&directory)? {
                fs::remove_file(path)?;
            }
            while receiver.try_recv().is_ok() {}
            if stopping {
                return Ok(());
            }
            continue;
        }
        if pending.len() >= BATCH_SIZE
            || ((!pending.is_empty())
                && (last_persist.elapsed() >= Duration::from_secs(1) || stopping))
        {
            persist(&directory, &batch(std::mem::take(&mut pending))?)?;
            last_persist = std::time::Instant::now();
        }
        if !stopping && std::time::Instant::now() >= next {
            let mut failed = false;
            for path in files(&directory)?.into_iter().take(8) {
                if upload(&agent, &config, &path).is_err() {
                    failed = true;
                    break;
                }
            }
            if failed {
                set_status(format!(
                    "Automatic upload waiting to retry; pending_batches={}",
                    files(&directory)?.len()
                ));
                backoff = (backoff * 2).min(300);
            } else {
                set_status(format!(
                    "Automatic upload connected; pending_batches={}",
                    files(&directory)?.len()
                ));
                backoff = 5;
            }
            next = std::time::Instant::now() + Duration::from_secs(backoff);
        }
        if stopping {
            // Drain already accepted events to disk before returning.
            while let Ok(line) = receiver.try_recv() {
                pending.push(event(&line, level(&line)));
                if pending.len() >= BATCH_SIZE {
                    persist(&directory, &batch(std::mem::take(&mut pending))?)?;
                }
            }
            if !pending.is_empty() {
                persist(&directory, &batch(pending)?)?;
            }
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_batch_until_http_200_and_matching_acknowledgement() {
        use std::io::{Read, Write};
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("batch.json");
        let batch = Batch {
            schema: 1,
            batch_id: id().unwrap(),
            installation_id: id().unwrap(),
            launch_id: id().unwrap(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            build: "test".into(),
            events: vec![],
        };
        fs::write(&path, serde_json::to_vec(&batch).unwrap()).unwrap();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .build()
            .into();
        for (status, acknowledgement, accepted) in [
            (200, "wrong-batch", false),
            (202, batch.batch_id.as_str(), false),
            (200, batch.batch_id.as_str(), true),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = serde_json::json!({"accepted_batch": acknowledgement}).to_string();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0, "request ended before body");
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse().ok())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break;
                        }
                    }
                }
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let config = Config {
                endpoint: format!("http://{address}/v1/events"),
                ingest_key: "write-only".into(),
                enabled: true,
                allow_http: true,
            };
            assert_eq!(upload(&agent, &config, &path).is_ok(), accepted);
            server.join().unwrap();
            assert_eq!(path.exists(), !accepted);
        }
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn fatal_reports_queue_as_errors_with_their_own_sequence_numbers() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        queue_into(&directory, "EternalMonitor could not start: token=abc").unwrap();
        queue_into(&directory, "EternalMonitor could not start: again").unwrap();
        let saved: Vec<Batch> = files(&directory)
            .unwrap()
            .iter()
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect();
        assert_eq!(saved.len(), 2);
        for batch in &saved {
            assert_eq!(batch.events.len(), 1);
            assert_eq!(batch.events[0].level, "error");
            assert_eq!(batch.events[0].category, "host");
        }
        assert!(saved
            .iter()
            .any(|b| b.events[0].message == "EternalMonitor could not start: token=[REDACTED]"));
        assert_ne!(saved[0].events[0].sequence, saved[1].events[0].sequence);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn drained_events_keep_their_level() {
        assert_eq!(level("2026-10-20T20:47:20Z ERROR Host panic"), "error");
        assert_eq!(
            level("2026-10-20T20:47:20Z  WARN Missing downlevel flags"),
            "warning"
        );
        assert_eq!(
            level("2026-10-20T20:47:20Z  INFO EternalMonitor shutting down"),
            "info"
        );
    }
    #[test]
    fn rejects_plaintext_and_credentials_in_urls() {
        let mut c = Config {
            endpoint: "https://logs.example/v1/events".into(),
            ingest_key: "write-only".into(),
            enabled: true,
            allow_http: false,
        };
        assert!(c.valid());
        for url in [
            "http://logs.example/v1/events",
            "https://secret@logs.example/v1/events",
            "https://logs.example/v1/events?key=secret",
            "relative",
        ] {
            c.endpoint = url.into();
            assert!(!c.valid());
        }
    }
    #[test]
    fn outbox_survives_reopen_and_is_bounded() {
        let directory = std::env::temp_dir().join(id().unwrap());
        fs::create_dir_all(&directory).unwrap();
        let mut batch = Batch {
            schema: 1,
            batch_id: id().unwrap(),
            installation_id: id().unwrap(),
            launch_id: id().unwrap(),
            platform: "windows".into(),
            app_version: "0.3.0".into(),
            build: "test".into(),
            events: vec![Event {
                sequence: 1,
                at_ms: 1,
                level: "error".into(),
                category: "decode".into(),
                message: "status=-9".into(),
            }],
        };
        for _ in 0..MAX_FILES + 1 {
            batch.batch_id = id().unwrap();
            persist(&directory, &batch).unwrap();
        }
        let paths = files(&directory).unwrap();
        assert_eq!(paths.len(), MAX_FILES);
        let saved: Batch = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
        assert_eq!(saved.events[0].message, "status=-9");
        fs::remove_dir_all(directory).unwrap();
    }
}
