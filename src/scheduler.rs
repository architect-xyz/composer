use crate::compose::ComposeContext;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use log::{error, info, warn};
use serde_json::json;
use std::process::Stdio;

pub async fn run_command_on_schedule(
    context: ComposeContext,
    schedule: Schedule,
    action: &str,
    command: &str,
    args: &[&str],
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
) {
    loop {
        let up = match schedule.upcoming(Utc).next() {
            Some(up) => up,
            None => {
                warn!("no more scheduled times for {action}, task exiting");
                break;
            }
        };
        let duration_from_now = (up - Utc::now()).to_std().unwrap();
        info!("next {action} in {}", humantime::format_duration(duration_from_now));
        tokio::time::sleep(duration_from_now).await;
        let now = Utc::now();
        if (now - up).abs() > chrono::Duration::seconds(1) {
            error!("time skew for scheduled {action}: expected {up}, is {now}");
        }
        let args_s = args.iter().cloned().collect::<Vec<_>>().join(" ");
        info!("{action}: running `{command} {args_s}`...");
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                error!("error for {action}: {e}");
                if let Some(url) = slack_webhook_on_error_url.as_deref() {
                    if let Err(e) = notify_slack(
                        url,
                        &context.hostname,
                        action,
                        "".to_string(),
                        e.to_string(),
                        false,
                        up,
                    )
                    .await
                    {
                        error!("error notifying slack: {e:?}");
                    }
                }
                continue;
            }
        };
        let out = match child.wait_with_output().await {
            Ok(out) => out,
            Err(e) => {
                error!("error for {action}: {e}");
                if let Some(url) = slack_webhook_on_error_url.as_deref() {
                    if let Err(e) = notify_slack(
                        url,
                        &context.hostname,
                        action,
                        "".to_string(),
                        e.to_string(),
                        false,
                        up,
                    )
                    .await
                    {
                        error!("error notifying slack: {e:?}");
                    }
                }
                continue;
            }
        };
        if !out.status.success() {
            error!("{action} failed with status {}", out.status,);
        } else {
            info!("{action} succeeded");
        }
        if let Some(webhook_url) = slack_webhook_url.as_deref() {
            if let Err(e) = notify_slack(
                webhook_url,
                &context.hostname,
                action,
                String::from_utf8(out.stdout.clone()).unwrap_or_default(),
                String::from_utf8(out.stderr.clone()).unwrap_or_default(),
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
                    action,
                    String::from_utf8(out.stdout).unwrap_or_default(),
                    String::from_utf8(out.stderr).unwrap_or_default(),
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
    action: &str,
    stdout_s: String,
    stderr_s: String,
    exit_success: bool,
    next_action_at: DateTime<Utc>,
) -> Result<()> {
    let mut lines = vec![];
    lines.push(format!(
        "{} *{hostname}* {}",
        if exit_success { "✅" } else { "❌" },
        action,
    ));
    if !stdout_s.is_empty() {
        lines.push(format!("```\n{stdout_s}\n```"));
    }
    if !exit_success && !stderr_s.is_empty() {
        lines.push(format!("```\n{stderr_s}\n```"));
    }
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
