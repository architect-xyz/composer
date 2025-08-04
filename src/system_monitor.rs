use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use log::{info, trace, warn};
use metrics::gauge;
use metrics_exporter_opentelemetry::Recorder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, str::FromStr, time::Duration};
use sysinfo::{Disks, System};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMonitorConfig {
    /// Warn if memory usage exceeds this percentage
    #[serde(default = "default_warn_memory_pct")]
    warn_memory_pct: Option<f64>,
    /// Unwarn if memory usage drops below this percentage;
    /// if not set, will be the same as warn_memory_pct (no hysteresis)
    #[serde(default)]
    unwarn_memory_pct: Option<f64>,
    /// Warn if swap usage exceeds this percentage
    #[serde(default = "default_warn_swap_pct")]
    warn_swap_pct: Option<f64>,
    /// Unwarn if swap usage drops below this percentage;
    /// if not set, will be the same as warn_swap_pct (no hysteresis)
    #[serde(default)]
    unwarn_swap_pct: Option<f64>,
    /// Warn if disk usage for any disk exceeds this percentage
    #[serde(default = "default_warn_disk_pct")]
    warn_disk_pct: Option<f64>,
    /// Unwarn if disk usage for any disk drops below this percentage;
    /// if not set, will be the same as warn_disk_pct (no hysteresis)
    #[serde(default)]
    unwarn_disk_pct: Option<f64>,
    #[serde(default)]
    ignore_disk_mounts_smaller_than: Option<String>,
}

fn default_warn_memory_pct() -> Option<f64> {
    Some(80.0)
}

fn default_warn_swap_pct() -> Option<f64> {
    Some(80.0)
}

fn default_warn_disk_pct() -> Option<f64> {
    Some(80.0)
}

#[derive(Default, Debug, Clone)]
struct SystemMonitorStatus {
    memory_warning: Option<f64>,
    swap_warning: Option<f64>,
    // disk mount name => (percentage used, total size in bytes)
    disk_warnings: BTreeMap<String, (f64, u64)>,
}

impl SystemMonitorStatus {
    fn is_ok(&self) -> bool {
        self.memory_warning.is_none()
            && self.swap_warning.is_none()
            && self.disk_warnings.is_empty()
    }

    fn is_qualitatively_different(&self, other: &Self) -> bool {
        self.memory_warning.is_some() != other.memory_warning.is_some()
            || self.swap_warning.is_some() != other.swap_warning.is_some()
            || self.disk_warnings.keys().collect::<Vec<_>>()
                != other.disk_warnings.keys().collect::<Vec<_>>()
    }
}

