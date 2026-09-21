use crate::{
    compose::{compose_command, spawn_error, ComposeContext},
    last_run::{self, RunHistory},
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use log::debug;
use prettytable_rs::{color, format, row, Attr, Cell, Row, Table};
use regex::Regex;
use std::{collections::BTreeMap, process::Stdio, sync::LazyLock};
use term::terminfo::{TermInfo, TerminfoTerminal};

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];

#[derive(Debug)]
pub struct ServiceInfo {
    pub profile: String,
    pub name: String,
    pub service_type: String, // "job" or "service"
    /// Image reference declared in the compose file, if any
    pub image: Option<String>,
    /// When composer itself last ran or restarted this service
    pub history: RunHistory,
}

#[derive(serde::Deserialize)]
struct DockerComposePsJson {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Image", default)]
    image: String,
}

#[derive(Debug, Default)]
pub struct ContainerStatus {
    pub state: String,
    pub status: String,
    pub image: String,
    /// When the container was last (re)started
    pub started_at: Option<DateTime<Utc>>,
    /// Id (sha256:...) of the image the container is running
    pub image_id: Option<String>,
    /// The image's `org.opencontainers.image.version` label, if set
    pub image_version_label: Option<String>,
}

pub async fn gather_status_data(
    context: &ComposeContext,
    compose: &crate::compose_types::Compose,
) -> Result<(Vec<ServiceInfo>, BTreeMap<String, ContainerStatus>)> {
    // Collect service information
    let last_runs = last_run::load(context);
    let mut services_info: Vec<ServiceInfo> = Vec::new();
    for (name, service_opt) in &compose.services {
        if let Some(service) = service_opt {
            // Get profile (first one if multiple)
            let profile = service
                .profiles
                .as_ref()
                .and_then(|p| p.first().cloned())
                .unwrap_or_else(|| "".to_string());

            // Determine service type: job if has co.architect.composer.run, service otherwise
            let service_type = if let Some(labels) = &service.labels {
                let mut is_job = false;
                for key in labels.keys() {
                    if RUN_KEYS.contains(&key.as_str()) {
                        is_job = true;
                        break;
                    }
                }
                if is_job {
                    "job"
                } else {
                    "service"
                }
            } else {
                "service"
            };

            services_info.push(ServiceInfo {
                profile,
                name: name.clone(),
                service_type: service_type.to_string(),
                image: service.image.clone(),
                history: last_run::history(last_runs.as_ref(), name),
            });
        }
    }

    // Sort by profile, then by name
    services_info
        .sort_by(|a, b| a.profile.cmp(&b.profile).then_with(|| a.name.cmp(&b.name)));

    // Query docker compose ps to get status
    let mut cmd = compose_command(context, None::<&str>);
    cmd.arg("ps")
        .arg("--all")
        .arg("--format")
        .arg("json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let cmd_out = cmd
        .output()
        .await
        .map_err(|e| spawn_error("docker", e))
        .context("running docker compose ps")?;
    if !cmd_out.status.success() {
        let stderr = String::from_utf8_lossy(&cmd_out.stderr);
        bail!("docker compose ps failed: {stderr}");
    }

    let stdout_s = String::from_utf8_lossy(&cmd_out.stdout);
    let mut status_map: BTreeMap<String, ContainerStatus> = BTreeMap::new();
    let mut container_ids: BTreeMap<String, String> = BTreeMap::new(); // id -> service
    for line in stdout_s.lines() {
        if let Ok(row) = serde_json::from_str::<DockerComposePsJson>(line) {
            if !row.id.is_empty() {
                container_ids.insert(row.id, row.service.clone());
            }
            status_map.insert(
                row.service,
                ContainerStatus {
                    state: row.state,
                    status: row.status,
                    image: row.image,
                    started_at: None,
                    image_id: None,
                    image_version_label: None,
                },
            );
        }
    }

    // Inspect containers for their last start time, in one call.
    if !container_ids.is_empty() {
        for details in inspect_containers(container_ids.keys()).await? {
            // compose ps reports short ids; inspect reports full ids
            let service = container_ids
                .iter()
                .find(|(id, _)| details.id.starts_with(id.as_str()))
                .map(|(_, service)| service);
            if let Some(container) = service.and_then(|s| status_map.get_mut(s)) {
                container.started_at = details.started_at;
                container.image_id = Some(details.image_id);
                container.image_version_label = details.version_label;
            }
        }
    }

    Ok((services_info, status_map))
}

struct ContainerDetails {
    id: String,
    started_at: Option<DateTime<Utc>>,
    image_id: String,
    version_label: Option<String>,
}

const OCI_VERSION_LABEL: &str = "org.opencontainers.image.version";

/// Run `docker inspect` on the given container ids, returning the start
/// time, image id, and OCI version label (container labels inherit the
/// image's labels) of each one found.  Containers that have disappeared since they
/// were listed are skipped rather than failing the whole status.
async fn inspect_containers<I, S>(ids: I) -> Result<Vec<ContainerDetails>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("inspect")
        .arg("--type")
        .arg("container")
        .arg("--format")
        .arg(format!(
            "{{{{.Id}}}}\t{{{{.State.StartedAt}}}}\t{{{{.Image}}}}\t{{{{index .Config.Labels \"{OCI_VERSION_LABEL}\"}}}}"
        ))
        .args(ids)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd
        .output()
        .await
        .map_err(|e| spawn_error("docker", e))
        .context("running docker inspect")?;
    if !out.status.success() {
        // partial output is still usable; docker prints an error per
        // missing container but inspects the rest
        let stderr = String::from_utf8_lossy(&out.stderr);
        debug!("docker inspect exited with {}: {}", out.status, stderr.trim());
    }
    let stdout_s = String::from_utf8_lossy(&out.stdout);
    Ok(stdout_s
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let id = parts.next()?.to_string();
            let started_at = parse_docker_time(parts.next()?);
            let image_id = parts.next()?.to_string();
            let version_label = parts.next().and_then(parse_label_value);
            Some(ContainerDetails { id, started_at, image_id, version_label })
        })
        .collect())
}

