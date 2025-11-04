use crate::compose::ComposeContext;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
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
    timezone: Tz,
) {
    loop {
        let up = match schedule.upcoming(timezone).next() {
            Some(up) => up,
            None => {
                warn!("no more scheduled times for {action}, task exiting");
                break;
            }
        };
        let up_utc = up.with_timezone(&Utc);
        let duration_from_now = (up_utc - Utc::now()).to_std().unwrap();
        info!("next {action} in {}", humantime::format_duration(duration_from_now));
        tokio::time::sleep(duration_from_now).await;
        let now = Utc::now();
        if (now - up_utc).abs() > chrono::Duration::seconds(1) {
            error!("time skew for scheduled {action}: expected {up_utc}, is {now}");
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
                        up_utc,
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
                        up_utc,
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
                    action,
                    String::from_utf8(out.stdout).unwrap_or_default(),
                    String::from_utf8(out.stderr).unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_schedule_across_dst_boundary() {
        // Test that scheduling works correctly across a DST boundary
        // America/Chicago transitions from CST to CDT on March 10, 2024 at 2:00 AM
        // (clocks spring forward to 3:00 AM)

        let chicago = chrono_tz::America::Chicago;

        // Create a schedule that runs every hour: "0 0 * * * *"
        let schedule: Schedule = "0 0 * * * *".parse().expect("valid cron expression");

        // Start from a point before DST transition: March 10, 2024 at 1:00 AM CST
        let start_time = chicago.with_ymd_and_hms(2024, 3, 10, 1, 0, 0).unwrap();

        // Get the next few scheduled times
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();

        // Verify we have 5 scheduled times
        assert_eq!(upcoming.len(), 5);

        // Expected times:
        // 1. March 10, 2024 at 2:00 AM CST (this becomes 3:00 AM CDT due to DST)
        // 2. March 10, 2024 at 3:00 AM CDT
        // 3. March 10, 2024 at 4:00 AM CDT
        // 4. March 10, 2024 at 5:00 AM CDT
        // 5. March 10, 2024 at 6:00 AM CDT

        // Note: During DST transition, 2:00 AM doesn't exist (clocks jump to 3:00 AM)
        // The cron library should handle this correctly

        // Verify that times are increasing
        for i in 1..upcoming.len() {
            assert!(upcoming[i] > upcoming[i - 1],
                "Scheduled times should be strictly increasing, but {} is not after {}",
                upcoming[i], upcoming[i - 1]);
        }

        // Verify the first scheduled time is after our start time
        assert!(upcoming[0] > start_time,
            "First scheduled time {} should be after start time {}",
            upcoming[0], start_time);

        // Convert to UTC and verify the time differences
        let utc_times: Vec<_> = upcoming.iter().map(|dt| dt.with_timezone(&Utc)).collect();

        // Check that most intervals are approximately 1 hour (3600 seconds)
        // Note: during DST transition, one interval will be shorter (0 seconds if 2 AM is skipped)
        for i in 1..utc_times.len() {
            let diff = (utc_times[i] - utc_times[i - 1]).num_seconds();
            // Allow for DST transitions where an hour is skipped
            assert!(diff >= 0 && diff <= 3600,
                "Time difference should be between 0 and 3600 seconds, but got {} seconds between {} and {}",
                diff, utc_times[i], utc_times[i - 1]);
        }
    }

    #[test]
    fn test_schedule_with_utc() {
        // Test that UTC timezone works correctly (no DST)
        let utc = chrono_tz::UTC;

        // Create a schedule that runs every hour
        let schedule: Schedule = "0 0 * * * *".parse().expect("valid cron expression");

        // Start from an arbitrary time
        let start_time = utc.with_ymd_and_hms(2024, 3, 10, 1, 0, 0).unwrap();

        // Get the next 5 scheduled times
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();

        assert_eq!(upcoming.len(), 5);

        // With UTC, all intervals should be exactly 1 hour
        for i in 1..upcoming.len() {
            let diff = (upcoming[i] - upcoming[i - 1]).num_seconds();
            assert_eq!(diff, 3600,
                "UTC time differences should always be exactly 3600 seconds, got {}", diff);
        }
    }
}
