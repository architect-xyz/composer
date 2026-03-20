use crate::compose::*;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use cron::Schedule;
use log::{debug, error, info, warn};
use serde_json::json;
use std::{fs::File, path::PathBuf};
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

    /// Compose file path. If not specified, auto-detects compose.yml,
    /// compose.yaml, docker-compose.yml, or docker-compose.yaml in the
    /// current directory.
    #[clap(short = 'f')]
    compose_file: Option<PathBuf>,
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
    /// If set to "true" or "1", watch the compose file for
    /// changes and automatically restart the scheduler.
    ///
    /// Note that restarting the scheduler may cause some jobs
    /// or scheduled restarts to be lost, as a scheduler restart
    /// typically causes ~1s of downtime.
    #[clap(long, env = "WATCH_COMPOSE_FILE")]
    watch_compose_file: Option<String>,
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
    /// Remove all installed components (aliases, service)
    Uninstall,
    /// Update composer to the latest version
    Update,
}

const COMPOSE_FILE_CANDIDATES: &[&str] = &[
    "compose.yml",
    "compose.yaml",
    "docker-compose.yml",
    "docker-compose.yaml",
];

fn resolve_compose_file(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let found: Vec<PathBuf> = COMPOSE_FILE_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect();
    match found.len() {
        0 => bail!("No compose.yml or docker-compose.yml found in the current directory."),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => {
            let options: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
            let choice = inquire::Select::new("Multiple compose files found:", options)
                .prompt()
                .context("failed to select compose file")?;
            Ok(PathBuf::from(choice))
        }
    }
}

const RUN_KEY: &str = "co.architect.composer.run";
const RESTART_KEY: &str = "co.architect.composer.restart";

