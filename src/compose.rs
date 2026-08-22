use crate::compose_types;
use anyhow::{Context, Result};
use log::{debug, error, log_enabled};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ComposeContext {
    pub compose_file: PathBuf,
    pub env_file: Option<PathBuf>,
    pub project_directory: Option<String>,
    pub hostname: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ComposeAction {
    Run,
    Restart,
}

impl ComposeAction {
    pub fn as_gerund(&self) -> &str {
        match self {
            ComposeAction::Run => "running",
            ComposeAction::Restart => "restarting",
        }
    }

    pub fn as_past_participle(&self) -> &str {
        match self {
            ComposeAction::Run => "run",
            ComposeAction::Restart => "restarted",
        }
    }
}

impl std::fmt::Display for ComposeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComposeAction::Run => write!(f, "run"),
            ComposeAction::Restart => write!(f, "restart"),
        }
    }
}

pub fn compose_command<S: AsRef<str>>(
    context: &ComposeContext,
    profile: Option<S>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("compose").arg("-f").arg(context.compose_file.as_os_str());
    if let Some(env_file) = &context.env_file {
        cmd.arg("--env-file").arg(env_file.as_os_str());
    }
    if let Some(project_directory) = &context.project_directory {
        cmd.arg("--project-directory").arg(project_directory.as_str());
    }
    if let Some(profile) = profile {
        cmd.arg("--profile").arg(profile.as_ref());
    }
    cmd
}

pub async fn _load_compose_profiles(context: &ComposeContext) -> Result<Vec<String>> {
    let mut cmd = compose_command(context, None::<&str>);
    let out = cmd
        .arg("config")
        .arg("--profiles")
        .output()
        .await
        .with_context(|| "docker compose config --profiles")?;
    let out_s = std::str::from_utf8(&out.stdout)?;
    let profiles: Vec<String> = out_s.lines().map(|line| line.to_owned()).collect();
    Ok(profiles)
}

pub async fn load_compose_config<S: AsRef<str>>(
    context: &ComposeContext,
    profile: Option<S>,
) -> Result<compose_types::Compose> {
    let mut cmd = compose_command(context, profile);
    debug!("compose command context: {context:?}");
    let out =
        cmd.arg("config").output().await.with_context(|| "docker compose config")?;
    let stderr_s = std::str::from_utf8(&out.stderr).unwrap_or("<invalid utf-8>");
    if log_enabled!(log::Level::Debug) {
        // never log the raw config: it contains resolved environment
        // variables, which commonly hold secrets
        debug!("compose config:\r\n{}", redact_compose_config(&out.stdout));
    }
    for line in stderr_s.lines() {
        error!("compose config error: {line}");
    }
    let compose: compose_types::Compose =
        serde_yaml::from_slice(&out.stdout).context("parsing compose config")?;
    Ok(compose)
}

const REDACTED: &str = "<redacted>";

/// Render `docker compose config` output for logging with the values of every
/// service's `environment` stanza replaced by `<redacted>`.  Keys are kept so
/// the log remains useful for debugging which variables are set.
///
/// If the output cannot be parsed as YAML, nothing from it is returned: it's
/// safer to log nothing than to risk leaking secrets.
fn redact_compose_config(raw: &[u8]) -> String {
    let mut value: serde_yaml::Value = match serde_yaml::from_slice(raw) {
        Ok(value) => value,
        Err(e) => return format!("<not shown: could not parse compose config: {e}>"),
    };
    if let Some(services) = value.get_mut("services").and_then(|s| s.as_mapping_mut()) {
        for (_, service) in services.iter_mut() {
            if let Some(environment) = service.get_mut("environment") {
                redact_environment(environment);
            }
        }
    }
    serde_yaml::to_string(&value).unwrap_or_else(|e| {
        format!("<not shown: could not serialize compose config: {e}>")
    })
}

/// Redact an `environment` stanza in place.  Handles both the normalized
/// mapping form (`KEY: value`) and the list form (`- KEY=value`).
fn redact_environment(environment: &mut serde_yaml::Value) {
    match environment {
        serde_yaml::Value::Mapping(map) => {
            for (_, v) in map.iter_mut() {
                if !v.is_null() {
                    *v = serde_yaml::Value::String(REDACTED.to_string());
                }
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for item in seq.iter_mut() {
                if let Some(s) = item.as_str() {
                    if let Some((key, _)) = s.split_once('=') {
                        *item = serde_yaml::Value::String(format!("{key}={REDACTED}"));
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_environment_mapping_values() {
        let raw = b"\
name: example
services:
  app:
    image: alpine
    environment:
      DB_PASSWORD: hunter2
      API_KEY: sk-live-123
      EMPTY: null
    labels:
      co.architect.composer.run: '0 0 * * * *'
";
        let out = redact_compose_config(raw);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("sk-live-123"), "{out}");
        assert!(out.contains("DB_PASSWORD: <redacted>"), "{out}");
        assert!(out.contains("API_KEY: <redacted>"), "{out}");
        assert!(out.contains("EMPTY: null"), "{out}");
        // non-environment content is preserved
        assert!(out.contains("image: alpine"), "{out}");
        assert!(out.contains("co.architect.composer.run"), "{out}");
    }

    #[test]
    fn redacts_environment_list_values() {
        let raw = b"\
services:
  app:
    environment:
      - DB_PASSWORD=hunter2
      - PASSTHROUGH
";
        let out = redact_compose_config(raw);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("DB_PASSWORD=<redacted>"), "{out}");
        assert!(out.contains("PASSTHROUGH"), "{out}");
    }

    #[test]
    fn unparseable_config_is_not_echoed() {
        let raw = b"services: [unterminated\n  SECRET: hunter2";
        let out = redact_compose_config(raw);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.starts_with("<not shown"), "{out}");
    }

    #[test]
    fn services_without_environment_are_untouched() {
        let raw = b"services:\n  app:\n    image: alpine\n  other: null\n";
        let out = redact_compose_config(raw);
        assert!(out.contains("image: alpine"), "{out}");
    }
}
