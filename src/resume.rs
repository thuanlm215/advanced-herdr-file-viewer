//! Best-effort relaunch of viewer processes after a full herdr server restart.
//!
//! Herdr restores pane ids, layout, cwd, and focus, but arbitrary processes return as fresh
//! shells. A running managed viewer therefore leaves one tiny record in the plugin-owned state
//! directory. The manifest's one-shot `[[startup]]` hook reconciles those records after herdr's
//! API is ready and uses `pane run` to start a fresh viewer in the same restored pane.
//!
//! This deliberately does not persist UI state. The record carries only enough launch context to
//! resolve the same tree root and config file again. Every operation is best-effort: state or host
//! failures must never prevent an ordinary viewer launch or affect an unrelated pane.

use crate::context::LaunchContext;
use crate::herdr::HerdrCli;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const STATE_DIR_ENV: &str = "HERDR_PLUGIN_STATE_DIR";
pub const SOCKET_ENV: &str = "HERDR_SOCKET_PATH";
pub const PANE_ENV: &str = "HERDR_PANE_ID";
pub const PLUGIN_ENV: &str = "HERDR_PLUGIN_ID";
pub const ENTRYPOINT_ENV: &str = "HERDR_PLUGIN_ENTRYPOINT_ID";
pub const EVENT_ENV: &str = "HERDR_PLUGIN_EVENT";

const PLUGIN_ID: &str = "advanced-herdr-file-viewer";
const ENTRYPOINT_ID: &str = "file-viewer";
const RECORD_SCHEMA: u32 = 1;
const RECORD_DIR: &str = "pane-resume-v1";
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const MAX_RECORD_FILES: usize = 256;
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRecord {
    schema: u32,
    socket_path: String,
    pane_id: String,
    launch_context: LaunchContext,
    config_path: PathBuf,
}

impl ResumeRecord {
    pub fn launch_context(&self) -> &LaunchContext {
        &self.launch_context
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

/// A registered managed viewer. The record remains armed unless the event loop reports a real
/// user-requested close; host loss, a crash, or an I/O error intentionally leaves it behind.
#[derive(Debug)]
pub struct Registration {
    path: PathBuf,
}

/// Validated input for a TUI process relaunched by the startup reconciler.
#[derive(Debug)]
pub struct ResumeLaunch {
    record_path: PathBuf,
    record: ResumeRecord,
}

impl ResumeLaunch {
    pub fn new(record_path: PathBuf, record: ResumeRecord) -> Self {
        Self {
            record_path,
            record,
        }
    }

    pub fn record(&self) -> &ResumeRecord {
        &self.record
    }

    pub fn into_registration(self) -> Registration {
        Registration {
            path: self.record_path,
        }
    }
}

impl Registration {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn disarm(self) {
        let _ = fs::remove_file(self.path);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnv {
    pub state_dir: PathBuf,
    pub socket_path: String,
    pub pane_id: String,
}

/// Read the managed-plugin identity from an injected environment getter. Partial context and
/// standalone runs are ineligible and retain today's no-persistence behavior.
pub fn runtime_env(get: impl Fn(&str) -> Option<String>) -> Option<RuntimeEnv> {
    let plugin = get(PLUGIN_ENV).filter(|s| !s.is_empty())?;
    let entrypoint = get(ENTRYPOINT_ENV).filter(|s| !s.is_empty())?;
    if plugin != PLUGIN_ID || entrypoint != ENTRYPOINT_ID {
        return None;
    }
    let state_dir = PathBuf::from(get(STATE_DIR_ENV).filter(|s| !s.is_empty())?);
    if !state_dir.is_absolute() {
        return None;
    }
    let socket_path = get(SOCKET_ENV).filter(|s| !s.is_empty())?;
    let pane_id = get(PANE_ENV).filter(|s| valid_pane_id(s))?;
    Some(RuntimeEnv {
        state_dir,
        socket_path,
        pane_id,
    })
}

pub fn runtime_env_from_process() -> Option<RuntimeEnv> {
    runtime_env(|key| std::env::var(key).ok())
}

/// Arm resume for a viewer whose TUI has successfully initialized.
pub fn register(
    env: RuntimeEnv,
    launch_context: LaunchContext,
    config_path: PathBuf,
) -> io::Result<Registration> {
    let record = ResumeRecord {
        schema: RECORD_SCHEMA,
        socket_path: env.socket_path,
        pane_id: env.pane_id,
        launch_context,
        config_path,
    };
    let dir = env.state_dir.join(RECORD_DIR);
    fs::create_dir_all(&dir)?;
    let path = record_path(&dir, &record);
    store_atomic(&path, &record)?;
    Ok(Registration { path })
}

pub fn load_record(path: &Path) -> io::Result<ResumeRecord> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pane resume record exceeds size cap",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pane resume record exceeds size cap",
        ));
    }
    let record: ResumeRecord = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    validate_record(&record)?;
    Ok(record)
}

