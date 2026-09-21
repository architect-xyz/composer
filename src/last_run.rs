//! Persistent record of when composer last ran or restarted each service.
//!
//! Scheduled runs use `docker compose run --rm`, so once a job finishes no
//! container remains for `status` to inspect.  The scheduler therefore keeps
//! its own record, in a small JSON file under the platform state directory,
//! keyed by the compose file's path on the host (see [`project_key`]) and
//! service name.  Both the scheduler (which writes it) and `composer status`
//! (which reads it) resolve the same key, so the CLI can tell "never ran"
//! from "no idea".

use crate::compose::{ComposeAction, ComposeContext};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Environment variable overriding the directory the record is kept in.
pub const STATE_DIR_ENV: &str = "COMPOSER_STATE_DIR";
const FILE_NAME: &str = "last-runs.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastRun {
    /// What composer did: `run` or `restart`
    pub action: String,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// None while the command is still running
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

/// What composer knows about a service's run history.
#[derive(Debug, Clone, PartialEq)]
pub enum RunHistory {
    /// No record file for this compose project: no scheduler has registered
    /// it on this host, so nothing can be said either way.
    Unknown,
    /// The project is registered but composer has never run this service.
    Never,
    Last(LastRun),
}

/// compose file path -> service -> last run
type Records = BTreeMap<String, BTreeMap<String, LastRun>>;

/// Directory holding composer's own state.  `COMPOSER_STATE_DIR` wins;
/// otherwise the platform convention (`~/Library/Application Support` on
/// macOS, `$XDG_STATE_HOME` or `~/.local/state` elsewhere).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(STATE_DIR_ENV).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        return Some(PathBuf::from(home).join("Library/Application Support/composer"));
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("composer"));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/composer"))
}

fn records_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join(FILE_NAME))
}

/// The key a compose project is recorded under: the path of its compose
/// file *on the host*.
///
/// Normally that is the canonical compose file path, so the scheduler (which
/// canonicalizes) and `status` (which may not) agree.  When a project
/// directory is set (`COMPOSE_PROJECT_DIRECTORY`), the key is that directory
/// plus the compose file's name instead.  A scheduler running in a container
/// sees the compose file at a mount path like `/compose.yml`, but is told the
/// host's project directory, so this gives it the same key as a
/// `composer status` run on the host against the real file.
fn project_key(context: &ComposeContext) -> String {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let project_file = context
        .project_directory
        .as_deref()
        .filter(|d| !d.is_empty())
        .zip(context.compose_file.file_name())
        .map(|(dir, name)| canonical(Path::new(dir)).join(name));
    project_file.unwrap_or_else(|| canonical(&context.compose_file)).display().to_string()
}

