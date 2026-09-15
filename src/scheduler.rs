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
/// crontab form has 5 fields (`minute hour day-of-month month day-of-week`).
/// Try the crate first so every expression it accepts keeps its meaning,
/// including shorthands and spellings whose whitespace tokens are not fields.
/// Otherwise, [`crontab_to_quartz`] recognises a strict crontab subset and
/// rewrites it to fire at second 0.
pub fn parse_schedule(expr: &str) -> Result<Schedule> {
    let expr = expr.trim();
    expr.parse::<Schedule>()
        .map_err(anyhow::Error::from)
        .or_else(|original_error| {
            let tokens: Vec<&str> = expr.split_whitespace().collect();
            match <[&str; 5]>::try_from(tokens) {
                Ok(fields) => {
                    crontab_to_quartz(&fields)?.parse().map_err(anyhow::Error::from)
                }
                Err(_) => Err(original_error),
            }
        })
        .with_context(|| format!("while parsing cron expression: {expr}"))
}

/// One crontab field: `item(/step)?(,item(/step)?)*` where an item is `*`,
/// a number, a numeric range, a name, or a name range. No whitespace, no
/// Quartz-only syntax (`?`, `L`, `W`, `#`).
static CRONTAB_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)^
        (?:\*|[0-9]+(?:-[0-9]+)?|[A-Za-z]+(?:-[A-Za-z]+)?)(?:/[0-9]+)?
        (?:,(?:\*|[0-9]+(?:-[0-9]+)?|[A-Za-z]+(?:-[A-Za-z]+)?)(?:/[0-9]+)?)*
        $",
    )
    .expect("CRONTAB_FIELD regex is valid")
});

/// The crontab day-of-week field, restricted to `*` or named days
/// (`MON`, `MON-FRI/2`, `SAT,SUN`). Steps are allowed on named ranges.
/// Numeric weekdays and wildcard steps are refused: crontab counts
/// 0 (or 7) = Sunday, 1 = Monday, while the `cron` crate counts
/// 1 = Sunday, so a numeric day would silently shift by one.
static CRONTAB_DAY_OF_WEEK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)^(?:\*|
        [A-Za-z]+(?:-[A-Za-z]+(?:/[0-9]+)?)?
        (?:,[A-Za-z]+(?:-[A-Za-z]+(?:/[0-9]+)?)?)*
        )$",
    )
    .expect("CRONTAB_DAY_OF_WEEK regex is valid")
});