/// Check if a label key is a schedule label and return the corresponding action
/// and optional schedule name (the suffix after the base key).
/// Matches exact keys (e.g., `co.architect.composer.run`) and suffixed keys
/// (e.g., `co.architect.composer.run.morning`).
fn get_schedule_action(key: &str) -> Option<(ComposeAction, Option<&str>)> {
    if key == RUN_KEY {
        Some((ComposeAction::Run, None))
    } else if let Some(suffix) = key.strip_prefix(&format!("{RUN_KEY}.")) {
        Some((ComposeAction::Run, Some(suffix)))
    } else if key == RESTART_KEY {
        Some((ComposeAction::Restart, None))
    } else if let Some(suffix) = key.strip_prefix(&format!("{RESTART_KEY}.")) {
        Some((ComposeAction::Restart, Some(suffix)))
    } else {
        None
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Cli::parse();
    let hostname = args.hostname.unwrap_or_else(|| {
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
                let compose_file = resolve_compose_file(args.compose_file)?;
                let context = ComposeContext {
                    compose_file,
                    env_file: args.env_file.map(|f| f.to_owned()),
                    project_directory: args.project_directory.clone(),
                    hostname,
                };
                status_command::show_status(&context).await
            }
            Commands::Install(command) => install_commands::install(command),
            Commands::Uninstall => install_commands::uninstall(),
            Commands::Update => install_commands::update(),
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
    let container_monitor =
        args.container_monitor.is_some_and(|v| v == "true" || v == "1");
    let compose_file = std::fs::canonicalize(resolve_compose_file(args.compose_file)?)?;
    let context = ComposeContext {
        compose_file,
        env_file: args.env_file.map(|f| f.to_owned()),
        project_directory,
        hostname,
    };
    let mut aux_tasks = JoinSet::new();

    // add system monitor task
    if let Some(config_file) = args.system_monitor {
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
        aux_tasks.spawn(async move {
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
    if let Some(config_file) = args.certificate_monitor {
        let config_s = std::fs::read_to_string(config_file)?;
        let config: certificate_monitor::CertificateMonitorConfig =
            serde_yaml::from_str(&config_s)?;
        let context = context.clone();
        let slack_webhook_url = slack_webhook_url.clone();
        let slack_webhook_on_error_url = slack_webhook_on_error_url.clone();
        aux_tasks.spawn(async move {
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
        aux_tasks.spawn(scheduler::run_command_on_schedule(
            context.clone(),
            schedule,
            "prune images",
            "docker",
            &["image", "prune", "-f"],
            slack_webhook_url.clone(),
            slack_webhook_on_error_url.clone(),
            chrono_tz::UTC,
        ));
    }

    // start main tasks which depend on the compose file; if the compose
    // file is observed to change, restart the tasks.
    let (changed_tx, mut changed_rx) = tokio::sync::mpsc::channel(1);
    if args.watch_compose_file.is_some_and(|v| v == "true" || v == "1") {
        let compose_file = context.compose_file.clone();
        tokio::task::spawn_blocking(move || {
            watch_compose_file(compose_file, changed_tx.clone())
                .expect("error watching compose file");
        });
    }
    let mut compose = load_compose_config(&context, Some("*")).await?;
    'outer: loop {
        info!("compose config reloaded");
        info!("starting scheduler...");
        let mut tasks = run_tasks(
            &context,
            &compose,
            slack_webhook_url.clone(),
            slack_webhook_on_error_url.clone(),
            run_logs.clone(),
            container_monitor,
        )?;

        // add status server task
        {
            let context = context.clone();
            tasks.spawn(async move {
                if let Err(e) =
                    status_server::run_status_server(context, compose, args.status_port)
                        .await
                {
                    error!("error running status server: {e:?}");
                }
            });
        }

        'inner: loop {
            info!("scheduler started, listening for compose file changes");
            changed_rx.recv().await.ok_or_else(|| anyhow!("watch channel closed"))?;
            // compose file changed, attempt to reload config
            // if failed, do NOTHING; continue with the last good config
            match load_compose_config(&context, Some("*")).await {
                Ok(new_compose) => {
                    compose = new_compose;
                    // new config is good, stop all tasks and restart
                    info!("stopping scheduler...");
                    tasks.shutdown().await;
                    // flush out any additional changes, we know
                    while let Ok(()) = changed_rx.try_recv() {}
                    continue 'outer;
                }
                Err(e) => {
                    error!("error reloading compose config: {e:?}");
                    info!("continuing with last good config");
                    continue 'inner;
                }
            }
        }
    }
}

fn watch_compose_file(
    compose_file: PathBuf,
    changed_tx: tokio::sync::mpsc::Sender<()>,
) -> Result<()> {
    use notify::{Config, EventKind, PollWatcher, RecursiveMode, Watcher};
    use std::{path::Path, time::Duration};

    let (tx, rx) = std::sync::mpsc::channel();
    // NB: only the PollWatcher is reliable w.r.t. Docker volume mounts cross-platform
    let mut watcher = PollWatcher::new(
        move |res| {
            debug!("compose file event: {res:?}");
            tx.send(res).unwrap();
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )?;

    let mut file_contents = std::fs::read_to_string(&compose_file)?;

    // Watch the directory containing the compose file, not the file itself;
    // watching the file directly might not work if it gets replaced.
    let watch_path = match compose_file.parent() {
        Some(parent) => parent,
        None => Path::new("/"),
    };
    watcher.watch(watch_path, RecursiveMode::NonRecursive)?;
    info!(
        "watching compose file {} via {}",
        compose_file.display(),
        watch_path.display()
    );

    loop {
        let event = rx.recv()??;
        if matches!(
            event.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        ) && event.paths.iter().any(|path| path == &compose_file)
        {
            // check if the file contents have actually changed;
            // for file deletion, the read will fail and we
            // will assume that the file has changed.
            //
            // the reason not to use notify's compare_contents
            // feature is that we are watching the parent directory
            // and only need to compare contents on this particular
            // file, so we do it manually behind this filter.
            match std::fs::read_to_string(&compose_file) {
                Ok(new_file_contents) => {
                    if new_file_contents != file_contents {
                        file_contents = new_file_contents;
                        changed_tx.blocking_send(()).unwrap();
                    }
                }
                Err(_) => {
                    changed_tx.blocking_send(()).unwrap();
                }
            }
        }
    }
}

fn run_tasks(
    context: &ComposeContext,
    compose: &compose_types::Compose,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
    run_logs: Option<PathBuf>,
    container_monitor: bool,
) -> Result<JoinSet<()>> {
    let mut tasks = JoinSet::new();
    let mut monitor_containers = vec![];
    for (name, service) in &compose.services {
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
                let schedule_timezone: Tz = labels
                    .get("co.architect.composer.tz")
                    .map(|tz_str| tz_str.parse::<Tz>())
                    .transpose()?
                    .unwrap_or(chrono_tz::UTC);

                for (key, value) in labels {
                    let (action, schedule_name) = match get_schedule_action(key) {
                        Some((ComposeAction::Run, schedule_name)) => {
                            // don't monitor services that are run one-shot
                            should_monitor = false;
                            (ComposeAction::Run, schedule_name)
                        }
                        Some((ComposeAction::Restart, schedule_name)) => {
                            (ComposeAction::Restart, schedule_name)
                        }
                        None => continue,
                    };
                    if value != "manual" {
                        let schedule: Schedule = value.parse().with_context(|| {
                            format!("while parsing cron expression: {value}")
                        })?;
                        let schedule_name_display =
                            schedule_name.map(|s| format!(" ({s})")).unwrap_or_default();
                        info!("service {name} has a {action} schedule{schedule_name_display}: {schedule} ({schedule_timezone})");
                        tasks.spawn(run_on_schedule(
                            context.clone(),
                            action,
                            schedule,
                            schedule_timezone,
                            name.clone(),
                            schedule_name.map(|s| s.to_owned()),
                            run_logs.clone(),
                            maybe_slack_webhook_url.clone(),
                            maybe_slack_webhook_on_error_url.clone(),
                        ));
                    }
                    // for up in schedule.upcoming(Utc).take(3) {
                    //     println!("  -> {}", up);
                    // }
                }
            }
        }
        if should_monitor {
            monitor_containers.push(name.clone());
        }
    }
    // add container monitor task
    if container_monitor {
        let context = context.clone();
        let slack_webhook_url = slack_webhook_url.clone();
        let slack_webhook_on_error_url = slack_webhook_on_error_url.clone();
        tasks.spawn(async move {
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
    Ok(tasks)
}

async fn run_on_schedule(
    context: ComposeContext,
    action: ComposeAction,
    schedule: Schedule,
    schedule_timezone: Tz,
    service: String,
    schedule_name: Option<String>,
    run_logs: Option<PathBuf>,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
) {
    // Format service name with optional schedule name for log messages
    let service_display = schedule_name
        .as_ref()
        .map(|s| format!("{service} ({s})"))
        .unwrap_or_else(|| service.clone());
    loop {
        let up = match schedule.upcoming(schedule_timezone).next() {
            Some(up) => up,
            None => {
                warn!("no more upcoming {action}s for {service_display}, task exiting");
                break;
            }
        };
        let up_utc = up.with_timezone(&Utc);
        let duration_from_now = (up_utc - Utc::now()).to_std().unwrap();
        info!(
            "next {action} for {service_display} in {}",
            humantime::format_duration(duration_from_now)
        );
        tokio::time::sleep(duration_from_now).await;
        let now = Utc::now();
        if (now - up_utc).abs() > chrono::Duration::seconds(1) {
            error!("time skew for scheduled {action}: expected {up_utc}, is {now}");
        }
        info!("{} {service_display}...", action.as_gerund());
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
                error!("error {} {service_display}: {e}", action.as_gerund());
                continue;
            }
        };
        let out = match child.wait_with_output().await {
            Ok(out) => out,
            Err(e) => {
                error!("error while {} {service_display}: {e}", action.as_gerund());
                continue;
            }
        };
        if run_logs.is_none() {
            let stdout_s = std::str::from_utf8(&out.stdout).unwrap_or("<invalid utf-8>");
            let stderr_s = std::str::from_utf8(&out.stderr).unwrap_or("<invalid utf-8>");
            for line in stdout_s.lines() {
                debug!("{service_display} stdout: {line}");
            }
            for line in stderr_s.lines() {
                debug!("{service_display} stderr: {line}");
            }
        }
        if !out.status.success() {
            error!("{action} {service_display} failed with status {}", out.status,);
        } else {
            info!("{} {service_display} succeeded", action.as_gerund());
        }
        if let Some(webhook_url) = slack_webhook_url.as_deref() {
            if let Err(e) = notify_slack(
                webhook_url,
                &context.hostname,
                &service,
                action,
                out.status.success(),
                up_utc,
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
                    up_utc,
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
