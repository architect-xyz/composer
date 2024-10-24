use crate::compose::*;
use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use clap::Parser;
use cron::Schedule;
use log::{debug, error, info, warn};
use std::{env::VarError, fs::File, path::PathBuf};
use tokio::task::JoinSet;

mod compose;
mod compose_types;

/// Scheduler for docker-compose services
///
/// Add the `co.architect.composer.run` or `co.architect.composer.restart`
/// labels to your services with a cron expression (Quartz-compatible, e.g.
/// seconds field is first) to schedule runs or restarts.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    // CR alee: try [docker-]compose.{yml,yaml}
    #[clap(short = 'f', default_value = "compose.yml")]
    compose_file: PathBuf,
    /// Specify the environment file to use for docker compose commands.
    #[clap(long)]
    env_file: Option<PathBuf>,
    /// Specify the --project-directory option for docker compose commands.
    #[clap(long)]
    project_directory: Option<String>,
    /// Put stdout/stderr from job runs to this directory instead of
    /// logging them to console.
    #[clap(long)]
    run_logs: Option<PathBuf>,
}

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];
const RESTART_KEYS: [&str; 1] = ["co.architect.composer.restart"];

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
    let run_logs = match args.run_logs {
        Some(run_logs) => Some(run_logs.to_owned()),
        None => match std::env::var("COMPOSE_RUN_LOGS") {
            Ok(run_logs) => Some(run_logs.into()),
            Err(VarError::NotPresent) => None,
            Err(_) => bail!("COMPOSE_RUN_LOGS was specified but not utf-8"),
        },
    };
    if let Some(run_logs) = run_logs.as_ref() {
        if !run_logs.try_exists()? {
            info!("creating run logs directory: {}", run_logs.display());
            std::fs::create_dir_all(run_logs)?;
        }
        if !run_logs.is_dir() {
            bail!("run logs path is not a directory: {}", run_logs.display());
        }
    }
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
                        run_logs.clone(),
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

async fn run_on_schedule(
    context: ComposeContext,
    action: ComposeAction,
    schedule: Schedule,
    service: String,
    run_logs: Option<PathBuf>,
) {
    loop {
        let up = match schedule.upcoming(Utc).next() {
            Some(up) => up,
            None => {
                warn!("no more upcoming {action}s for {service}, task exiting");
                break;
            }
        };
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
        if let Some(run_logs) = run_logs.as_ref() {
            let std_file = |suffix: &str| {
                run_logs.join(format!(
                    "{}_{service}.{suffix}.log",
                    now.to_rfc3339_opts(SecondsFormat::Secs, true)
                ))
            };
            let stdout_file = std_file("stdout");
            let stderr_file = std_file("stderr");
            if let Ok(stdout) = File::create(&stdout_file) {
                cmd.stdout(stdout);
            } else {
                error!(
                    "could not open stdout file for writing: {}",
                    stdout_file.display()
                );
            }
            if let Ok(stderr) = File::create(&stderr_file) {
                cmd.stderr(stderr);
            } else {
                error!(
                    "could not open stderr file for writing: {}",
                    stderr_file.display()
                );
            }
        }
        match action {
            ComposeAction::Run => cmd.arg("run").arg("--rm"),
            ComposeAction::Restart => cmd.arg("restart"),
        };
        let child = match cmd.arg(&service).spawn() {
            Ok(child) => child,
            Err(e) => {
                error!("error {} {service}: {e}", action.as_gerund());
                continue;
            }
        };
        let out = match child.wait_with_output().await {
            Ok(out) => out,
            Err(e) => {
                error!("error while {} {service}: {e}", action.as_gerund());
                continue;
            }
        };
        if run_logs.is_none() {
            let stdout_s = std::str::from_utf8(&out.stdout).unwrap_or("<invalid utf-8>");
            let stderr_s = std::str::from_utf8(&out.stderr).unwrap_or("<invalid utf-8>");
            for line in stdout_s.lines() {
                debug!("{service} stdout: {line}");
            }
            for line in stderr_s.lines() {
                debug!("{service} stderr: {line}");
            }
        }
        if !out.status.success() {
            error!("{action} {service} failed with status {}", out.status,);
        } else {
            info!("{} {service} succeeded", action.as_gerund());
        }
    }
}