/// Parse a label value as emitted by a docker inspect `--format` template.
/// A missing label renders as the literal `<no value>` on engines where the
/// lookup yields a nil interface, and as an empty string on others; both
/// mean the label is not set.
fn parse_label_value(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "<no value>").then(|| s.to_string())
}

/// Parse an RFC 3339 timestamp as emitted by docker inspect.  Docker uses
/// the zero time (`0001-01-01T00:00:00Z`) for "never", which maps to None.
fn parse_docker_time(s: &str) -> Option<DateTime<Utc>> {
    let dt = DateTime::parse_from_rfc3339(s.trim()).ok()?.with_timezone(&Utc);
    if dt.timestamp() <= 0 {
        return None;
    }
    Some(dt)
}

/// Render a timestamp for the status table in host-local time, minute
/// precision, with the numeric UTC offset so the reader can correlate it
/// against UTC-stamped sources (container logs, `docker inspect`, CI).
fn format_time(dt: Option<DateTime<Utc>>) -> String {
    format_time_in(dt, &Local)
}

fn format_time_in<Tz: chrono::TimeZone>(dt: Option<DateTime<Utc>>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match dt {
        Some(dt) => dt.with_timezone(tz).format("%Y-%m-%d %H:%M %:z").to_string(),
        None => UNKNOWN.to_string(),
    }
}

/// Coarse "how long ago" hint: `45s`, `23m`, `5h`, `12d`.
fn short_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

// Every "no value" in the table is one of these, so a reader can tell them
// apart without guessing:
/// nothing to inspect (no container, and nothing in the compose file to go on)
const NOT_APPLICABLE: &str = "-";
// (a job composer has never run reads `not since <time>`, see [`not_since_cell`])
/// composer genuinely cannot tell
const UNKNOWN: &str = "unknown";

/// Version column: what the container runs, else what the compose file
/// declares.  `unknown` only when there *is* a container and nothing could
/// be determined; `-` when there's simply nothing to inspect.
fn version_cell(info: &ServiceInfo, container: Option<&ContainerStatus>) -> String {
    let image = container.map(|c| c.image.as_str()).or(info.image.as_deref());
    match detect_version(image, container) {
        Some(v) => v,
        None if container.is_some() => UNKNOWN.to_string(),
        None => NOT_APPLICABLE.to_string(),
    }
}

