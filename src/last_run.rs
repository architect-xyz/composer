//! Persistent record of when composer last ran or restarted each service.
//!
//! Scheduled runs use `docker compose run --rm`, so once a job finishes no
//! container remains for `status` to inspect.  The scheduler therefore keeps
//! its own record, in a small JSON file under the platform state directory,
//! keyed by the canonical compose file path and service name.  Both the
//! scheduler (which writes it) and `composer status` (which reads it) resolve
//! the same path, so the CLI can tell "never ran" from "no idea".

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
    /// Composer has not run this service for as far back as the project's
    /// record goes, which is the given time.
    NotSince(DateTime<Utc>),
    Last(LastRun),
}

/// What is recorded about one compose project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// When the project first entered this record.  The record says nothing
    /// about earlier runs (it may have been lost, e.g. with a recreated
    /// container), so "never ran" is only known since then.
    pub registered_at: DateTime<Utc>,
    /// service -> last run
    #[serde(default)]
    pub services: BTreeMap<String, LastRun>,
}

/// compose file path -> project
type Records = BTreeMap<String, Project>;

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

/// The key a compose file is recorded under: its canonical path, so the
/// scheduler (which canonicalizes) and `status` (which may not) agree.
fn project_key(context: &ComposeContext) -> String {
    fs::canonicalize(&context.compose_file)
        .unwrap_or_else(|_| context.compose_file.clone())
        .display()
        .to_string()
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
fn update<F>(context: &ComposeContext, at: DateTime<Utc>, f: F)
where
    F: FnOnce(&mut Project),
{
    let Some(path) = records_path() else {
        debug!("no state directory (HOME unset); not recording last run");
        return;
    };
    let result = (|| -> Result<()> {
        let mut records = read_records(&path)?;
        let project = records
            .entry(project_key(context))
            .or_insert_with(|| Project { registered_at: at, services: BTreeMap::new() });
        f(project);
        write_records(&path, &records)
    })();
    if let Err(e) = result {
        warn!("could not update last-run record: {e:?}");
    }
}

/// Register a compose project so its services report `never (*)` rather
/// than `unknown` until they first run.  Called when the scheduler
/// starts; a project already in the record keeps its original time.
pub fn register_project(context: &ComposeContext, at: DateTime<Utc>) {
    update(context, at, |_| {});
}

pub fn record_started(
    context: &ComposeContext,
    service: &str,
    action: ComposeAction,
    at: DateTime<Utc>,
) {
    update(context, at, |project| {
        project.services.insert(
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
    update(context, at, |project| {
        if let Some(run) = project.services.get_mut(service) {
            run.finished_at = Some(at);
            run.success = Some(success);
        }
    });
}

/// Run history for every service in this compose project, or None if the
/// project has never been registered on this host.
pub fn load(context: &ComposeContext) -> Option<Project> {
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
pub fn history(project: Option<&Project>, service: &str) -> RunHistory {
    match project {
        None => RunHistory::Unknown,
        Some(project) => match project.services.get(service) {
            Some(run) => RunHistory::Last(run.clone()),
            None => RunHistory::NotSince(project.registered_at),
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
        let project =
            records.entry("/srv/app/compose.yml".to_string()).or_insert_with(|| {
                Project {
                    registered_at: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
                    services: BTreeMap::new(),
                }
            });
        project.services.insert(
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
    fn history_distinguishes_unknown_not_since_and_last() {
        assert_eq!(history(None, "backup"), RunHistory::Unknown);
        let registered_at = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let mut project = Project { registered_at, services: BTreeMap::new() };
        assert_eq!(
            history(Some(&project), "backup"),
            RunHistory::NotSince(registered_at)
        );
        let run = LastRun {
            action: "run".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 8, 26, 10, 15, 0).unwrap(),
            finished_at: Some(Utc.with_ymd_and_hms(2026, 8, 26, 10, 16, 0).unwrap()),
            success: Some(true),
        };
        project.services.insert("backup".to_string(), run.clone());
        assert_eq!(history(Some(&project), "backup"), RunHistory::Last(run));
        assert_eq!(history(Some(&project), "other"), RunHistory::NotSince(registered_at));
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