/// Rewrite a strict 5-field crontab expression as the 6-field Quartz form
/// the `cron` crate parses, by prepending a `0` seconds field.
///
/// # Field boundaries
///
/// Legacy expressions are preserved by the raw-first parse in
/// [`parse_schedule`], not by assumptions about the crate's grammar.
/// For `cron` 0.12.1 (`src/parsing.rs`), a successful parse of the rewrite
/// reads exactly `0 minute hour day-of-month month day-of-week`:
///
/// - ASCII digit/name runs are consumed whole, or fail validation; they
///   cannot be shortened into multiple fields. A `*` is one whole item.
/// - Within a token, `,`, `-` and `/` are consumed with the item/operand
///   that follows them, including `*` in a list. Alternatives and lists
///   can backtrack, but a successful prefix then leaves an operator that
///   cannot begin another field. Such an incomplete token is rejected.
/// - Tokens contain no whitespace and neither begin nor end with an
///   operator. Although the crate accepts spaces on either side of an
///   operator (e.g. `1- 2`), neither side of a strict-token boundary has
///   one, so a field cannot consume part of the next token.
///
/// Values and names are still validated by the crate. Numeric weekdays
/// are refused because their numbering differs. A non-`*` weekday requires
/// DOM to start with `*`, preserving Vixie/Cronie's AND semantics; otherwise
/// crontab uses OR while the crate uses AND.
///
/// Tests compare generated valid expressions with independent field sets
/// and exercise lexical boundaries. They are regression checks, not a
/// proof against every future grammar change; review this argument and
/// the dependency parser when upgrading `cron`.
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
             (e.g. MON-FRI, MON-FRI/2 or SAT,SUN), or use '*', because crontab (0 = Sunday) and Quartz \
             (1 = Sunday) number them differently; or write the 6-field form"
        );
    }
    if !fields[2].starts_with('*') && day_of_week != "*" {
        bail!(
            "day-of-month ({:?}) must start with '*' when day-of-week ({day_of_week:?}) is set: \
             Vixie/Cronie crontab otherwise fires when either matches but composer fires only when both do; \
             separate schedule labels can run twice when both match",
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
    fn five_field_rejects_numeric_day_of_week() {
        for dow in ["1", "0", "7", "1-5", "*/2", "MON,1", "MON/2", "*,MON"] {
            let expr = format!("0 2 * * {dow}");
            let err = format!("{:?}", parse_schedule(&expr).unwrap_err());
            assert!(err.contains("must name days"), "{expr}: {err}");
        }
        assert!(parse_schedule("0 2 * * MON").is_ok());
        assert!(parse_schedule("0 2 * * mon-fri").is_ok());
        assert!(parse_schedule("0 2 * * SAT,SUN").is_ok());
        // 6-field numeric days are the crate's business, unchanged
        assert!(parse_schedule("0 0 2 * * 1").is_ok());
    }

    #[test]
    fn five_field_rejects_or_day_semantics() {
        for dom in ["1", "1-31", "1,*"] {
            let expr = format!("0 0 {dom} * MON");
            let err = format!("{:?}", parse_schedule(&expr).unwrap_err());
            assert!(err.contains("either matches"), "{expr}: {err}");
            assert!(err.contains("run twice"), "{expr}: {err}");
        }
        assert!(parse_schedule("0 0 1 * *").is_ok());
        assert!(parse_schedule("0 0 * * MON").is_ok());
        // the crate accepts the 6-field form; its and-semantics are then explicit
        assert!(parse_schedule("0 0 0 1 * MON").is_ok());
    }

    #[test]
    fn five_field_wildcard_dom_keeps_and_semantics() {
        for dom in ["*", "*/1", "*,1"] {
            assert_eq!(upcoming(&format!("0 0 {dom} * MON")), upcoming("0 0 * * MON"));
        }
        assert_eq!(
            upcoming("0 0 */2 * MON"),
            vec!["2026-03-09 00:00:00", "2026-03-23 00:00:00", "2026-04-13 00:00:00"]
        );
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
    fn five_field_numeric_point_step_extension() {
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
        // Quartz-only syntax isn't crontab, so it isn't rewritten either
        let err = format!("{:?}", parse_schedule("0 2 ? * MON").unwrap_err());
        assert!(err.contains("not a valid crontab field"), "{err}");
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
            "JAN/2",
            "JAN-FEB/2",
            "January",
            "L",
            "W",
        ] {
            assert!(CRONTAB_FIELD.is_match(token), "{token}");
            for position in 0..4 {
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
        ] {
            let err = parse_schedule(bad).unwrap_err().to_string();
            assert!(err.contains("while parsing cron expression"), "{bad}: {err}");
        }
    }

    mod crontab_properties {
        //! Generate valid spellings with independently calculated field sets.
        //! No parser is used to build the expectations, and parse failures are
        //! failures, not skipped cases. Sampling supplements the boundary
        //! regressions; it does not cover every possible grammar change.
        use super::*;
        use proptest::prelude::*;

        fn numeric_item(min: u32, max: u32) -> impl Strategy<Value = (String, Vec<u32>)> {
            (min..=max, min..=max, 0..3, prop::option::of(1..=max + 1), 0..=4usize)
                .prop_map(move |(a, b, kind, step, width)| {
                    let (mut text, start, end) = match kind {
                        0 => ("*".to_string(), min, max),
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
            let days = prop_oneof![
                field(numeric_item(1, 31))
                    .prop_map(|dom| (dom, ("*".to_string(), (1..=7).collect()))),
                (prop::option::of(1..=32u32), field(named_item(DAYS))).prop_map(
                    |(step, dow)| {
                        let text = step
                            .map(|step| format!("*/{step}"))
                            .unwrap_or_else(|| "*".to_string());
                        (
                            (
                                text,
                                (1..=31).step_by(step.unwrap_or(1) as usize).collect(),
                            ),
                            dow,
                        )
                    }
                ),
            ];
            (
                field(numeric_item(0, 59)),
                field(numeric_item(0, 23)),
                days,
                field(prop_oneof![numeric_item(1, 12), named_item(MONTHS)]),
            )
                .prop_map(|(minute, hour, (dom, dow), month)| {
                    [minute, hour, dom, month, dow]
                })
        }

        proptest! {
            #[test]
            fn valid_fields_are_read_positionally(fields in fields()) {
                let tokens = fields.each_ref().map(|(text, _)| text.as_str());
                let quartz = crontab_to_quartz(&tokens).unwrap();
                let schedule: Schedule = quartz.parse().unwrap();
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
                    prop_assert_eq!(&actual, expected, "field {:?} in {:?}", text, quartz);
                }
                let parsed = parse_schedule(&tokens.join(" ")).unwrap();
                prop_assert!(parsed.timeunitspec_eq(&schedule));
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
