use crate::compose::{compose_command, ComposeContext};
use anyhow::{anyhow, bail, Context, Result};
use prettytable_rs::{color, format, row, Attr, Cell, Row, Table};
use std::{collections::BTreeMap, process::Stdio};
use term::terminfo::{TermInfo, TerminfoTerminal};

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];

#[derive(Debug)]
pub struct ServiceInfo {
    pub profile: String,
    pub name: String,
    pub service_type: String, // "job" or "service"
}

#[derive(serde::Deserialize)]
struct DockerComposePsJson {
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
}

pub async fn gather_status_data(
    context: &ComposeContext,
    compose: &crate::compose_types::Compose,
) -> Result<(Vec<ServiceInfo>, BTreeMap<String, ContainerStatus>)> {
    // Collect service information
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
    let cmd_out = cmd.output().await.context("running docker compose ps")?;
    if !cmd_out.status.success() {
        let stderr = String::from_utf8_lossy(&cmd_out.stderr);
        bail!("docker compose ps failed: {stderr}");
    }

    let stdout_s = String::from_utf8_lossy(&cmd_out.stdout);
    let mut status_map: BTreeMap<String, ContainerStatus> = BTreeMap::new();
    for line in stdout_s.lines() {
        if let Ok(row) = serde_json::from_str::<DockerComposePsJson>(line) {
            status_map.insert(
                row.service,
                ContainerStatus {
                    state: row.state,
                    status: row.status,
                    image: row.image,
                },
            );
        }
    }

    Ok((services_info, status_map))
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

/// Scan an image reference for a semver-shaped tag like `v1.2.3` or
/// `v1.2.3-beta.1` and return it if present.
fn extract_version(image: &str) -> Option<String> {
    let bytes = image.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'v' {
            let boundary =
                i == 0 || matches!(bytes[i - 1], b':' | b'/' | b'@' | b'-' | b'_' | b'.');
            if boundary {
                if let Some(end) = parse_semver_tail(bytes, i + 1) {
                    return std::str::from_utf8(&bytes[i..end]).ok().map(str::to_string);
                }
            }
        }
        i += 1;
    }
    None
}

/// Parse `N.N.N(-suffix)?` starting at `start`. Returns the end index if the
/// parse succeeds, None otherwise.
fn parse_semver_tail(bytes: &[u8], start: usize) -> Option<usize> {
    let n = bytes.len();
    let mut i = start;
    for seg in 0..3 {
        let seg_start = i;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == seg_start {
            return None;
        }
        if seg < 2 {
            if i >= n || bytes[i] != b'.' {
                return None;
            }
            i += 1;
        }
    }
    if i < n && bytes[i] == b'-' {
        i += 1;
        while i < n
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'-')
        {
            i += 1;
        }
    }
    Some(i)
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

    table.set_titles(row!["Profile", "Name", "Type", "Status", "Version"]);

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

        let version = container
            .map(|c| c.image.as_str())
            .and_then(extract_version)
            .unwrap_or_else(|| "?".to_string());

        table.add_row(Row::new(vec![
            Cell::new(&info.profile),
            Cell::new(&info.name),
            Cell::new(&info.service_type),
            status_cell,
            Cell::new(&version),
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
    }

    #[test]
    fn extract_version_missing_returns_none() {
        assert_eq!(extract_version("nginx:latest"), None);
        assert_eq!(extract_version("postgres"), None);
        assert_eq!(extract_version("service:1.2.3"), None); // no 'v' prefix
        assert_eq!(extract_version("service:v1.2"), None); // not full semver
        assert_eq!(extract_version(""), None);
    }

    #[test]
    fn extract_version_ignores_v_inside_words() {
        // The 'v' in 'nova' should not be treated as a version prefix.
        assert_eq!(extract_version("nova:latest"), None);
        assert_eq!(extract_version("service:stable"), None);
    }

    #[test]
    fn extract_version_with_digest_suffix() {
        // Even when a digest follows, we still surface the tag.
        assert_eq!(
            extract_version("nginx:v1.2.3@sha256:abc123"),
            Some("v1.2.3".to_string())
        );
    }
}