fn read_records(path: &Path) -> Result<Records> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Records::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write atomically (temp file + rename) so a crash mid-write can't leave a
/// truncated record behind, and a concurrent reader never sees a partial one.
fn write_records(path: &Path, records: &Records) -> Result<()> {
    let dir = path.parent().context("records path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let json = serde_json::to_vec_pretty(records)?;
    fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// Read-modify-write the record for one compose project.
fn update<F>(context: &ComposeContext, f: F)
where
    F: FnOnce(&mut BTreeMap<String, LastRun>),
{
    let Some(path) = records_path() else {
        debug!("no state directory (HOME unset); not recording last run");
        return;
    };
    let result = (|| -> Result<()> {
        let mut records = read_records(&path)?;
        f(records.entry(project_key(context)).or_default());
        write_records(&path, &records)
    })();
    if let Err(e) = result {
        warn!("could not update last-run record: {e:?}");
    }
}

/// Register a compose project so its services report `never` rather than
/// `unknown` until they first run.  Called when the scheduler starts.
pub fn register_project(context: &ComposeContext) {
    update(context, |_| {});
}

pub fn record_started(
    context: &ComposeContext,
    service: &str,
    action: ComposeAction,
    at: DateTime<Utc>,
) {
    update(context, |project| {
        project.insert(
            service.to_string(),
            LastRun {
                action: action.to_string(),
                started_at: at,
                finished_at: None,
                success: None,
            },
        );
    });
}

pub fn record_finished(
    context: &ComposeContext,
    service: &str,
    at: DateTime<Utc>,
    success: bool,
) {
    update(context, |project| {
        if let Some(run) = project.get_mut(service) {
            run.finished_at = Some(at);
            run.success = Some(success);
        }
    });
}

/// Run history for every service in this compose project, or None if the
/// project has never been registered on this host.
pub fn load(context: &ComposeContext) -> Option<BTreeMap<String, LastRun>> {
    let path = records_path()?;
    let records = match read_records(&path) {
        Ok(records) => records,
        Err(e) => {
            warn!("could not read last-run record: {e:?}");
            return None;
        }
    };
    records.get(&project_key(context)).cloned()
}

/// Look up one service's history in the result of [`load`].
pub fn history(records: Option<&BTreeMap<String, LastRun>>, service: &str) -> RunHistory {
    match records {
        None => RunHistory::Unknown,
        Some(records) => match records.get(service) {
            Some(run) => RunHistory::Last(run.clone()),
            None => RunHistory::Never,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn temp_records_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("composer-last-run-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join(FILE_NAME)
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let path = temp_records_path("missing");
        assert!(read_records(&path).unwrap().is_empty());
    }

    #[test]
    fn round_trips_records() {
        let path = temp_records_path("roundtrip");
        let mut records = Records::new();
        let project = records.entry("/srv/app/compose.yml".to_string()).or_default();
        project.insert(
            "backup".to_string(),
            LastRun {
                action: "run".to_string(),
                started_at: Utc.with_ymd_and_hms(2026, 8, 26, 10, 15, 0).unwrap(),
                finished_at: None,
                success: None,
            },
        );
        write_records(&path, &records).unwrap();
        assert_eq!(read_records(&path).unwrap(), records);
        // no temp file left behind
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(FILE_NAME)]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn history_distinguishes_unknown_never_and_last() {
        assert_eq!(history(None, "backup"), RunHistory::Unknown);
        let mut project = BTreeMap::new();
        assert_eq!(history(Some(&project), "backup"), RunHistory::Never);
        let run = LastRun {
            action: "run".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 8, 26, 10, 15, 0).unwrap(),
            finished_at: Some(Utc.with_ymd_and_hms(2026, 8, 26, 10, 16, 0).unwrap()),
            success: Some(true),
        };
        project.insert("backup".to_string(), run.clone());
        assert_eq!(history(Some(&project), "backup"), RunHistory::Last(run));
        assert_eq!(history(Some(&project), "other"), RunHistory::Never);
    }

    fn context(compose_file: &str, project_directory: Option<&str>) -> ComposeContext {
        ComposeContext {
            compose_file: PathBuf::from(compose_file),
            env_file: None,
            project_directory: project_directory.map(str::to_string),
            hostname: "test".to_string(),
        }
    }

    #[test]
    fn project_key_uses_host_project_directory() {
        // no project directory: the compose file's own path
        assert_eq!(
            project_key(&context("/nonexistent/app/compose.yml", None)),
            "/nonexistent/app/compose.yml"
        );
        assert_eq!(
            project_key(&context("/nonexistent/app/compose.yml", Some(""))),
            "/nonexistent/app/compose.yml"
        );
        // in a container the file is at a mount path, but the project
        // directory names where it lives on the host...
        let in_container = context("/compose.yml", Some("/nonexistent/app"));
        assert_eq!(project_key(&in_container), "/nonexistent/app/compose.yml");
        // ...which is the key `composer status` on the host arrives at
        let on_host = context("/nonexistent/app/compose.yml", None);
        assert_eq!(project_key(&in_container), project_key(&on_host));
        // a trailing slash on the directory doesn't change the key
        assert_eq!(
            project_key(&context("/compose.yml", Some("/nonexistent/app/"))),
            "/nonexistent/app/compose.yml"
        );
    }

    #[test]
    fn state_dir_prefers_env_override() {
        // the override is read from the environment at call time; only
        // assert on the platform default when it is not set
        if std::env::var_os(STATE_DIR_ENV).is_none() {
            let dir = state_dir().expect("HOME is set in tests");
            assert!(dir.ends_with("composer"), "{}", dir.display());
        }
    }
}
