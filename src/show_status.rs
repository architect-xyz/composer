use crate::compose::{compose_command, load_compose_config, ComposeContext};
use anyhow::{bail, Context, Result};
use prettytable_rs::{color, format, row, Attr, Cell, Row, Table};
use std::{collections::BTreeMap, process::Stdio};

const RUN_KEYS: [&str; 1] = ["co.architect.composer.run"];

#[derive(Debug)]
struct ServiceInfo {
    profile: String,
    name: String,
    service_type: String, // "job" or "service"
}

#[derive(serde::Deserialize)]
struct DockerComposePsJson {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
}

pub async fn show_status(context: &ComposeContext) -> Result<()> {
    // Load compose config with all profiles
    let compose = load_compose_config(context, Some("*")).await?;

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
    let mut status_map: BTreeMap<String, String> = BTreeMap::new();
    for line in stdout_s.lines() {
        if let Ok(row) = serde_json::from_str::<DockerComposePsJson>(line) {
            status_map.insert(row.service, row.state);
        }
    }

    // Build and print table
    if services_info.is_empty() {
        println!("No services found in compose file.");
        return Ok(());
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

    table.set_titles(row!["Profile", "Name", "Type", "Status"]);

    for info in &services_info {
        let raw_state = status_map.get(&info.name).map(|s| s.as_str());

        // For jobs: only show RUNNING, otherwise show nothing
        // For services: show UP/DOWN as before
        let status_cell = if info.service_type == "job" {
            if raw_state == Some("running") {
                Cell::new("RUNNING").with_style(Attr::ForegroundColor(color::GREEN))
            } else {
                Cell::new("FINISHED")
            }
        } else {
            // Service: use UP/DOWN
            let display_status = if raw_state == Some("running") { "UP" } else { "DOWN" };
            if display_status == "UP" {
                Cell::new(display_status).with_style(Attr::ForegroundColor(color::GREEN))
            } else {
                Cell::new(display_status).with_style(Attr::ForegroundColor(color::RED))
            }
        };

        // Create row with colored status cell
        table.add_row(Row::new(vec![
            Cell::new(&info.profile),
            Cell::new(&info.name),
            Cell::new(&info.service_type),
            status_cell,
        ]));
    }

    table.printstd();

    Ok(())
}