/// Started column: the container's last start when there is one; otherwise
/// composer's own record of when it last ran the service.  `--rm` jobs leave
/// no container, so for them the record is the only source.
fn started_cell(
    info: &ServiceInfo,
    container: Option<&ContainerStatus>,
    now: DateTime<Utc>,
) -> String {
    if let Some(container) = container {
        return format_time(container.started_at);
    }
    match &info.history {
        RunHistory::Last(run) => format!(
            "{} ({} ago)",
            format_time(Some(run.started_at)),
            short_duration(now - run.started_at)
        ),
        // a plain service is started by compose, not by composer, so the
        // absence of a composer record says nothing about it
        _ if info.service_type != "job" => NOT_APPLICABLE.to_string(),
        RunHistory::NotSince(since) => not_since_cell(*since, &Local),
        RunHistory::Unknown => UNKNOWN.to_string(),
    }
}

/// A job composer has never run: `not since <time>`, the time being how far
/// back composer's record goes.  The record can be lost (e.g. with a
/// recreated scheduler container), so a bare "never" would claim too much.
fn not_since_cell<Tz: chrono::TimeZone>(since: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!("not since {}", format_time_in(Some(since), tz))
}

/// Condense docker's "Up 3 hours" status text into a short form like "3h".
/// Returns None if the input doesn't match the running-uptime format.
fn short_uptime(status: &str) -> Option<String> {
    let s = status.strip_prefix("Up ")?;
    // Drop trailing health/parenthetical annotations like " (healthy)".
    let s = s.split(" (").next()?.trim();

    match s {
        "Less than a second" => return Some("<1s".to_string()),
        "About a minute" => return Some("1m".to_string()),
        "About an hour" => return Some("1h".to_string()),
        _ => {}
    }

    let (num, unit) = s.split_once(' ')?;
    let n: u64 = num.parse().ok()?;
    let suffix = match unit {
        "second" | "seconds" => "s",
        "minute" | "minutes" => "m",
        "hour" | "hours" => "h",
        "day" | "days" => "d",
        "week" | "weeks" => "w",
        "month" | "months" => "mo",
        "year" | "years" => "y",
        _ => return None,
    };
    Some(format!("{n}{suffix}"))
}

/// The tag portion of an image reference, e.g. `v1.2.3` from
/// `ghcr.io/org/app:v1.2.3@sha256:...`.  None if the reference has no tag.
fn image_tag(image: &str) -> Option<&str> {
    let without_digest = image.split('@').next()?;
    // registries may carry a port (`localhost:5000/app`), so only look at
    // the last path component for the tag separator
    let name = without_digest.rsplit('/').next()?;
    let (_, tag) = name.split_once(':')?;
    (!tag.is_empty()).then_some(tag)
}

/// Matches a version-shaped token in an image tag.  Handles tags that are a
/// plain version (`v1.2.3`, `3.12`) and tags that embed one after a
/// separator (`hello-world-v1.2.3`, `bob-jones-2.0`).
static TAG_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?:^|[-_+])                 # preceded by the start of the tag or a separator
        (
            v?\d+(?:\.\d+)+          # N.N(.N)*; at least two segments, optional leading v
            (?:                     # optional prerelease suffix: -rc.1, -beta, -20240101
                -(?:\d|(?i:rc|alpha|beta|pre|dev|snapshot|nightly|canary|build))
                [A-Za-z0-9.-]*
            )?
        )
        (?:$|[-_+.])                # followed by the end of the tag or a separator
        ",
    )
    .expect("TAG_VERSION regex is valid")
});

/// Find a version-shaped token in an image reference's tag (see
/// [`TAG_VERSION`]).  Requires at least `N.N` (a lone `16` is too
/// ambiguous); a leading `v` is kept if present.  A `-suffix` is kept only
/// if it looks like a prerelease, so `1.25.3-alpine` yields `1.25.3` while
/// `v1.2.3-rc.1` is kept whole.
fn extract_version(image: &str) -> Option<String> {
    let tag = image_tag(image)?;
    TAG_VERSION.captures(tag).map(|c| c[1].to_string())
}

