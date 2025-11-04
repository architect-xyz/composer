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
    fn test_daily_schedule_across_dst_spring_forward() {
        // Test a daily task at 2:30 AM across the spring forward DST transition
        // America/Chicago transitions from CST to CDT on March 10, 2024 at 2:00 AM
        // (clocks spring forward to 3:00 AM, so 2:30 AM doesn't exist on that day)

        let chicago = chrono_tz::America::Chicago;

        // Daily at 2:30 AM: "0 30 2 * * *"
        let schedule: Schedule = "0 30 2 * * *".parse().expect("valid cron expression");

        // Start from March 8, 2024 at 3:00 AM CST (before DST transition)
        let start_time = chicago.with_ymd_and_hms(2024, 3, 8, 3, 0, 0).unwrap();

        // Get the next 5 scheduled times
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();

        // Expected times:
        // March 9, 2024 at 2:30 AM CST
        // March 11, 2024 at 2:30 AM CDT (March 10 is SKIPPED - 2:30 AM doesn't exist that day)
        // March 12, 2024 at 2:30 AM CDT
        // March 13, 2024 at 2:30 AM CDT
        // March 14, 2024 at 2:30 AM CDT
        //
        // Note: The cron library skips March 10 entirely since the scheduled time doesn't exist.
        // This is correct behavior - the task simply doesn't run on days when the time is invalid.
        let expected = [
            "2024-03-09 02:30:00 CST",
            "2024-03-11 02:30:00 CDT",
            "2024-03-12 02:30:00 CDT",
            "2024-03-13 02:30:00 CDT",
            "2024-03-14 02:30:00 CDT",
        ];

        assert_eq!(upcoming.len(), expected.len());

        for (i, (actual, expected_str)) in upcoming.iter().zip(expected.iter()).enumerate() {
            let actual_str = format!("{}", actual.format("%Y-%m-%d %H:%M:%S %Z"));
            assert_eq!(
                &actual_str, expected_str,
                "Mismatch at index {}: expected '{}', got '{}'",
                i, expected_str, actual_str
            );
        }
    }

    #[test]
    fn test_hourly_schedule_at_dst_spring_forward() {
        // Test an hourly task at the exact moment of DST spring forward
        // America/Chicago: March 10, 2024 at 2:00 AM CST -> 3:00 AM CDT

        let chicago = chrono_tz::America::Chicago;

        // Every hour on the hour: "0 0 * * * *"
        let schedule: Schedule = "0 0 * * * *".parse().expect("valid cron expression");

        // Start from March 10, 2024 at 12:30 AM CST (just after midnight)
        let start_time = chicago.with_ymd_and_hms(2024, 3, 10, 0, 30, 0).unwrap();

        // Get the next 5 scheduled times
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();

        // Expected times:
        // 1:00 AM CST
        // 3:00 AM CDT (2:00 AM doesn't exist - clocks jump from 2:00 AM to 3:00 AM)
        // 4:00 AM CDT
        // 5:00 AM CDT
        // 6:00 AM CDT
        let expected = [
            "2024-03-10 01:00:00 CST",
            "2024-03-10 03:00:00 CDT",
            "2024-03-10 04:00:00 CDT",
            "2024-03-10 05:00:00 CDT",
            "2024-03-10 06:00:00 CDT",
        ];

        assert_eq!(upcoming.len(), expected.len());

        for (i, (actual, expected_str)) in upcoming.iter().zip(expected.iter()).enumerate() {
            let actual_str = format!("{}", actual.format("%Y-%m-%d %H:%M:%S %Z"));
            assert_eq!(
                &actual_str, expected_str,
                "Mismatch at index {}: expected '{}', got '{}'",
                i, expected_str, actual_str
            );
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
