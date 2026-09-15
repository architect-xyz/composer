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
/// # Correctness
///
/// **Claim.** No expression this function rewrites was accepted by the
/// `cron` crate before, and the crate reads the rewritten expression as
/// exactly the six fields `0 minute hour day-of-month month day-of-week`.
///
/// **Assumptions**, from the grammar in `cron` 0.12, `src/parsing.rs`:
///
/// - A1. A longhand schedule is six or seven consecutive fields followed
///   by end of input. A shorthand schedule begins with `@`.
/// - A2. A field is a comma-separated list of items, each optionally
///   followed by `/step`, surrounded by optional whitespace. Nothing is
///   required between two fields.
/// - A3. An item is `*`, `?`, a number, a name, `number-number` or
///   `name-name`. A field therefore begins with `*`, `?`, a digit or a
///   letter, never with `,`, `-` or `/`. Numbers are parsed by `digit1`
///   and names by `alpha1`, which consume every following digit or letter.
/// - A4. Numbers and names may be surrounded by whitespace. A field can
///   continue past whitespace only if the next non-whitespace character is
///   `-`, `,` or `/`.
/// - A5. Parsing is deterministic and does not backtrack: once a parser
///   has matched, a later failure does not make it retry a shorter match.
///
/// **Definition.** A *strict token* is a string matching [`CRONTAB_FIELD`]:
/// `item(,item)*(/N)?` with items `*`, `N`, `N-M`, `NAME` or `NAME-NAME`.
/// It contains no whitespace, begins with `*`, a digit or a letter, and
/// ends with `*`, a digit or a letter.
///
/// **Lemma 1 (no split).** The crate finds at most one field in a strict
/// token.
///
/// *Proof.* A second field would begin at an interior position. Every
/// interior position is at `,`, `-` or `/`, or inside a run of digits or
/// letters. No field begins with `,`, `-` or `/` (A3). A run of digits or
/// letters is consumed whole by the parser that started it (A3, A5), so no
/// field begins inside it. ∎
///
/// **Lemma 2 (no merge).** A field that begins in a strict token ends at
/// or before the end of that token.
///
/// *Proof.* A strict token contains no whitespace, so the field can pass
/// the token's end only by continuing across the whitespace that follows
/// it. By A4 that requires the next non-whitespace character to be `-`,
/// `,` or `/`. The next token begins with `*`, a digit or a letter, or the
/// input ends. Neither continues the field. ∎
///
/// **Lemma 3 (exactly one).** The crate reads a strict token as exactly one
/// field, or rejects the expression.
///
/// *Proof.* The token begins with `*`, a digit or a letter, so a field
/// begins at its start (A3). By Lemma 1 no second field begins inside it,
/// and by Lemma 2 the field ends with the token. The field parser either
/// accepts the token or fails, and a failed field rejects the whole
/// expression (A1). ∎
///
/// **Theorem.** Let `E = t1 t2 t3 t4 t5` with each `ti` a strict token.
///
/// - (i) The crate rejects `E`.
/// - (ii) The crate reads `0 E` as the six fields `0 t1 t2 t3 t4 t5` in
///   that order, or rejects it.
///
/// *Proof of (i).* `E` does not begin with `@`, so it is not a shorthand
/// (A1). By Lemma 1 the crate finds at most five fields in `E`. A longhand
/// schedule needs six (A1). ∎
///
/// *Proof of (ii).* `0` is a strict token, so `0 E` is six strict tokens
/// separated by whitespace. By Lemma 3 each is exactly one field, and
/// fields are read in input order (A1). ∎
///
/// By (i) this function never rewrites an expression that was already
/// valid. By (ii) the rewrite adds a seconds field and changes nothing
/// else. QED.
///
/// The assumptions describe a third-party grammar and can change with a
/// crate upgrade. The `crontab_properties` test checks (i) and (ii) against
/// the crate on every `cargo test`.
///
/// Two crontab meanings have no Quartz equivalent and are rejected instead
/// of rewritten: numeric days of week ([`CRONTAB_DAY_OF_WEEK`]), and
/// day-of-month and day-of-week both set (crontab runs when either
/// matches; the crate runs only when both match).
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