/// Best available version string for a service: the image's OCI version
/// label (the image's own claim about itself), else a version parsed from
/// the image tag, else the literal tag (`latest`, `main`), else the short
/// image id of the running container.
///
/// A floating tag is shown together with the short image id when there is
/// a container, e.g. `latest (102dbfdde2da)`: the tag says the service
/// tracks the registry, the id makes two hosts on `latest` comparable.
fn detect_version(
    image: Option<&str>,
    container: Option<&ContainerStatus>,
) -> Option<String> {
    if let Some(v) = container.and_then(|c| c.image_version_label.as_deref()) {
        return Some(v.to_string());
    }
    if let Some(v) = image.and_then(extract_version) {
        return Some(v);
    }
    let tag = image.and_then(image_tag);
    let image_id = container.and_then(|c| c.image_id.as_deref()).map(short_image_id);
    match (tag, image_id) {
        (Some(tag), Some(id)) => Some(format!("{tag} ({id})")),
        (Some(tag), None) => Some(tag.to_string()),
        (None, Some(id)) => Some(id),
        (None, None) => None,
    }
}

/// `sha256:102dbfdde2da60d2...` -> `102dbfdde2da`
fn short_image_id(id: &str) -> String {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    hex.chars().take(12).collect()
}

pub fn format_status_table(
    services_info: &[ServiceInfo],
    status_map: &BTreeMap<String, ContainerStatus>,
) -> Result<String> {
    if services_info.is_empty() {
        return Ok("No services found in compose file.\n".to_string());
    }

    let mut table = Table::new();

    // Custom format: box chars with no line separators between rows (except header)
    let custom_format = format::FormatBuilder::new()
        .column_separator('│')
        .borders('│')
        .separator(
            format::LinePosition::Top,
            format::LineSeparator::new('─', '┬', '┌', '┐'),
        )
        .separator(
            format::LinePosition::Title,
            format::LineSeparator::new('─', '┼', '├', '┤'),
        )
        .separator(
            format::LinePosition::Bottom,
            format::LineSeparator::new('─', '┴', '└', '┘'),
        )
        .padding(1, 1)
        .build();
    table.set_format(custom_format);

    table.set_titles(row!["Profile", "Name", "Type", "Status", "Version", "Started"]);

    let now = Utc::now();
    for info in services_info {
        let container = status_map.get(&info.name);
        let raw_state = container.map(|c| c.state.as_str());
        let uptime = container
            .filter(|c| c.state == "running")
            .and_then(|c| short_uptime(&c.status));

        let is_running = raw_state == Some("running");
        let (label, color) = if info.service_type == "job" {
            if is_running {
                ("JOB_RUNNING", Some(color::GREEN))
            } else {
                ("JOB", None)
            }
        } else if is_running {
            ("UP", Some(color::GREEN))
        } else {
            ("DOWN", Some(color::RED))
        };

        let status_text = match uptime {
            Some(u) => format!("{label} ({u})"),
            None => label.to_string(),
        };
        let mut status_cell = Cell::new(&status_text);
        if let Some(c) = color {
            status_cell = status_cell.with_style(Attr::ForegroundColor(c));
        }

        table.add_row(Row::new(vec![
            Cell::new(&info.profile),
            Cell::new(&info.name),
            Cell::new(&info.service_type),
            status_cell,
            Cell::new(&version_cell(info, container)),
            Cell::new(&started_cell(info, container, now)),
        ]));
    }

    // Convert table to string with ANSI colors preserved
    // Use TerminfoTerminal to wrap our buffer so print_term will emit ANSI codes
    // If terminfo is not available (non-TTY context), fall back to fake ANSI terminfo
    let mut buffer = Vec::new();

    // Try to get terminfo from environment first, otherwise use fake ANSI terminfo
    // "xterm" is in the ANSI fallback list, so from_name will always create a basic ANSI terminfo
    // with escape sequences like \x1B[3%p1%dm for colors
    let terminfo =
        TermInfo::from_env().or_else(|_| TermInfo::from_name("xterm")).map_err(|e| {
            anyhow!("failed to create terminfo (tried env and xterm fallback): {e:?}")
        })?;

    let mut terminal = TerminfoTerminal::new_with_terminfo(&mut buffer, terminfo);
    table.print_term(&mut terminal)?;
    drop(terminal); // Ensure terminal is dropped before converting buffer to string

    let out = String::from_utf8_lossy(&buffer).to_string();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_uptime_common_forms() {
        assert_eq!(short_uptime("Up 3 hours"), Some("3h".to_string()));
        assert_eq!(short_uptime("Up 5 minutes"), Some("5m".to_string()));
        assert_eq!(short_uptime("Up 1 second"), Some("1s".to_string()));
        assert_eq!(short_uptime("Up 12 days"), Some("12d".to_string()));
        assert_eq!(short_uptime("Up 2 weeks"), Some("2w".to_string()));
        assert_eq!(short_uptime("Up 4 months"), Some("4mo".to_string()));
        assert_eq!(short_uptime("Up 1 year"), Some("1y".to_string()));
    }

    #[test]
    fn short_uptime_approximate_forms() {
        assert_eq!(short_uptime("Up About a minute"), Some("1m".to_string()));
        assert_eq!(short_uptime("Up About an hour"), Some("1h".to_string()));
        assert_eq!(short_uptime("Up Less than a second"), Some("<1s".to_string()));
    }

    #[test]
    fn short_uptime_strips_health_annotation() {
        assert_eq!(short_uptime("Up 3 hours (healthy)"), Some("3h".to_string()));
        assert_eq!(short_uptime("Up 2 minutes (unhealthy)"), Some("2m".to_string()));
    }

    #[test]
    fn short_uptime_rejects_non_running() {
        assert_eq!(short_uptime("Exited (0) 5 minutes ago"), None);
        assert_eq!(short_uptime("Restarting (1) 3 seconds ago"), None);
        assert_eq!(short_uptime(""), None);
    }

    #[test]
    fn extract_version_common_tags() {
        assert_eq!(extract_version("nginx:v1.2.3"), Some("v1.2.3".to_string()));
        assert_eq!(
            extract_version("ghcr.io/org/svc:v0.10.12"),
            Some("v0.10.12".to_string())
        );
        assert_eq!(
            extract_version("registry.example.com/team/app:v2.0.0-beta.1"),
            Some("v2.0.0-beta.1".to_string())
        );
        assert_eq!(extract_version("service:1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(extract_version("alpine:3.12"), Some("3.12".to_string()));
        assert_eq!(extract_version("app:2024.08.22"), Some("2024.08.22".to_string()));
        assert_eq!(extract_version("localhost:5000/app:1.2"), Some("1.2".to_string()));
    }

    #[test]
    fn extract_version_embedded_in_tag() {
        assert_eq!(extract_version("app:hello-world-v1.2.3"), Some("v1.2.3".to_string()));
        assert_eq!(extract_version("app:bob-jones-2.0"), Some("2.0".to_string()));
        assert_eq!(extract_version("app:release_1.4.0"), Some("1.4.0".to_string()));
        assert_eq!(
            extract_version("app:hello-world2-v1.2.3"),
            Some("v1.2.3".to_string())
        );
    }

    #[test]
    fn extract_version_suffix_handling() {
        // image variants are dropped, prereleases are kept
        assert_eq!(extract_version("nginx:1.25.3-alpine"), Some("1.25.3".to_string()));
        assert_eq!(extract_version("python:3.11-slim"), Some("3.11".to_string()));
        assert_eq!(extract_version("app:v1.2.3-rc.1"), Some("v1.2.3-rc.1".to_string()));
        assert_eq!(extract_version("app:v1.2.3-beta"), Some("v1.2.3-beta".to_string()));
        assert_eq!(extract_version("app:2.0-20240101"), Some("2.0-20240101".to_string()));
        assert_eq!(extract_version("app:v1.2.3-bob-jones"), Some("v1.2.3".to_string()));
    }

    #[test]
    fn extract_version_missing_returns_none() {
        assert_eq!(extract_version("nginx:latest"), None);
        assert_eq!(extract_version("postgres"), None);
        assert_eq!(extract_version("postgres:16"), None); // single number: too ambiguous
        assert_eq!(extract_version("app:main"), None);
        assert_eq!(extract_version("app:sha-abc1234"), None);
        assert_eq!(extract_version("app:build-42"), None);
        assert_eq!(extract_version("app:2.0abc"), None);
        assert_eq!(extract_version("localhost:5000/app"), None); // port, no tag
        assert_eq!(extract_version("my2.0app:latest"), None); // only the tag is scanned
        assert_eq!(extract_version(""), None);
    }

    #[test]
    fn extract_version_ignores_v_inside_words() {
        // The 'v' in 'nova' should not be treated as a version prefix.
        assert_eq!(extract_version("nova:latest"), None);
        assert_eq!(extract_version("service:stable"), None);
        assert_eq!(extract_version("app:hello-worldv1.2.3"), None); // no boundary before v
    }

    #[test]
    fn extract_version_with_digest_suffix() {
        // Even when a digest follows, we still surface the tag.
        assert_eq!(
            extract_version("nginx:v1.2.3@sha256:abc123"),
            Some("v1.2.3".to_string())
        );
        assert_eq!(extract_version("nginx@sha256:abc123"), None);
    }

    fn container(label: Option<&str>, id: Option<&str>) -> ContainerStatus {
        ContainerStatus {
            image_version_label: label.map(str::to_string),
            image_id: id.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn detect_version_prefers_label_then_tag_then_id() {
        let c = container(Some("0.159.0"), Some("sha256:102dbfdde2da60d2b8ec"));
        assert_eq!(
            detect_version(Some("app:v1.2.3"), Some(&c)),
            Some("0.159.0".to_string())
        );
        assert_eq!(
            detect_version(Some("app:latest"), Some(&c)),
            Some("0.159.0".to_string())
        );
        let c = container(None, Some("sha256:102dbfdde2da60d2b8ec"));
        assert_eq!(
            detect_version(Some("app:v1.2.3"), Some(&c)),
            Some("v1.2.3".to_string())
        );
        assert_eq!(
            detect_version(Some("app:latest"), Some(&c)),
            Some("latest (102dbfdde2da)".to_string())
        );
        assert_eq!(detect_version(Some("app:latest"), None), Some("latest".to_string()));
        assert_eq!(detect_version(None, None), None);
    }

    #[test]
    fn detect_version_falls_back_to_literal_tag() {
        let c = container(None, Some("sha256:102dbfdde2da60d2b8ec"));
        // non-semver tags are shown as-is, with the image id when running
        assert_eq!(detect_version(Some("app:main"), None), Some("main".to_string()));
        assert_eq!(
            detect_version(Some("app:sha-abc1234"), Some(&c)),
            Some("sha-abc1234 (102dbfdde2da)".to_string())
        );
        assert_eq!(
            detect_version(Some("ironsh/iron-proxy:latest"), Some(&c)),
            Some("latest (102dbfdde2da)".to_string())
        );
        // a semver-shaped tag is still reduced to the version alone
        assert_eq!(
            detect_version(Some("nginx:1.25.3-alpine"), Some(&c)),
            Some("1.25.3".to_string())
        );
        // no tag at all: docker implies latest, but only the id is certain
        assert_eq!(
            detect_version(Some("postgres"), Some(&c)),
            Some("102dbfdde2da".to_string())
        );
        assert_eq!(detect_version(Some("postgres"), None), None);
        assert_eq!(
            detect_version(Some("app@sha256:abcdef"), Some(&c)),
            Some("102dbfdde2da".to_string())
        );
    }

    #[test]
    fn parse_label_value_missing_forms() {
        assert_eq!(parse_label_value("0.159.0"), Some("0.159.0".to_string()));
        assert_eq!(parse_label_value(" v1.2.3 "), Some("v1.2.3".to_string()));
        assert_eq!(parse_label_value(""), None);
        assert_eq!(parse_label_value("  "), None);
        assert_eq!(parse_label_value("<no value>"), None);
    }

    fn service(
        service_type: &str,
        image: Option<&str>,
        history: RunHistory,
    ) -> ServiceInfo {
        ServiceInfo {
            profile: String::new(),
            name: "svc".to_string(),
            service_type: service_type.to_string(),
            image: image.map(str::to_string),
            history,
        }
    }

    fn run_at(secs_ago: i64, now: DateTime<Utc>) -> crate::last_run::LastRun {
        crate::last_run::LastRun {
            action: "run".to_string(),
            started_at: now - chrono::Duration::seconds(secs_ago),
            finished_at: None,
            success: None,
        }
    }

    #[test]
    fn version_cell_sentinels() {
        // no container: derive from the compose file, else nothing to inspect
        assert_eq!(
            version_cell(
                &service(
                    "job",
                    Some("app:1.2.3"),
                    RunHistory::NotSince(DateTime::UNIX_EPOCH)
                ),
                None
            ),
            "1.2.3"
        );
        assert_eq!(
            version_cell(
                &service(
                    "job",
                    Some("app:latest"),
                    RunHistory::NotSince(DateTime::UNIX_EPOCH)
                ),
                None
            ),
            "latest"
        );
        assert_eq!(
            version_cell(
                &service("job", Some("app"), RunHistory::NotSince(DateTime::UNIX_EPOCH)),
                None
            ),
            "-"
        );
        assert_eq!(
            version_cell(&service("service", None, RunHistory::Unknown), None),
            "-"
        );
        // a container with nothing determinable is the one truly unknown case
        let bare = ContainerStatus { image: "app".to_string(), ..Default::default() };
        assert_eq!(
            version_cell(&service("service", None, RunHistory::Unknown), Some(&bare)),
            "unknown"
        );
    }

    #[test]
    fn not_since_cell_says_how_far_back_the_record_goes() {
        let since = parse_docker_time("2026-08-21T20:58:12Z").unwrap();
        assert_eq!(not_since_cell(since, &Utc), "not since 2026-08-21 20:58 +00:00");
        let cell = started_cell(
            &service("job", None, RunHistory::NotSince(since)),
            None,
            Utc::now(),
        );
        assert!(cell.starts_with("not since 2026-08-2"), "{cell}");
        // still not applicable to a plain service
        assert_eq!(
            started_cell(
                &service("service", None, RunHistory::NotSince(since)),
                None,
                Utc::now()
            ),
            "-"
        );
    }

    #[test]
    fn started_cell_sentinels() {
        let now = Utc::now();
        // jobs: composer's record decides
        assert_eq!(
            started_cell(&service("job", None, RunHistory::Unknown), None, now),
            "unknown"
        );
        let cell = started_cell(
            &service("job", None, RunHistory::NotSince(DateTime::UNIX_EPOCH)),
            None,
            now,
        );
        assert!(cell.starts_with("not since "), "{cell}");
        let cell = started_cell(
            &service("job", None, RunHistory::Last(run_at(23 * 60 + 5, now))),
            None,
            now,
        );
        assert!(cell.ends_with(" (23m ago)"), "{cell}");
        // services: compose starts them, so no record means nothing to say
        assert_eq!(
            started_cell(
                &service("service", None, RunHistory::NotSince(DateTime::UNIX_EPOCH)),
                None,
                now
            ),
            "-"
        );
        assert_eq!(
            started_cell(&service("service", None, RunHistory::Unknown), None, now),
            "-"
        );
        // a container's own start time always wins; a container without one is unknown
        let started = ContainerStatus { started_at: Some(now), ..Default::default() };
        let cell = started_cell(
            &service("job", None, RunHistory::NotSince(DateTime::UNIX_EPOCH)),
            Some(&started),
            now,
        );
        assert!(!cell.contains("ago") && !cell.starts_with("not since"), "{cell}");
        let unstarted = ContainerStatus::default();
        assert_eq!(
            started_cell(
                &service("service", None, RunHistory::Unknown),
                Some(&unstarted),
                now
            ),
            "unknown"
        );
    }

    #[test]
    fn short_duration_units() {
        use chrono::Duration;
        assert_eq!(short_duration(Duration::seconds(45)), "45s");
        assert_eq!(short_duration(Duration::seconds(23 * 60 + 14)), "23m");
        assert_eq!(short_duration(Duration::hours(5) + Duration::minutes(59)), "5h");
        assert_eq!(short_duration(Duration::days(12)), "12d");
        assert_eq!(short_duration(Duration::seconds(-5)), "0s");
    }

    #[test]
    fn format_time_shows_numeric_offset() {
        use chrono::FixedOffset;
        let dt = parse_docker_time("2026-08-21T20:58:12Z");
        let chicago = FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(format_time_in(dt, &chicago), "2026-08-21 15:58 -05:00");
        let kolkata = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        assert_eq!(format_time_in(dt, &kolkata), "2026-08-22 02:28 +05:30");
        assert_eq!(format_time_in(dt, &Utc), "2026-08-21 20:58 +00:00");
        assert_eq!(format_time_in(None, &Utc), UNKNOWN);
    }

    #[test]
    fn parse_docker_time_rfc3339_nanos() {
        let dt = parse_docker_time("2026-08-22T12:34:56.123456789Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-08-22T12:34:56.123456789+00:00");
    }

    #[test]
    fn parse_docker_time_zero_is_none() {
        assert_eq!(parse_docker_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_docker_time(""), None);
        assert_eq!(parse_docker_time("not a time"), None);
    }
}
