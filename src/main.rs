use crate::compose::*;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Parser, Subcommand};
use cron::Schedule;
use log::{debug, error, info, warn};
use serde_json::json;
use std::{
    fs::File,
    path::PathBuf,
    sync::{mpsc, Arc},
};
use tokio::task::JoinSet;

mod certificate_monitor;
mod compose;
mod compose_types;
mod container_monitor;
mod install_commands;
mod scheduler;
mod status;
mod status_command;
mod status_server;
mod system_monitor;

/// Scheduler for docker-compose services
///
/// Add the `co.architect.composer.run` or `co.architect.composer.restart`
/// labels to your services with a cron expression (Quartz-compatible, e.g.
/// seconds field is first) to schedule runs or restarts.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // CR alee: try [docker-]compose.{yml,yaml}
    #[clap(short = 'f', default_value = "compose.yml")]
    compose_file: PathBuf,
    /// Specify the environment file to use for docker compose commands.
    #[clap(long)]
    env_file: Option<PathBuf>,
    /// Specify the --project-directory option for docker compose commands.
    #[clap(long, env = "COMPOSE_PROJECT_DIRECTORY")]
    project_directory: Option<String>,
    /// Put stdout/stderr from job runs to this directory instead of
    /// logging them to console.
    #[clap(long, env = "COMPOSE_RUN_LOGS")]
    run_logs: Option<PathBuf>,
    /// Optional hostname to identify the host.
    /// You may also set this via env var HOST.
    #[clap(long, env = "HOST")]
    hostname: Option<String>,
    /// Port to run the status server on.  Defaults to 10080.
    /// You may also set this via env var STATUS_PORT.
    #[clap(long, env = "STATUS_PORT", default_value = "10080")]
    status_port: u16,
    /// If set to "true" or "1", run the container monitor
    #[clap(long, env = "CONTAINER_MONITOR")]
    container_monitor: Option<String>,
    /// If set, run the system monitor (CPU, memory, disk alerting) using
    /// the specified config file.  Or, set to "true" or "1" to use the
    /// default system monitor config.
    ///
    /// The application must have root access to the host.
    ///
    /// You may also set this via env var SYSTEM_MONITOR.
    #[clap(long, env = "SYSTEM_MONITOR")]
    system_monitor: Option<String>,
    /// Path to certificate monitor configuration file.
    /// You may also set this via env var CERTIFICATE_MONITOR.
    #[clap(long, env = "CERTIFICATE_MONITOR")]
    certificate_monitor: Option<String>,
    /// Run `docker image prune -f` on the provided schedule.
    #[clap(long, env = "PRUNE_IMAGES")]
    prune_images: Option<String>,
    /// Slack webhook URL for notifications.
    /// You may also set this via env var SLACK_WEBHOOK_URL.
    ///
    /// If set, jobs can opt in to slack notifications with the label
    /// co.architect.composer.notify.slack=true
    #[clap(long, env = "SLACK_WEBHOOK_URL")]
    slack_webhook_url: Option<String>,
    #[clap(long, env = "SLACK_WEBHOOK_ON_ERROR_URL")]
    slack_webhook_on_error_url: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Check SSL certificates once and print status to stdout
    CheckCertificates {
        /// Path to certificate monitor configuration file.
        /// If not provided, will use --certificate-monitor or CERTIFICATE_MONITOR env var.
        #[clap(short = 'c', long)]
        config: Option<String>,
    },
    /// Show status of all services: profile, service name, type (service/job), and UP/DOWN status
    Status,
    /// Install additional components
    #[command(subcommand)]
    Install(install_commands::InstallCommands),
}

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];
const RESTART_KEYS: [&str; 1] = ["co.architect.composer.restart"];

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Cli::parse();
    let hostname = args.hostname.clone().unwrap_or_else(|| {
        hostname::get()
            .expect("failed to get system hostname")
            .to_string_lossy()
            .to_string()
    });

    // Handle subcommands
    if let Some(command) = args.command {
        return match command {
            Commands::CheckCertificates { config } => {
                let cert_config = config.or(args.certificate_monitor);
                if let Some(cert_config) = cert_config {
                    let config: certificate_monitor::CertificateMonitorConfig =
                        serde_yaml::from_reader(File::open(&cert_config)
                            .with_context(|| format!("failed to open certificate monitor config file: {cert_config}"))?
                        ).with_context(|| format!("failed to parse certificate monitor config file: {cert_config}"))?;
                    certificate_monitor::check_once(hostname, config).await
                } else {
                    bail!("No certificate monitor configuration provided. Use --config, --certificate-monitor, or CERTIFICATE_MONITOR env var");
                }
            }
            Commands::Status => {
                let context = ComposeContext {
                    compose_file: args.compose_file.to_owned(),
                    env_file: args.env_file.map(|f| f.to_owned()),
                    project_directory: args.project_directory.clone(),
                    hostname,
                };
                status_command::show_status(&context).await
            }
            Commands::Install(command) => install_commands::install(command),
        };
    }
    let project_directory = args.project_directory.clone();
    let run_logs = args.run_logs.clone();
    if let Some(run_logs) = run_logs.as_ref() {
        if !run_logs.try_exists()? {
            info!("creating run logs directory: {}", run_logs.display());
            std::fs::create_dir_all(run_logs)?;
        }
        if !run_logs.is_dir() {
            bail!("run logs path is not a directory: {}", run_logs.display());
        }
    }
    let sanitize_url = |url: String| {
        if url.trim().is_empty() {
            warn!("ignoring malformed slack webhook URL: {url}");
            None
        } else {
            Some(url)
        }
    };
    let slack_webhook_url =
        args.slack_webhook_url.as_ref().map(|s| s.clone()).and_then(sanitize_url);
    let slack_webhook_on_error_url = args
        .slack_webhook_on_error_url
        .as_ref()
        .map(|s| s.clone())
        .and_then(sanitize_url);
    let prune_images = args.prune_images.clone();
    let context = ComposeContext {
        compose_file: args.compose_file.clone(),
        env_file: args.env_file.clone(),
        project_directory,
        hostname,
    };

    // Implement file watching and reload loop
    let compose_file = context.compose_file.clone();
    loop {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

        // Spawn file watcher task
        let file_watcher_handle = {
            let compose_file = compose_file.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = watch_compose_file(compose_file, tx).await {
                    warn!(
                        "file watcher error (will continue without auto-reload): {e:?}"
                    );
                }
            })
        };

        // Run scheduler setup - this returns the JoinSet
        let mut scheduler = run(
            context.clone(),
            &args,
            run_logs.clone(),
            slack_webhook_url.clone(),
            slack_webhook_on_error_url.clone(),
            prune_images.clone(),
        )
        .await?;

        // Wait for either scheduler completion or file change
        tokio::select! {
            result = scheduler.join_next() => {
                // A task completed (shouldn't happen normally, tasks run forever)
                if let Some(Ok(())) = result {
                    // Task completed successfully, continue
                    continue;
                } else if let Some(Err(e)) = result {
                    // Task panicked, propagate error
                    return Err(anyhow!("scheduler task error: {e:?}"));
                } else {
                    // No more tasks (shouldn't happen)
                    error!("all scheduler tasks completed unexpectedly");
                    return Err(anyhow!("all scheduler tasks completed unexpectedly"));
                }
            }
            _ = rx.recv() => {
                // File changed, abort all tasks and restart
                info!("compose file changed, reloading...");

                // Abort all tasks in the scheduler by shutting it down
                scheduler.shutdown().await;

                // Wait for tasks to finish aborting
                while scheduler.join_next().await.is_some() {}

                // Abort file watcher
                file_watcher_handle.abort();

                // Wait a moment before restarting
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                // Loop will restart run()
                continue;
            }
        }
    }
}

