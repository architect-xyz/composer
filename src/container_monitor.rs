use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use log::{error, trace};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::time::MissedTickBehavior;

#[derive(Deserialize)]
struct DockerComposePsJson {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
}

#[derive(Default, Clone)]
struct ServiceStatus {
    is_running: Option<bool>,
    last_running: Option<DateTime<Utc>>,
}

pub async fn run(
    host_name: String,
    services: Vec<String>,
    slack_webhook_url: Option<String>,
) -> Result<()> {
    let mut status_board: BTreeMap<String, ServiceStatus> = BTreeMap::new();
    let mut check_interval = tokio::time::interval(Duration::from_secs(60));
    check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut should_notify = true;
    loop {
        check_interval.tick().await;
        let now = Utc::now();
        let mut cmd = tokio::process::Command::new("docker");
        cmd.arg("compose")
            .arg("ps")
            .arg("--format")
            .arg("json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let cmd_out = cmd.output().await?;
        let std_out = String::from_utf8_lossy(&cmd_out.stdout);
        let mut rows = BTreeMap::new();
        for line in std_out.lines() {
            let row: DockerComposePsJson = match serde_json::from_str(line) {
                Ok(row) => row,
                Err(e) => {
                    trace!("{line}");
                    error!("error parsing [docker compose ps] output line: {e:?}");
                    continue;
                }
            };
            rows.insert(row.service.clone(), row);
        }
        for service in &services {
            let is_running = match rows.get(service) {
                Some(row) => row.state == "running",
                None => false,
            };
            let status = status_board.entry(service.clone()).or_default();
            let last_running = status.last_running.unwrap_or(DateTime::<Utc>::MIN_UTC);
            if !is_running && (now - last_running).num_seconds() >= 30 {
                should_notify = true;
            }
            status.is_running = Some(is_running);
            if is_running {
                status.last_running = Some(now);
            }
        }
        if should_notify {
            if let Some(url) = slack_webhook_url.as_deref() {
                if let Err(e) = notify_slack(url, &host_name, now, &status_board).await {
                    error!("error notifying slack: {e:?}");
                }
            }
        }
        should_notify = false;
    }
}

async fn notify_slack(
    url: &str,
    host_name: &str,
    run_at: DateTime<Utc>,
    services: &BTreeMap<String, ServiceStatus>,
) -> Result<()> {
    let mut lines = vec![];
    lines.push(format!("💿 *container monitor* for {host_name}"));
    lines.push("".to_string());
    lines.push(format!(
        "<!date^{}^Run at {{date_num}} {{time_secs}}|Run at {run_at}>",
        run_at.timestamp()
    ));
    lines.push("".to_string());
    for (service, status) in services {
        let status_emoji =
            if status.is_running.unwrap_or(false) { "🟢" } else { "🔴" };
        lines.push(format!("{status_emoji} {service}"));
    }
    let text =
        lines.iter().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
    let client = reqwest::Client::new();
    let res = client.post(url).json(&json!({ "text": text })).send().await?;
    if !res.status().is_success() {
        let err_body = res.text().await.context("reading response body")?;
        return Err(anyhow!("{err_body}"));
    }
    Ok(())
}
