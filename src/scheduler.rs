use crate::compose::ComposeContext;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use log::{error, info, warn};
use serde_json::json;
use std::process::Stdio;

/// Parse a cron expression.  The `cron` crate wants the Quartz form, with a
/// seconds field first (6 fields, or 7 with a trailing year).  The classic
/// crontab form has 5 fields (`minute hour day-of-month month day-of-week`)
/// and is unambiguous by its field count alone, so it is accepted too and
/// fires at second 0.  Shorthands like `@daily` pass straight through.
pub fn parse_schedule(expr: &str) -> Result<Schedule> {
    let expr = expr.trim();
    let normalized = if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    };
    normalized.parse().with_context(|| format!("while parsing cron expression: {expr}"))
}

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
    use std::fmt::Display;

    fn assert_schedule_matches(
        actual: Vec<DateTime<Tz>>,
        expected: Vec<&str>,
        expected_is_utc: bool,
    ) where
        Tz: TimeZone,
        <Tz as TimeZone>::Offset: Display,
    {
        assert_eq!(actual.len(), expected.len());
        for (i, (actual, expected_str)) in actual.iter().zip(expected.iter()).enumerate()
        {
            let actual_str = if expected_is_utc {
                format!("{}", actual.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S %Z"))
            } else {
                format!("{}", actual.format("%Y-%m-%d %H:%M:%S %Z"))
            };
            assert_eq!(
                actual_str, *expected_str,
                "mismatch at index {i}: expected {expected_str}, got {actual_str}",
            );
        }
    }

    #[test]
    fn five_field_expressions_fire_at_second_zero() {
        let start = Utc.with_ymd_and_hms(2026, 3, 6, 12, 0, 30).unwrap();
        let upcoming = |expr: &str| -> Vec<String> {
            parse_schedule(expr)
                .unwrap()
                .after(&start)
                .take(3)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .collect()
        };
        assert_eq!(upcoming("0 2 * * *"), upcoming("0 0 2 * * *"));
        assert_eq!(
            upcoming("*/15 * * * *"),
            vec!["2026-03-06 12:15:00", "2026-03-06 12:30:00", "2026-03-06 12:45:00"]
        );
        assert_eq!(upcoming("30 6 * * MON-FRI"), upcoming("0 30 6 * * MON-FRI"));
        // whitespace around and between fields is tolerated
        assert_eq!(upcoming("  0  2 * *   *  "), upcoming("0 0 2 * * *"));
    }

    #[test]
    fn six_and_seven_field_expressions_are_unchanged() {
        assert_eq!(parse_schedule("0 0 2 * * *").unwrap().to_string(), "0 0 2 * * *");
        assert_eq!(
            parse_schedule("0 0 2 * * * 2027").unwrap().to_string(),
            "0 0 2 * * * 2027"
        );
        assert!(parse_schedule("@daily").is_ok());
    }

    #[test]
    fn invalid_expressions_name_the_input() {
        for bad in ["0 2 * *", "0 0 2 * * * * *", "not a cron", "", "99 * * * *"] {
            let err = parse_schedule(bad).unwrap_err().to_string();
            assert!(err.contains("while parsing cron expression"), "{bad}: {err}");
        }
    }

    #[test]
    fn test_timezone_aware_schedule() {
        let chicago = chrono_tz::America::Chicago;

        // Every day at 4:00 PM America/Chicago
        let schedule: Schedule = "0 0 16 * * *".parse().expect("valid cron expression");
        let start_time = chicago.with_ymd_and_hms(2026, 3, 6, 12, 0, 0).unwrap();
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();
        let expected_in_utc = vec![
            "2026-03-06 22:00:00 UTC", // CST is UTC-6
            "2026-03-07 22:00:00 UTC",
            "2026-03-08 21:00:00 UTC", // CDT is UTC-5
            "2026-03-09 21:00:00 UTC",
            "2026-03-10 21:00:00 UTC",
        ];
        assert_schedule_matches(upcoming, expected_in_utc, true);
    }

    /// Demonstrate an edge case where the scheduler will skip a day!
    ///
    /// This is due to the specific legal definition of how daylight savings time
    /// is applied in the United States, skipping an hour.
    #[test]
    fn test_daylight_savings_time_edge_case() {
        let chicago = chrono_tz::America::Chicago;

        // Every day at 2:30 AM America/Chicago
        let schedule: Schedule = "0 30 2 * * *".parse().expect("valid cron expression");
        let start_time = chicago.with_ymd_and_hms(2024, 3, 8, 12, 0, 0).unwrap();
        let upcoming: Vec<_> = schedule.after(&start_time).take(5).collect();
        let expected = vec![
            "2024-03-09 02:30:00 CST",
            // March 10 is skipped because March 10th, 2:30 AM is not a valid time in Chicago;
            // the clock jumps immediately from 2:00 AM CST to 3:00 AM CDT.
            "2024-03-11 02:30:00 CDT",
            "2024-03-12 02:30:00 CDT",
            "2024-03-13 02:30:00 CDT",
            "2024-03-14 02:30:00 CDT",
        ];
        assert_schedule_matches(upcoming, expected, false);
    }
}