/// Load a record only for the herdr pane/socket currently executing it. The startup reconciler
/// targets the pane explicitly; this second check prevents a copied/stale internal argv from
/// applying another session's launch context.
pub fn load_record_for_process(
    path: &Path,
    get: impl Fn(&str) -> Option<String>,
) -> io::Result<ResumeRecord> {
    let record = load_record(path)?;
    let socket = get(SOCKET_ENV).filter(|s| !s.is_empty()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing herdr socket identity")
    })?;
    let pane = get(PANE_ENV).filter(|s| valid_pane_id(s)).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing herdr pane identity")
    })?;
    if record.socket_path != socket || record.pane_id != pane {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resume record does not belong to this herdr pane",
        ));
    }
    Ok(record)
}

fn validate_record(record: &ResumeRecord) -> io::Result<()> {
    if record.schema != RECORD_SCHEMA
        || record.socket_path.is_empty()
        || !valid_pane_id(&record.pane_id)
        || !record.launch_context.cwd.is_absolute()
        || !record.config_path.is_absolute()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid pane resume record",
        ));
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RestoreSummary {
    pub relaunched: usize,
    pub already_running: usize,
    pub stale_removed: usize,
    pub skipped: usize,
}

#[derive(Deserialize)]
struct PaneListEnvelope {
    result: PaneListResult,
}

#[derive(Deserialize)]
struct PaneListResult {
    #[serde(default)]
    panes: Vec<PaneIdentity>,
}

#[derive(Deserialize)]
struct PaneIdentity {
    pane_id: Option<String>,
}

#[derive(Deserialize)]
struct ProcessInfoEnvelope {
    result: ProcessInfoResult,
}

#[derive(Deserialize)]
struct ProcessInfoResult {
    process_info: ProcessInfo,
}

#[derive(Deserialize)]
struct ProcessInfo {
    foreground_processes: Vec<serde_json::Value>,
}

/// Reconcile every record belonging to `socket_path` against one live herdr snapshot.
///
/// Verified against herdr 0.8.2 (`herdr --help`, 2026-09-03):
/// `pane list`, `pane process-info --pane ID`, and `pane run ID COMMAND` are the exact argv used.
/// A restored idle shell reports no `foreground_processes`; any foreground process makes this
/// fail closed so live handoff cannot duplicate a viewer and user work is never overwritten.
pub fn restore_with(
    host: &dyn HerdrCli,
    state_dir: &Path,
    socket_path: &str,
    viewer_exe: &Path,
    shell: ShellKind,
) -> RestoreSummary {
    let mut summary = RestoreSummary::default();
    if socket_path.is_empty() || !state_dir.is_absolute() {
        return summary;
    }
    let Some(exe) = viewer_exe.to_str() else {
        return summary;
    };
    let dir = state_dir.join(RECORD_DIR);
    let records = records_for_socket(&dir, socket_path);
    if records.is_empty() {
        return summary;
    }

    let Ok(raw_panes) = host.run_json(&["pane", "list"]) else {
        summary.skipped = records.len();
        return summary;
    };
    let Ok(panes) = serde_json::from_str::<PaneListEnvelope>(&raw_panes) else {
        summary.skipped = records.len();
        return summary;
    };
    let live: BTreeSet<String> = panes
        .result
        .panes
        .into_iter()
        .filter_map(|p| p.pane_id)
        .collect();

    // A corrupt directory may contain duplicate records for a pane. Deduplicate before issuing
    // host commands: at most one `pane run` is ever sent to a pane in one startup reconciliation.
    let mut seen = BTreeSet::new();
    for (path, record) in records {
        if !seen.insert(record.pane_id.clone()) {
            summary.skipped += 1;
            continue;
        }
        if !live.contains(&record.pane_id) {
            if fs::remove_file(path).is_ok() {
                summary.stale_removed += 1;
            } else {
                summary.skipped += 1;
            }
            continue;
        }

        let Ok(raw_process) =
            host.run_json(&["pane", "process-info", "--pane", record.pane_id.as_str()])
        else {
            summary.skipped += 1;
            continue;
        };
        let Ok(process) = serde_json::from_str::<ProcessInfoEnvelope>(&raw_process) else {
            summary.skipped += 1;
            continue;
        };
        if !process.result.process_info.foreground_processes.is_empty() {
            summary.already_running += 1;
            continue;
        }

        let Some(record_arg) = path.to_str() else {
            summary.skipped += 1;
            continue;
        };
        let command = resume_command(shell, exe, record_arg);
        if host
            .run(&["pane", "run", record.pane_id.as_str(), command.as_str()])
            .is_ok()
        {
            summary.relaunched += 1;
        } else {
            summary.skipped += 1;
        }
    }
    summary
}

