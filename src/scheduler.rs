use crate::compose::ComposeContext;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use log::{error, info, warn};
use regex::Regex;
use serde_json::json;
use std::{process::Stdio, sync::LazyLock};

/// Parse the `cron` crate's Quartz dialect, optionally omitting seconds.
/// Native expressions are tried first to preserve their meaning. Otherwise,
/// five complete fields are parsed with a leading `0` seconds field; their
/// syntax and semantics are still those of the crate, not Unix crontab.
pub fn parse_schedule(expr: &str) -> Result<Schedule> {
    let expr = expr.trim();
    expr.parse::<Schedule>()
        .map_err(anyhow::Error::from)
        .or_else(|original_error| {
            let fields: Vec<&str> = expr.split_whitespace().collect();
            if fields.len() != 5 || !fields.iter().all(|f| QUARTZ_FIELD.is_match(f)) {
                return Err(original_error);
            }
            format!("0 {}", fields.join(" ")).parse().map_err(anyhow::Error::from)
        })
        .with_context(|| format!("while parsing cron expression: {expr}"))
}

/// Recognise complete, whitespace-free fields, not just whitespace tokens.
/// In `cron` 0.12.1, digit/name runs are consumed whole. Lists, ranges and
/// steps consume `,`, `-` and `/` with their operands (including wildcards),
/// or backtrack and leave an operator that cannot start another field.
/// Since tokens neither start nor end with these operators, they cannot
/// split into fields or merge across whitespace. The crate still validates
/// values and where syntax such as `?` is allowed. Review these assumptions
/// on crate upgrades; the field-set properties are regression checks, not
/// a proof against every possible grammar change.
static QUARTZ_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)^
        (?:[*?]|[0-9]+(?:-[0-9]+)?|[A-Za-z]+(?:-[A-Za-z]+)?)(?:/[0-9]+)?
        (?:,(?:[*?]|[0-9]+(?:-[0-9]+)?|[A-Za-z]+(?:-[A-Za-z]+)?)(?:/[0-9]+)?)*
        $",
    )
    .expect("QUARTZ_FIELD regex is valid")
});

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
    use cron::TimeUnitSpec;
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

    fn upcoming(expr: &str) -> Vec<String> {
        let start = Utc.with_ymd_and_hms(2026, 3, 6, 12, 0, 30).unwrap();
        parse_schedule(expr)
            .unwrap()
            .after(&start)
            .take(3)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .collect()
    }

    #[test]
    fn five_field_uses_quartz_weekdays() {
        for (dow, expected) in [
            ("1", vec![1]),
            ("7", vec![7]),
            ("1-5", vec![1, 2, 3, 4, 5]),
            ("*/2", vec![1, 3, 5, 7]),
            ("MON,1", vec![1, 2]),
            ("*,MON", vec![1, 2, 3, 4, 5, 6, 7]),
        ] {
            let schedule = parse_schedule(&format!("0 2 * * {dow}")).unwrap();
            assert_eq!(schedule.days_of_week().iter().collect::<Vec<_>>(), expected);
        }
        assert_eq!(upcoming("0 2 * * 1"), upcoming("0 2 * * SUN"));
        assert_eq!(upcoming("0 2 * * 7"), upcoming("0 2 * * SAT"));
    }

    #[test]
    fn five_field_day_constraints_use_and() {
        assert_eq!(
            upcoming("0 2 1 * MON"),
            vec!["2026-06-01 02:00:00", "2027-02-01 02:00:00", "2027-03-01 02:00:00"]
        );
        for dom in ["*", "*/1", "*,1", "1,*", "1-31"] {
            assert_eq!(upcoming(&format!("0 0 {dom} * MON")), upcoming("0 0 * * MON"));
        }
        assert_eq!(
            upcoming("0 0 */2 * MON"),
            vec!["2026-03-09 00:00:00", "2026-03-23 00:00:00", "2026-04-13 00:00:00"]
        );
    }

    #[test]
    fn five_field_question_mark_is_a_day_wildcard() {
        let schedule = parse_schedule("0 2 ? * MON").unwrap();
        assert!(schedule.days_of_month().is_all());
        assert_eq!(schedule.days_of_week().iter().collect::<Vec<_>>(), vec![2]);
        let schedule = parse_schedule("0 2 1 * ?").unwrap();
        assert_eq!(schedule.days_of_month().iter().collect::<Vec<_>>(), vec![1]);
        assert!(schedule.days_of_week().is_all());
        let schedule = parse_schedule("0 2 ?/2 * ?/2").unwrap();
        assert_eq!(
            schedule.days_of_month().iter().collect::<Vec<_>>(),
            (1..=31).step_by(2).collect::<Vec<_>>()
        );
        assert_eq!(schedule.days_of_week().iter().collect::<Vec<_>>(), vec![1, 3, 5, 7]);
    }

    #[test]
    fn five_field_steps_apply_to_each_list_item() {
        for minute in ["0-10/5,30", "30,0-10/5", "0-10/5,30-40/10"] {
            let schedule = parse_schedule(&format!("{minute} * * * *")).unwrap();
            let expected = if minute.ends_with("/10") {
                vec![0, 5, 10, 30, 40]
            } else {
                vec![0, 5, 10, 30]
            };
            assert_eq!(schedule.minutes().iter().collect::<Vec<_>>(), expected);
        }
        let schedule = parse_schedule("0 2 * JAN-MAR/2,JUN-AUG/2 MON-FRI/2,SAT").unwrap();
        assert_eq!(schedule.months().iter().collect::<Vec<_>>(), vec![1, 3, 6, 8]);
        assert_eq!(schedule.days_of_week().iter().collect::<Vec<_>>(), vec![2, 4, 6, 7]);
    }

    #[test]
    fn five_field_numeric_point_steps() {
        let schedule = parse_schedule("5/10,30 2 * * *").unwrap();
        assert_eq!(
            schedule.minutes().iter().collect::<Vec<_>>(),
            vec![5, 15, 25, 30, 35, 45, 55]
        );
    }

    #[test]
    fn crate_accepted_spellings_keep_their_meaning() {
        // The crate allows adjacent fields and whitespace inside fields,
        // including after an operator. Whitespace token count is not field count.
        for expr in [
            "0 0 2 ** *",
            "* * * * *1",
            "* * * ** *",
            "0 0 0 1- 2 * *",
            "0 0 0 1, 2 * *",
            "0 0 0 */ 2 * *",
        ] {
            let ours = parse_schedule(expr).unwrap();
            let crates: Schedule = expr.parse().unwrap();
            assert!(ours.timeunitspec_eq(&crates), "{expr}");
            assert_eq!(ours.to_string(), expr);
        }
    }

    #[test]
    fn five_field_requires_complete_fields() {
        for expr in ["0 0 31 * *2027", "0 0 31 * **"] {
            assert!(expr.parse::<Schedule>().is_err());
            // A token-count-only fallback would accidentally accept seven fields.
            assert!(format!("0 {expr}").parse::<Schedule>().is_ok());
            assert!(parse_schedule(expr).is_err());
        }
    }

    #[test]
    fn five_field_leading_zeros_and_names_keep_their_positions() {
        let schedule = parse_schedule("00 02 * January tues-thurs").unwrap();
        assert_eq!(schedule.seconds().iter().collect::<Vec<_>>(), vec![0]);
        assert_eq!(schedule.minutes().iter().collect::<Vec<_>>(), vec![0]);
        assert_eq!(schedule.hours().iter().collect::<Vec<_>>(), vec![2]);
        assert!(schedule.days_of_month().is_all());
        assert_eq!(schedule.months().iter().collect::<Vec<_>>(), vec![1]);
        assert_eq!(schedule.days_of_week().iter().collect::<Vec<_>>(), vec![3, 4, 5]);
        assert!(schedule.years().is_all());
        let schedule = parse_schedule("01-09/02,00059 002 * * *").unwrap();
        assert_eq!(
            schedule.minutes().iter().collect::<Vec<_>>(),
            vec![1, 3, 5, 7, 9, 59]
        );
        assert_eq!(schedule.hours().iter().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn strict_token_boundaries_do_not_create_extra_fields() {
        for token in [
            "00",
            "000",
            "00059",
            "4294967295",
            "4294967296",
            "1-4294967296",
            "1,4294967296",
            "*/4294967296",
            "*/0002",
            "1,*",
            "1,*/2",
            "1,JAN",
            "?",
            "?/2",
            "?,MON",
            "JAN/2",
            "JAN-FEB/2",
            "January",
            "L",
            "W",
        ] {
            assert!(QUARTZ_FIELD.is_match(token), "{token}");
            for position in 0..5 {
                let mut fields = ["0", "0", "*", "*", "*"];
                fields[position] = token;
                let expr = fields.join(" ");
                assert!(expr.parse::<Schedule>().is_err(), "crate accepted {expr:?} raw");
            }
        }
    }

    #[test]
    fn five_field_expressions_fire_at_second_zero() {
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
        for bad in [
            "0 2 * *",
            "0 0 2 * * * * *",
            "not a cron",
            "",
            "99 * * * *",
            "*/0 * * * *",
            "4294967295 * * * *",
            "4294967296 * * * *",
            "1-4294967296 * * * *",
            "1,4294967296 * * * *",
            "*/4294967296 * * * *",
            "JAN/2 * * * *",
            "0 0 * * MON-FRI/0",
            "0 0 * * MON-FRI/4294967296",
            "0 2 * * 0",
            "0 2 * * MON/2",
            "? 2 * * *",
            "0 ? * * *",
            "0 2 * ? *",
        ] {
            let err = parse_schedule(bad).unwrap_err().to_string();
            assert!(err.contains("while parsing cron expression"), "{bad}: {err}");
        }
    }

    mod five_field_properties {
        //! Generate valid spellings with independently calculated field sets.
        //! No parser is used to build the expectations, and parse failures are
        //! failures, not skipped cases. Sampling supplements the boundary
        //! regressions; it does not cover every possible grammar change.
        use super::*;
        use proptest::prelude::*;

        fn numeric_item(
            min: u32,
            max: u32,
            wildcard: &'static str,
        ) -> impl Strategy<Value = (String, Vec<u32>)> {
            (min..=max, min..=max, 0..3, prop::option::of(1..=max + 1), 0..=4usize)
                .prop_map(move |(a, b, kind, step, width)| {
                    let (mut text, start, end) = match kind {
                        0 => (wildcard.to_string(), min, max),
                        1 => (
                            format!("{a:0width$}"),
                            a,
                            if step.is_some() { max } else { a },
                        ),
                        _ => {
                            let (start, end) = (a.min(b), a.max(b));
                            (format!("{start:0width$}-{end:0width$}"), start, end)
                        }
                    };
                    if let Some(step) = step {
                        text.push_str(&format!("/{step:0width$}"));
                    }
                    (text, (start..=end).step_by(step.unwrap_or(1) as usize).collect())
                })
        }

        fn named_item(
            names: &'static [&'static str],
        ) -> impl Strategy<Value = (String, Vec<u32>)> {
            (
                0..names.len(),
                0..names.len(),
                any::<bool>(),
                prop::option::of(1..=names.len() as u32 + 1),
                0..3,
            )
                .prop_map(move |(a, b, range, step, style)| {
                    let name = |i: usize| match style {
                        0 => names[i][..3].to_uppercase(),
                        1 => names[i].to_lowercase(),
                        _ => names[i].to_string(),
                    };
                    let (start, end) = if range { (a.min(b), a.max(b)) } else { (a, a) };
                    let mut text = if range {
                        format!("{}-{}", name(start), name(end))
                    } else {
                        name(a)
                    };
                    let step = if range { step } else { None };
                    if let Some(step) = step {
                        text.push_str(&format!("/{step}"));
                    }
                    (
                        text,
                        (start as u32 + 1..=end as u32 + 1)
                            .step_by(step.unwrap_or(1) as usize)
                            .collect(),
                    )
                })
        }

        fn field(
            items: impl Strategy<Value = (String, Vec<u32>)>,
        ) -> impl Strategy<Value = (String, Vec<u32>)> {
            prop::collection::vec(items, 1..=6).prop_map(|items| {
                let text = items
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                let mut values: Vec<_> =
                    items.into_iter().flat_map(|(_, values)| values).collect();
                values.sort_unstable();
                values.dedup();
                (text, values)
            })
        }

        fn fields() -> impl Strategy<Value = [(String, Vec<u32>); 5]> {
            const MONTHS: &[&str] = &[
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ];
            const DAYS: &[&str] = &[
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ];
            (
                field(numeric_item(0, 59, "*")),
                field(numeric_item(0, 23, "*")),
                field(prop_oneof![numeric_item(1, 31, "*"), numeric_item(1, 31, "?")]),
                field(prop_oneof![numeric_item(1, 12, "*"), named_item(MONTHS)]),
                field(prop_oneof![
                    numeric_item(1, 7, "*"),
                    numeric_item(1, 7, "?"),
                    named_item(DAYS)
                ]),
            )
                .prop_map(|(minute, hour, dom, month, dow)| {
                    [minute, hour, dom, month, dow]
                })
        }

        proptest! {
            #[test]
            fn valid_fields_are_read_positionally(fields in fields()) {
                let tokens = fields.each_ref().map(|(text, _)| text.as_str());
                let expr = tokens.join(" ");
                let schedule = parse_schedule(&expr).unwrap();
                prop_assert_eq!(schedule.seconds().iter().collect::<Vec<_>>(), vec![0]);
                prop_assert!(schedule.years().is_all());
                let actual = [
                    schedule.minutes().iter().collect::<Vec<_>>(),
                    schedule.hours().iter().collect::<Vec<_>>(),
                    schedule.days_of_month().iter().collect::<Vec<_>>(),
                    schedule.months().iter().collect::<Vec<_>>(),
                    schedule.days_of_week().iter().collect::<Vec<_>>(),
                ];
                for ((text, expected), actual) in fields.iter().zip(actual) {
                    prop_assert_eq!(&actual, expected, "field {:?} in {:?}", text, expr);
                }
            }

            #[test]
            fn strict_five_fields_are_not_valid_raw(fields in fields()) {
                let expr = fields.each_ref().map(|(text, _)| text.as_str()).join(" ");
                prop_assert!(expr.parse::<Schedule>().is_err(), "crate accepted {expr:?} raw");
            }

            #[test]
            fn field_separator_whitespace_is_invariant(
                fields in fields(),
                whitespace in prop::sample::select(vec![" ", "   ", "\t", " \t ", "\n", "\r\n", "\u{a0}"]),
            ) {
                let tokens = fields.each_ref().map(|(text, _)| text.as_str());
                let expected = parse_schedule(&tokens.join(" ")).unwrap();
                let expr = format!("{whitespace}{}{whitespace}", tokens.join(whitespace));
                let actual = parse_schedule(&expr).unwrap();
                prop_assert!(actual.timeunitspec_eq(&expected));
            }
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