async fn watch_compose_file(
    compose_file: PathBuf,
    change_tx: tokio::sync::mpsc::Sender<()>,
) -> Result<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        if let Ok(event) = result {
            if let Err(e) = tx.send(event) {
                error!("failed to send file event: {e}");
            }
        } else if let Err(e) = result {
            error!("file watcher error: {e}");
        }
    })?;

    // Watch the directory containing the compose file, not the file itself
    // (watching the file directly might not work if it gets replaced)
    let watch_path = compose_file.parent().unwrap_or_else(|| std::path::Path::new("."));
    watcher.watch(watch_path, RecursiveMode::NonRecursive)?;
    info!("watching compose file: {}", compose_file.display());

    let mut last_event_time: Option<std::time::Instant> = None;
    let debounce_duration = Duration::from_millis(500);

    loop {
        // Check for events with timeout
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                // Check if this event is for our compose file
                if event.paths.iter().any(|path| path == &compose_file) {
                    // Only trigger on modify/create/remove events
                    match event.kind {
                        EventKind::Modify(_)
                        | EventKind::Create(_)
                        | EventKind::Remove(_) => {
                            last_event_time = Some(std::time::Instant::now());
                        }
                        _ => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Check if enough time has passed since last event (debouncing)
                if let Some(last_time) = last_event_time {
                    if last_time.elapsed() >= debounce_duration {
                        // Debounce period passed, trigger reload
                        info!("compose file changed, triggering reload");
                        let _ = change_tx.send(()).await;
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                error!("file watcher channel error: {e}");
                return Err(anyhow!("file watcher channel error: {e}"));
            }
        }
    }
}

async fn run(
    context: ComposeContext,
    args: &Cli,
    run_logs: Option<PathBuf>,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
    prune_images: Option<String>,
) -> Result<JoinSet<()>> {
    let compose = load_compose_config(&context, Some("*")).await?;
    let mut scheduler = JoinSet::new();
    let mut monitor_containers = vec![];
    let services: Vec<(String, Option<crate::compose_types::Service>)> = compose
        .services
        .iter()
        .map(|(name, service)| (name.clone(), (*service).clone()))
        .collect();
    for (name, service) in services {
        debug!("parsing service: {name}");
        let mut should_monitor = true;
        if let Some(service) = service.as_ref() {
            if let Some(labels) = service.labels.as_ref() {
                let maybe_slack_webhook_url = if labels
                    .get("co.architect.composer.notify.slack")
                    .is_some_and(|v| v == "true" || v == "1")
                {
                    slack_webhook_url.clone()
                } else {
                    None
                };
                let maybe_slack_webhook_on_error_url = if labels
                    .get("co.architect.composer.notify.slack.on-error")
                    .is_some_and(|v| v == "true" || v == "1")
                {
                    slack_webhook_on_error_url.clone()
                } else {
                    None
                };
                for (key, value) in labels {
                    let action = if RUN_KEYS.contains(&key.as_str()) {
                        // don't monitor services that are run one-shot
                        should_monitor = false;
                        ComposeAction::Run
                    } else if RESTART_KEYS.contains(&key.as_str()) {
                        ComposeAction::Restart
                    } else {
                        continue;
                    };
                    if value != "manual" {
                        let schedule: Schedule = value.parse().with_context(|| {
                            format!("while parsing cron expression: {value}")
                        })?;
                        let service_name = name.clone();
                        let run_logs_clone = run_logs.clone();
                        let context_clone = context.clone();
                        let slack_webhook_url_clone = maybe_slack_webhook_url.clone();
                        let slack_webhook_on_error_url_clone =
                            maybe_slack_webhook_on_error_url.clone();
                        info!(
                            "service {service_name} has a {action} schedule: {schedule}"
                        );
                        scheduler.spawn(async move {
                            run_on_schedule(
                                context_clone,
                                action,
                                schedule,
                                service_name,
                                run_logs_clone,
                                slack_webhook_url_clone,
                                slack_webhook_on_error_url_clone,
                            )
                            .await;
                        });
                    }
                }
            }
        }
        if should_monitor {
            monitor_containers.push(name.clone());
        }
    }
    // add container monitor task
    if args.container_monitor.as_ref().is_some_and(|v| v == "true" || v == "1") {
        let context = context.clone();
        let slack_webhook_url = slack_webhook_url.clone();
        let slack_webhook_on_error_url = slack_webhook_on_error_url.clone();
        scheduler.spawn(async move {
            if let Err(e) = container_monitor::run(
                context.clone(),
                monitor_containers,
                slack_webhook_url.clone(),
                slack_webhook_on_error_url.clone(),
            )
            .await
            {
                panic!("while running container monitor: {e:?}");
            }
        });
    }
    // add system monitor task
    if let Some(config_file) = &args.system_monitor {
        let config: system_monitor::SystemMonitorConfig =
            if config_file.to_lowercase() == "true" || config_file == "1" {
                serde_yaml::from_str("{}")?
            } else {
                let config_s = std::fs::read_to_string(config_file)?;
                serde_yaml::from_str(&config_s)?
            };
        let context = context.clone();
        let slack_webhook_url = slack_webhook_url.clone();
        let slack_webhook_on_error_url = slack_webhook_on_error_url.clone();
        scheduler.spawn(async move {
            if let Err(e) = system_monitor::run(
                context.hostname,
                config,
                slack_webhook_url,
                slack_webhook_on_error_url,
            )
            .await
            {
                panic!("while running system monitor: {e:?}");
            }
        });
    }
    // add certificate monitor task
    if let Some(config_file) = &args.certificate_monitor {
        let config_s = std::fs::read_to_string(config_file)?;
        let config: certificate_monitor::CertificateMonitorConfig =
            serde_yaml::from_str(&config_s)?;
        let context = context.clone();
        let slack_webhook_url = slack_webhook_url.clone();
        let slack_webhook_on_error_url = slack_webhook_on_error_url.clone();
        scheduler.spawn(async move {
            if let Err(e) = certificate_monitor::run(
                context.hostname,
                config,
                slack_webhook_url,
                slack_webhook_on_error_url,
            )
            .await
            {
                panic!("while running certificate monitor: {e:?}");
            }
        });
    }
    // add pruning tasks
    if let Some(prune_images) = prune_images {
        let schedule: Schedule = prune_images
            .parse()
            .with_context(|| format!("while parsing cron expression: {prune_images}"))?;
        let context_clone = context.clone();
        let slack_webhook_url_clone = slack_webhook_url.clone();
        let slack_webhook_on_error_url_clone = slack_webhook_on_error_url.clone();
        scheduler.spawn(async move {
            scheduler::run_command_on_schedule(
                context_clone,
                schedule,
                "prune images",
                "docker",
                &["image", "prune", "-f"],
                slack_webhook_url_clone,
                slack_webhook_on_error_url_clone,
            )
            .await;
        });
    }
    // add status server task
    {
        let context = Arc::new(context.clone());
        let status_port = args.status_port;
        scheduler.spawn(async move {
            if let Err(e) = status_server::run_status_server(context, status_port).await {
                error!("error running status server: {e:?}");
            }
        });
    }
    Ok(scheduler)
}

async fn run_on_schedule(
    context: ComposeContext,
    action: ComposeAction,
    schedule: Schedule,
    service: String,
    run_logs: Option<PathBuf>,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
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
            "next {action} for {service} in {}",
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
        if let Some(webhook_url) = slack_webhook_url.as_deref() {
            if let Err(e) = notify_slack(
                webhook_url,
                &context.hostname,
                &service,
                action,
                out.status.success(),
                up,
            )
            .await
            {
                error!("error notifying slack: {e:?}");
            }
        }
        if let Some(webhook_url) = slack_webhook_on_error_url.as_deref() {
            if !out.status.success() {
                if let Err(e) = notify_slack(
                    webhook_url,
                    &context.hostname,
                    &service,
                    action,
                    out.status.success(),
                    up,
                )
                .await
                {
                    error!("error notifying slack: {e:?}");
                }
            }
        }
    }
}

async fn notify_slack(
    webhook_url: &str,
    hostname: &str,
    service: &str,
    action: ComposeAction,
    exit_success: bool,
    next_action_at: DateTime<Utc>,
) -> Result<()> {
    let mut lines = vec![];
    lines.push(format!(
        "{} *{hostname}.{service}* {}",
        if exit_success { "✅" } else { "❌" },
        action.as_past_participle(),
    ));
    // if exit_success && !stdout_s.is_empty() {
    //     lines.push(format!("```\n{stdout_s}\n```"));
    // } else if !stderr_s.is_empty() {
    //     lines.push(format!("```\n{stderr_s}\n```"));
    // }
    lines.push(format!(
        "<!date^{}^Next {action} at {{date_num}} {{time_secs}}|Next {action} at {next_action_at}>",
        next_action_at.timestamp()
    ));
    let text =
        lines.iter().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
    let client = reqwest::Client::new();
    let res = client.post(webhook_url).json(&json!({ "text": text })).send().await?;
    if !res.status().is_success() {
        let err_body = res.text().await.context("reading response body")?;
        return Err(anyhow!("{err_body}"));
    }
    Ok(())
}