pub async fn run(
    host_name: String,
    config: SystemMonitorConfig,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
) -> Result<()> {
    // initialize telemetry, if the environment variables are set
    let _recorder = match initialize_telemetry(&host_name) {
        Ok(recorder) => Some(recorder),
        Err(e) => {
            warn!("failed to initialize telemetry, metrics will not be collected: {e:?}");
            None
        }
    };
    let mut sys = System::new_all();
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_status;
    let mut status = SystemMonitorStatus::default();
    let mut first_status = true;
    let disk_size_threshold = match config.ignore_disk_mounts_smaller_than {
        Some(size) => Some(
            byte_unit::Byte::from_str(&size)
                .context("invalid disk size threshold")?
                .as_u64(),
        ),
        None => None,
    };

    let gauge_memory_used_pct = gauge!("memory.used_pct");
    let gauge_memory_used_bytes = gauge!("memory.used_bytes");
    let gauge_memory_total_bytes = gauge!("memory.total_bytes");
    let gauge_swap_used_pct = gauge!("swap.used_pct");
    let gauge_swap_used_bytes = gauge!("swap.used_bytes");
    let gauge_swap_total_bytes = gauge!("swap.total_bytes");
    // root disk "/" only
    let gauge_disk_used_pct = gauge!("disk.used_pct");
    let gauge_disk_used_bytes = gauge!("disk.used_bytes");
    let gauge_disk_total_bytes = gauge!("disk.total_bytes");

    loop {
        interval.tick().await;
        sys.refresh_all();
        last_status = std::mem::take(&mut status);

        let pct_mem_used = sys.used_memory() as f64 / sys.total_memory() as f64 * 100.0;
        let pct_swap_used = sys.used_swap() as f64 / sys.total_swap() as f64 * 100.0;
        trace!("total memory: {} bytes", sys.total_memory());
        trace!(" used memory: {} bytes ({:.2}%)", sys.used_memory(), pct_mem_used);
        trace!("  total swap: {} bytes", sys.total_swap());
        trace!("   used swap: {} bytes ({:.2}%)", sys.used_swap(), pct_swap_used);
        trace!("---");

        gauge_memory_used_pct.set(pct_mem_used);
        gauge_memory_used_bytes.set(sys.used_memory() as f64);
        gauge_memory_total_bytes.set(sys.total_memory() as f64);
        gauge_swap_used_pct.set(pct_swap_used);
        gauge_swap_used_bytes.set(sys.used_swap() as f64);
        gauge_swap_total_bytes.set(sys.total_swap() as f64);

        if let Some(warn_memory_pct) = config.warn_memory_pct {
            let unwarn_memory_pct = config.unwarn_memory_pct.unwrap_or(warn_memory_pct);
            if pct_mem_used >= warn_memory_pct {
                status.memory_warning = Some(pct_mem_used);
            } else if pct_mem_used < unwarn_memory_pct {
                status.memory_warning = None;
            }
        }

        if let Some(warn_swap_pct) = config.warn_swap_pct {
            let unwarn_swap_pct = config.unwarn_swap_pct.unwrap_or(warn_swap_pct);
            if pct_swap_used >= warn_swap_pct {
                status.swap_warning = Some(pct_swap_used);
            } else if pct_swap_used < unwarn_swap_pct {
                status.swap_warning = None;
            }
        }

        let disks = Disks::new_with_refreshed_list();
        for disk in &disks {
            if disk.total_space() == 0 {
                continue;
            }
            if let Some(size_threshold) = disk_size_threshold {
                if disk.total_space() < size_threshold {
                    continue;
                }
            }
            let disk_mount = disk.mount_point().to_string_lossy().to_string();
            let pct_disk_used = (1.0
                - (disk.available_space() as f64 / disk.total_space() as f64))
                * 100.0;
            trace!(
                "disk usage {:.2}% of {} bytes total ({})",
                pct_disk_used,
                disk.total_space(),
                disk_mount,
            );

            if disk_mount == "/" {
                gauge_disk_used_pct.set(pct_disk_used);
                gauge_disk_used_bytes
                    .set((disk.total_space() - disk.available_space()) as f64);
                gauge_disk_total_bytes.set(disk.total_space() as f64);
            }

            if let Some(warn_disk_pct) = config.warn_disk_pct {
                let unwarn_disk_pct = config.unwarn_disk_pct.unwrap_or(warn_disk_pct);
                if pct_disk_used >= warn_disk_pct {
                    status
                        .disk_warnings
                        .insert(disk_mount, (pct_disk_used, disk.total_space()));
                } else if pct_disk_used < unwarn_disk_pct {
                    status.disk_warnings.remove(&disk_mount);
                }
            }
        }
        trace!("---");

        // notify of status changes
        if status.is_qualitatively_different(&last_status) || first_status {
            first_status = false;
            info!("system monitor status changed: {:?}", status);
            let mut slack_webhook_urls = vec![];
            if let Some(url) = &slack_webhook_url {
                slack_webhook_urls.push(url.clone());
            }
            if let Some(url) = &slack_webhook_on_error_url {
                slack_webhook_urls.push(url.clone());
            }
            for slack_webhook_url in slack_webhook_urls {
                notify_slack_with_changes(
                    slack_webhook_url.as_str(),
                    &host_name,
                    Utc::now(),
                    &status,
                )
                .await?;
            }
        }
    }
}

async fn notify_slack_with_changes(
    url: &str,
    host_name: &str,
    run_at: DateTime<Utc>,
    status: &SystemMonitorStatus,
) -> Result<()> {
    let mut lines = vec![];
    let status_emoji = if status.is_ok() { "🟢" } else { "🟡" };
    lines.push(format!("{status_emoji} *system monitor* for {host_name}"));
    lines.push("".to_string());
    lines.push(format!(
        "<!date^{}^Run at {{date_num}} {{time_secs}}|Run at {run_at}>",
        run_at.timestamp()
    ));
    lines.push("".to_string());
    if status.is_ok() {
        lines.push("🛫 System metrics nominal".to_string());
    }
    if let Some(pct_mem_used) = status.memory_warning {
        lines.push(format!("❗ Memory usage: {pct_mem_used:.2}%"));
    }
    if let Some(pct_swap_used) = status.swap_warning {
        lines.push(format!("❗ Swap usage: {pct_swap_used:.2}%"));
    }
    for (disk_mount, (pct_disk_used, total_size)) in &status.disk_warnings {
        let gigabytes = *total_size as f64 / 1024.0 / 1024.0 / 1024.0;
        lines.push(format!(
            "❗ Disk usage: {pct_disk_used:.2}% of {:.2} GiB ({disk_mount})",
            gigabytes
        ));
    }
    let text =
        lines.iter().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
    let client = reqwest::Client::new();
    let res = client.post(url).json(&json!({ "text": text })).send().await?;
    if !res.status().is_success() {
        let err_body = res.text().await.context("reading response body")?;
        return Err(anyhow!("{err_body}"));
    }
    Ok(())
}

fn initialize_telemetry(host_name: &str) -> Result<Recorder> {
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::MetricExporterBuilder;
    let otlp_exporter = MetricExporterBuilder::new().with_http().build()?;
    let recorder = Recorder::builder(env!("CARGO_PKG_NAME"))
        .with_instrumentation_scope(|scope| {
            scope.with_attributes([
                KeyValue::new("host.name", host_name.to_string()),
                KeyValue::new("service.name", "composer"),
            ])
        })
        .with_meter_provider(|mpb| {
            // Periodically push out with our OTLP exporter
            mpb.with_periodic_exporter(otlp_exporter)
        })
        .install_global()?;
    Ok(recorder)
}