/// Production entrypoint for the manifest startup hook. It is intentionally silent; herdr records
/// command completion, while a partial failure remains retryable at the next server startup.
pub fn restore_from_env(host: &dyn HerdrCli) -> RestoreSummary {
    if std::env::var(PLUGIN_ENV).ok().as_deref() != Some(PLUGIN_ID)
        || std::env::var(EVENT_ENV).ok().as_deref() != Some("startup")
    {
        return RestoreSummary::default();
    }
    let Some(state_dir) = std::env::var(STATE_DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    else {
        return RestoreSummary::default();
    };
    let Some(socket) = std::env::var(SOCKET_ENV).ok().filter(|s| !s.is_empty()) else {
        return RestoreSummary::default();
    };
    let Ok(exe) = std::env::current_exe() else {
        return RestoreSummary::default();
    };
    restore_with(host, &state_dir, &socket, &exe, ShellKind::current())
}

fn records_for_socket(dir: &Path, socket_path: &str) -> Vec<(PathBuf, ResumeRecord)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .take(MAX_RECORD_FILES)
        .filter_map(|entry| {
            let path = entry.path();
            let record = load_record(&path).ok()?;
            (record.socket_path == socket_path).then_some((path, record))
        })
        .collect()
}

fn record_path(dir: &Path, record: &ResumeRecord) -> PathBuf {
    let pane = record
        .pane_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    dir.join(format!(
        "{:016x}-{pane}.json",
        stable_hash(&record.socket_path)
    ))
}

fn stable_hash(value: &str) -> u64 {
    value.as_bytes().iter().fold(0xcbf29ce484222325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn store_atomic(path: &Path, record: &ResumeRecord) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let staged = path.with_extension(format!("tmp-{}-{seq}", std::process::id()));
    fs::write(&staged, bytes)?;
    // Windows cannot atomically rename over an existing destination. Registration is best-effort,
    // and losing an older descriptor before publishing its replacement is preferable to failing
    // every subsequent launch for that pane.
    if cfg!(windows) && path.exists() {
        let _ = fs::remove_file(path);
    }
    match fs::rename(&staged, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(staged);
            Err(e)
        }
    }
}

pub fn valid_pane_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    PowerShell,
}

impl ShellKind {
    fn current() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }
}

pub fn resume_command(shell: ShellKind, exe: &str, record: &str) -> String {
    match shell {
        ShellKind::Posix => format!(
            "{} --resume-record {}",
            quote_posix(exe),
            quote_posix(record)
        ),
        ShellKind::PowerShell => format!(
            "& {} --resume-record {}",
            quote_powershell(exe),
            quote_powershell(record)
        ),
    }
}

fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_requires_the_exact_managed_entrypoint() {
        let state = std::env::current_dir()
            .unwrap()
            .join("state")
            .to_string_lossy()
            .into_owned();
        let get = |key: &str| match key {
            PLUGIN_ENV => Some(PLUGIN_ID.into()),
            ENTRYPOINT_ENV => Some(ENTRYPOINT_ID.into()),
            STATE_DIR_ENV => Some(state.clone()),
            SOCKET_ENV => Some("/socket".into()),
            PANE_ENV => Some("w1:p2".into()),
            _ => None,
        };
        assert!(runtime_env(get).is_some());
        assert!(runtime_env(|_| None).is_none());
    }

    #[test]
    fn shell_commands_quote_paths_without_interpolation() {
        assert_eq!(
            resume_command(ShellKind::Posix, "/a b/o'ne", "/s/r'ec"),
            "'/a b/o'\"'\"'ne' --resume-record '/s/r'\"'\"'ec'"
        );
        assert_eq!(
            resume_command(ShellKind::PowerShell, r"C:\A B\o'ne.exe", r"C:\S\r'ec"),
            r"& 'C:\A B\o''ne.exe' --resume-record 'C:\S\r''ec'"
        );
    }

    #[test]
    fn pane_ids_are_flag_safe() {
        assert!(valid_pane_id("wA:p9"));
        assert!(!valid_pane_id("--current"));
        assert!(!valid_pane_id("w1:p2;echo"));
        assert!(!valid_pane_id(""));
    }
}
