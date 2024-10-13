use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use cron::Schedule;
use log::{debug, error, info, warn};
use std::{env::VarError, path::PathBuf};
use tokio::task::JoinSet;

mod compose_types;

/// Scheduler for docker-compose services
///
/// Add the `co.architect.composer.run` or `co.architect.composer.restart`
/// labels to your services with a cron expression (Quartz-compatible, e.g.
/// seconds field is first) to schedule runs or restarts.
#[derive(Parser)]
struct Args {
    // CR alee: try [docker-]compose.{yml,yaml}
    #[clap(short = 'f', default_value = "compose.yml")]
    compose_file: PathBuf,
    #[clap(long)]
    env_file: Option<PathBuf>,
    // Specify the --project-directory option for docker compose commands;
    //
    // This should used if there are relative directories in the compose
    // file and the file mount to composer doesn't match the outside
    // directory structure.
    #[clap(long)]
    project_directory: Option<String>,
}

#[derive(Clone)]
struct ComposeContext {
    compose_file: PathBuf,
    env_file: Option<PathBuf>,
    project_directory: Option<String>,
}

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];
const RESTART_KEYS: [&str; 1] = ["co.architect.composer.restart"];

#[derive(Debug)]
enum ComposeAction {
    Run,
    Restart,
}

impl ComposeAction {
    fn as_gerund(&self) -> &str {
        match self {
            ComposeAction::Run => "running",
            ComposeAction::Restart => "restarting",
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    let project_directory = match args.project_directory {
        Some(pwd) => Some(pwd.to_owned()),
        None => match std::env::var("COMPOSE_PROJECT_DIRECTORY") {
            Ok(pwd) => Some(pwd.to_owned()),
            Err(VarError::NotPresent) => None,
            Err(_) => bail!("COMPOSE_PROJECT_DIRECTORY was specified but not utf-8"),
        },
    };
    let context = ComposeContext {
        compose_file: args.compose_file.to_owned(),
        env_file: args.env_file.map(|f| f.to_owned()),
        project_directory,
    };
    let compose = load_compose_config(&context, Some("*")).await?;
    let mut scheduler = JoinSet::new();
    for (name, service) in &compose.services {
        debug!("parsing service: {name}");
        if let Some(service) = service.as_ref() {
            if let Some(labels) = service.labels.as_ref() {
                for (key, value) in labels {
                    let action = if RUN_KEYS.contains(&key.as_str()) {
                        ComposeAction::Run
                    } else if RESTART_KEYS.contains(&key.as_str()) {
                        ComposeAction::Restart
                    } else {
                        continue;
                    };
                    let schedule: Schedule = value.parse().with_context(|| {
                        format!("while parsing cron expression: {value}")
                    })?;
                    info!("service {name} has a {action} schedule: {schedule}");
                    scheduler.spawn(run_on_schedule(
                        context.clone(),
                        action,
                        schedule,
                        name.clone(),
                    ));
                    // for up in schedule.upcoming(Utc).take(3) {
                    //     println!("  -> {}", up);
                    // }
                }
            }
        }
    }
    scheduler.join_all().await;
    Ok(())
}

fn compose_command<S: AsRef<str>>(
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

async fn _load_compose_profiles(context: &ComposeContext) -> Result<Vec<String>> {
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

async fn load_compose_config<S: AsRef<str>>(
    context: &ComposeContext,
    profile: Option<S>,
) -> Result<compose_types::Compose> {
    let mut cmd = compose_command(context, profile);
    let out =
        cmd.arg("config").output().await.with_context(|| "docker compose config")?;
    let stdout_s = std::str::from_utf8(&out.stdout).unwrap_or("<invalid utf-8>");
    debug!("compose config:\r\n{stdout_s}");
    let compose: compose_types::Compose =
        serde_yaml::from_slice(&out.stdout).context("parsing compose config")?;
    Ok(compose)
}

async fn run_on_schedule(
    context: ComposeContext,
    action: ComposeAction,
    schedule: Schedule,
    service: String,
) {
    loop {
        if let Some(up) = schedule.upcoming(Utc).next() {
            let duration_from_now = (up - Utc::now()).to_std().unwrap();
            info!(
                "next {action} for {service} in {} at {up}",
                humantime::format_duration(duration_from_now)
            );
            tokio::time::sleep(duration_from_now).await;
            let now = Utc::now();
            if (now - up).abs() > chrono::Duration::seconds(1) {
                error!("time skew for scheduled {action}: expected {up}, is {now}");
            }
            info!("{} {service}...", action.as_gerund());
            let mut cmd = compose_command(&context, None::<&str>);
            match action {
                ComposeAction::Run => cmd.arg("run").arg("--rm"),
                ComposeAction::Restart => cmd.arg("restart"),
            };
            match cmd.arg(&service).output().await {
                Err(e) => {
                    error!("error {} {service}: {e}", action.as_gerund());
                }
                Ok(out) => {
                    let stdout_s =
                        std::str::from_utf8(&out.stdout).unwrap_or("<invalid utf-8>");
                    let stderr_s =
                        std::str::from_utf8(&out.stderr).unwrap_or("<invalid utf-8>");
                    for line in stdout_s.lines() {
                        debug!("{service} stdout: {line}");
                    }
                    for line in stderr_s.lines() {
                        debug!("{service} stderr: {line}");
                    }
                    if !out.status.success() {
                        error!("{action} {service} failed with status {}", out.status,);
                    } else {
                        info!("{} {service} succeeded", action.as_gerund());
                    }
                }
            }
        } else {
            warn!("no more upcoming {action}s for {service}, task exiting");
            break;
        }
    }
}
