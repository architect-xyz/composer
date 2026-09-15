use crate::compose::ComposeContext;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use log::{error, info, warn};
use regex::Regex;
use serde_json::json;
use std::{process::Stdio, sync::LazyLock};

/// Parse a cron expression.  The `cron` crate wants the Quartz form, with a
/// seconds field first (6 fields, or 7 with a trailing year).  The classic
/// crontab form has 5 fields (`minute hour day-of-month month day-of-week`);
/// an expression in that form is recognised by [`crontab_to_quartz`] and
/// rewritten to fire at second 0.  Anything else, including `@daily`-style
/// shorthands, goes to the crate untouched.
pub fn parse_schedule(expr: &str) -> Result<Schedule> {
    let expr = expr.trim();
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    let crontab = match <[&str; 5]>::try_from(tokens) {
        Ok(fields) => Some(crontab_to_quartz(&fields)),
        Err(_) => None,
    };
    match crontab {
        Some(Ok(quartz)) => quartz.parse().map_err(anyhow::Error::from),
        // Not a strict crontab expression.  Try the crate's own grammar
        // first (it accepts some 5-token spellings, e.g. `0 0 2 ** *`, as 6
        // fields); if that fails too, the crontab diagnosis is the more
        // useful error.
        Some(Err(e)) => expr.parse().map_err(|_| e),
        None => expr.parse().map_err(anyhow::Error::from),
    }
    .with_context(|| format!("while parsing cron expression: {expr}"))
}

/// One crontab field: `item(,item)*(/step)?` where an item is `*`, a
/// number, a numeric range, a name, or a name range.  No whitespace, no
/// Quartz-only syntax (`?`, `L`, `W`, `#`).
static CRONTAB_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)^
        (?:\*|\d+(?:-\d+)?|[A-Za-z]+(?:-[A-Za-z]+)?)
        (?:,(?:\*|\d+(?:-\d+)?|[A-Za-z]+(?:-[A-Za-z]+)?))*
        (?:/\d+)?
        $",
    )
    .expect("CRONTAB_FIELD regex is valid")
});

/// The crontab day-of-week field, restricted to `*` or named days
/// (`MON`, `MON-FRI`, `SAT,SUN`).  Numbers are refused: crontab counts
/// 0 (or 7) = Sunday, 1 = Monday, while the `cron` crate counts
/// 1 = Sunday, so a numeric day would silently shift by one.
static CRONTAB_DAY_OF_WEEK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\*|[A-Za-z]+(?:-[A-Za-z]+)?(?:,[A-Za-z]+(?:-[A-Za-z]+)?)*)$")
        .expect("CRONTAB_DAY_OF_WEEK regex is valid")
});

