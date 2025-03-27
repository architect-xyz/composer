use crate::compose_types;
use anyhow::{Context, Result};
use log::{debug, error};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ComposeContext {
    pub compose_file: PathBuf,
    pub env_file: Option<PathBuf>,
    pub project_directory: Option<String>,
    pub hostname: Option<String>,
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
    let stdout_s = std::str::from_utf8(&out.stdout).unwrap_or("<invalid utf-8>");
    let stderr_s = std::str::from_utf8(&out.stderr).unwrap_or("<invalid utf-8>");
    debug!("compose config:\r\n{stdout_s}");
    for line in stderr_s.lines() {
        error!("compose config error: {line}");
    }
    let compose: compose_types::Compose =
        serde_yaml::from_slice(&out.stdout).context("parsing compose config")?;
    Ok(compose)
}