/// Rewrite a strict 5-field crontab expression as the 6-field Quartz form
/// the `cron` crate parses, by prepending a `0` seconds field.
///
/// Why this is unambiguous: the crate has no mandatory separator between
/// fields and tolerates whitespace inside ranges and lists, so a raw
/// whitespace-token count is *not* its field count (`0 0 2 ** *` is a valid
/// 6-field expression to it).  A field matching [`CRONTAB_FIELD`] however
/// contains no whitespace and no character at which the crate could start
/// a new field (interior boundaries are `,`, `-`, `/`, or mid-number and
/// mid-name, and its number and name parsers are greedy), and it never ends
/// in `,`, `-` or `/`, so it cannot join the next token either.  Each strict
/// token is therefore exactly one crate field; five of them can never be
/// the six the crate needs, so no expression this function accepts was
/// previously valid, and prepending `0` yields exactly the six fields
/// positionally.  `proptest` below checks both halves against the crate.
///
/// Two crontab semantics have no equivalent in the crate and are refused
/// rather than silently changed: numeric days of week (see
/// [`CRONTAB_DAY_OF_WEEK`]), and restricting both day-of-month and
/// day-of-week (crontab fires when *either* matches, the crate when both).
fn crontab_to_quartz(fields: &[&str; 5]) -> Result<String> {
    const NAMES: [&str; 5] = ["minute", "hour", "day-of-month", "month", "day-of-week"];
    for (name, field) in NAMES.iter().zip(fields).take(4) {
        if !CRONTAB_FIELD.is_match(field) {
            bail!("{name} field {field:?} is not a valid crontab field");
        }
    }
    let day_of_week = fields[4];
    if !CRONTAB_DAY_OF_WEEK.is_match(day_of_week) {
        bail!(
            "day-of-week field {day_of_week:?}: 5-field expressions must name days \
             (e.g. MON-FRI or SAT,SUN) because crontab (0 = Sunday) and Quartz \
             (1 = Sunday) number them differently; or write the 6-field form"
        );
    }
    if fields[2] != "*" && day_of_week != "*" {
        bail!(
            "both day-of-month ({:?}) and day-of-week ({day_of_week:?}) are restricted: \
             crontab fires when either matches but composer fires only when both do; \
             use two schedule labels (e.g. run.monthly and run.weekly) instead",
            fields[2]
        );
    }
    Ok(format!("0 {}", fields.join(" ")))
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
    fn five_field_rejects_numeric_day_of_week() {
        for bad in ["0 2 * * 1", "0 2 * * 0", "0 2 * * 7", "0 2 * * 1-5", "0 2 * * */2"] {
            let err = format!("{:?}", parse_schedule(bad).unwrap_err());
            assert!(err.contains("must name days"), "{bad}: {err}");
        }
        assert!(parse_schedule("0 2 * * MON").is_ok());
        assert!(parse_schedule("0 2 * * mon-fri").is_ok());
        assert!(parse_schedule("0 2 * * SAT,SUN").is_ok());
        // 6-field numeric days are the crate's business, unchanged
        assert!(parse_schedule("0 0 2 * * 1").is_ok());
    }

    #[test]
    fn five_field_rejects_day_of_month_and_week_together() {
        let err = format!("{:?}", parse_schedule("0 0 1 * MON").unwrap_err());
        assert!(err.contains("either matches"), "{err}");
        assert!(parse_schedule("0 0 1 * *").is_ok());
        assert!(parse_schedule("0 0 * * MON").is_ok());
        // the crate accepts the 6-field form; its and-semantics are then explicit
        assert!(parse_schedule("0 0 0 1 * MON").is_ok());
    }

    #[test]
    fn non_strict_five_token_spellings_keep_their_crate_meaning() {
        // these are valid 6-field expressions to the crate despite having
        // five whitespace tokens; they must not be rewritten
        for expr in ["0 0 2 ** *", "* * * * *1", "* * * ** *"] {
            let ours = parse_schedule(expr).unwrap().to_string();
            let crates: Schedule = expr.parse().unwrap();
            assert_eq!(ours, crates.to_string(), "{expr}");
        }
        // Quartz-only syntax isn't crontab, so it isn't rewritten either
        let err = format!("{:?}", parse_schedule("0 2 ? * MON").unwrap_err());
        assert!(err.contains("not a valid crontab field"), "{err}");
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
        for bad in ["0 2 * *", "0 0 2 * * * * *", "not a cron", "", "99 * * * *"] {
            let err = parse_schedule(bad).unwrap_err().to_string();
            assert!(err.contains("while parsing cron expression"), "{bad}: {err}");
        }
    }

    mod crontab_properties {
        //! Checks the two halves of the argument in `crontab_to_quartz`
        //! against the real crate grammar: a strict 5-field expression is
        //! never something the crate already accepted, and rewriting it
        //! yields exactly the six fields positionally.
        use super::*;
        use proptest::prelude::*;

        fn item(max: u32) -> impl Strategy<Value = String> {
            prop_oneof![
                Just("*".to_string()),
                (0..=max).prop_map(|n| n.to_string()),
                (0..=max, 0..=max).prop_map(|(a, b)| format!("{a}-{b}")),
            ]
        }

        fn numeric_field(max: u32) -> impl Strategy<Value = String> {
            (prop::collection::vec(item(max), 1..=3), prop::option::of(1..=30u32))
                .prop_map(|(items, step)| {
                    let mut s = items.join(",");
                    if let Some(step) = step {
                        s.push_str(&format!("/{step}"));
                    }
                    s
                })
        }

        fn month_field() -> impl Strategy<Value = String> {
            prop_oneof![
                numeric_field(12),
                prop::sample::select(vec!["JAN", "FEB", "MAR-JUN", "jul,aug", "DEC"])
                    .prop_map(str::to_string),
            ]
        }

        fn day_of_week_field() -> impl Strategy<Value = String> {
            prop::sample::select(vec![
                "*",
                "MON",
                "SUN",
                "MON-FRI",
                "SAT,SUN",
                "sun-sat",
                "TUE,THU,SAT",
            ])
            .prop_map(str::to_string)
        }

        proptest! {
            #[test]
            fn strict_five_field_is_never_valid_raw_and_rewrites_positionally(
                minute in numeric_field(59),
                hour in numeric_field(23),
                dom in numeric_field(31),
                month in month_field(),
                dow in day_of_week_field(),
            ) {
                let expr = format!("{minute} {hour} {dom} {month} {dow}");
                // half one: the crate never accepted this string as-is
                prop_assert!(expr.parse::<Schedule>().is_err(), "crate accepted {expr:?} raw");
                // half two: when we accept it, it means exactly `0 <fields>`
                let fields = [&*minute, &*hour, &*dom, &*month, &*dow];
                if let Ok(quartz) = crontab_to_quartz(&fields) {
                    prop_assert_eq!(&quartz, &format!("0 {expr}"));
                    // and the crate splits the rewritten string into the same
                    // six fields regardless of how they are spaced
                    let spaced = format!("0   {minute}   {hour}   {dom}   {month}   {dow}");
                    match (quartz.parse::<Schedule>(), spaced.parse::<Schedule>()) {
                        (Ok(a), Ok(b)) => {
                            let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
                            prop_assert!(a.after(&start).take(20).eq(b.after(&start).take(20)));
                        }
                        (Err(_), Err(_)) => {} // out-of-range values; rejected either way
                        (a, b) => prop_assert!(false, "spacing changed validity: {a:?} vs {b:?}"),
                    }
                }
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
