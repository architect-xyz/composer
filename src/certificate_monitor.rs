use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use log::{info, trace};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateMonitorConfig {
    pub urls: Vec<String>,
    #[serde(default = "default_warn_threshold_days")]
    pub warn_threshold_days: u32,
}

fn default_warn_threshold_days() -> u32 {
    15
}

#[derive(Default, Debug, Clone)]
struct CertificateStatus {
    url: String,
    state: CertificateState,
    days_until_expiry: Option<i64>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum CertificateState {
    Valid,   // > warn_threshold_days
    Warning, // <= warn_threshold_days but not expired
    Expired, // Past expiration date
    Error,   // Unable to check (network/parsing error)
}

impl Default for CertificateState {
    fn default() -> Self {
        CertificateState::Valid
    }
}

#[derive(Default, Debug, Clone)]
struct CertificateMonitorStatus {
    certificates: Vec<CertificateStatus>,
}

impl CertificateMonitorStatus {
    fn is_ok(&self) -> bool {
        self.certificates.iter().all(|cert| cert.state == CertificateState::Valid)
    }

    fn is_qualitatively_different(&self, other: &Self) -> bool {
        if self.certificates.len() != other.certificates.len() {
            return true;
        }

        for (cert1, cert2) in self.certificates.iter().zip(other.certificates.iter()) {
            if cert1.url != cert2.url || cert1.state != cert2.state {
                return true;
            }
        }
        false
    }
}

async fn check_all_certificates(
    config: &CertificateMonitorConfig,
) -> CertificateMonitorStatus {
    let mut certificates = Vec::new();
    for url in &config.urls {
        let cert_status = check_certificate(url, config.warn_threshold_days).await;
        trace!("certificate status for {}: {:?}", url, cert_status);
        certificates.push(cert_status);
    }
    CertificateMonitorStatus { certificates }
}

pub async fn check_once(
    host_name: String,
    config: CertificateMonitorConfig,
) -> Result<()> {
    let status = check_all_certificates(&config).await;
    let output = format_status_for_stdout(&host_name, Utc::now(), &status);
    println!("{}", output);
    Ok(())
}

pub async fn run(
    host_name: String,
    config: CertificateMonitorConfig,
    slack_webhook_url: Option<String>,
    slack_webhook_on_error_url: Option<String>,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(3600)); // Check every hour
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_status;
    let mut status = CertificateMonitorStatus::default();
    let mut first_status = true;

    loop {
        interval.tick().await;
        last_status = std::mem::take(&mut status);

        // Check all certificates
        status = check_all_certificates(&config).await;

        // Notify of status changes
        if status.is_qualitatively_different(&last_status) || first_status {
            first_status = false;
            info!("certificate monitor status changed: {:?}", status);
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

async fn check_certificate(url: &str, warn_threshold_days: u32) -> CertificateStatus {
    let mut cert_status = CertificateStatus {
        url: url.to_string(),
        state: CertificateState::Valid,
        days_until_expiry: None,
        error: None,
    };

    match get_certificate_expiry(url).await {
        Ok(expiry_date) => {
            let now = Utc::now();
            let days_until_expiry = (expiry_date - now).num_days();
            cert_status.days_until_expiry = Some(days_until_expiry);

            if days_until_expiry < 0 {
                cert_status.state = CertificateState::Expired;
            } else if days_until_expiry <= warn_threshold_days as i64 {
                cert_status.state = CertificateState::Warning;
            } else {
                cert_status.state = CertificateState::Valid;
            }
        }
        Err(e) => {
            cert_status.state = CertificateState::Error;
            cert_status.error = Some(e.to_string());
        }
    }

    cert_status
}

async fn get_certificate_expiry(url: &str) -> Result<DateTime<Utc>> {
    // Create a client with TLS info enabled, and allow invalid certificates
    let client = reqwest::Client::builder()
        .tls_info(true)
        .danger_accept_invalid_certs(true) // Accept expired/self-signed certs to check them
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to create HTTP client")?;

    // Make a HEAD request to get TLS info without downloading content
    let res = client
        .head(url)
        .send()
        .await
        .with_context(|| format!("failed to connect to {}", url))?;

    // Extract TLS certificate information
    let tls_info = res
        .extensions()
        .get::<reqwest::tls::TlsInfo>()
        .ok_or_else(|| anyhow!("no TLS info available - not an HTTPS connection?"))?;

    let cert_der =
        tls_info.peer_certificate().ok_or_else(|| anyhow!("no peer certificate"))?;

    // Parse the certificate
    use x509_parser::prelude::*;
    let (_, cert) =
        X509Certificate::from_der(cert_der).context("failed to parse certificate")?;

    let validity = cert.validity();
    let not_after = validity.not_after;

    // Convert ASN.1 time to DateTime<Utc>
    let expiry = DateTime::<Utc>::from_timestamp(not_after.timestamp(), 0)
        .ok_or_else(|| anyhow!("invalid certificate expiry date"))?;

    Ok(expiry)
}

fn format_status_lines(
    host_name: &str,
    run_at: DateTime<Utc>,
    status: &CertificateMonitorStatus,
    for_slack: bool,
) -> Vec<String> {
    let mut lines = vec![];
    let status_emoji = if status.is_ok() { "🟢" } else { "🟡" };

    // Check if any certificates are expired (should be red)
    let has_expired =
        status.certificates.iter().any(|cert| cert.state == CertificateState::Expired);
    let status_emoji = if has_expired { "🔴" } else { status_emoji };

    if for_slack {
        lines.push(format!("{status_emoji} *certificate monitor* for {host_name}"));
        lines.push("".to_string());
        lines.push(format!(
            "<!date^{}^Run at {{date_num}} {{time_secs}}|Run at {run_at}>",
            run_at.timestamp()
        ));
    } else {
        lines.push(format!("{status_emoji} Certificate monitor for {host_name}"));
        lines.push("".to_string());
        lines.push(format!("Run at {run_at}"));
    }
    lines.push("".to_string());

    if status.is_ok() {
        lines.push("🛫 All certificates valid".to_string());
    }

    for cert in &status.certificates {
        match cert.state {
            CertificateState::Valid => {
                if let Some(days) = cert.days_until_expiry {
                    lines.push(format!("✅ {}: {} days until expiry", cert.url, days));
                } else {
                    lines.push(format!("✅ {}: Valid", cert.url));
                }
            }
            CertificateState::Warning => {
                if let Some(days) = cert.days_until_expiry {
                    lines.push(format!("⚠️ {}: {} days until expiry", cert.url, days));
                } else {
                    lines.push(format!("⚠️ {}: Expiring soon", cert.url));
                }
            }
            CertificateState::Expired => {
                if let Some(days) = cert.days_until_expiry {
                    lines.push(format!("❌ {}: Expired {} days ago", cert.url, -days));
                } else {
                    lines.push(format!("❌ {}: Expired", cert.url));
                }
            }
            CertificateState::Error => {
                let error_msg = cert.error.as_deref().unwrap_or("Unknown error");
                lines.push(format!("🚨 {}: Error - {}", cert.url, error_msg));
            }
        }
    }

    lines
}

fn format_status_for_stdout(
    host_name: &str,
    run_at: DateTime<Utc>,
    status: &CertificateMonitorStatus,
) -> String {
    let lines = format_status_lines(host_name, run_at, status, false);
    lines.join("\n")
}

async fn notify_slack_with_changes(
    url: &str,
    host_name: &str,
    run_at: DateTime<Utc>,
    status: &CertificateMonitorStatus,
) -> Result<()> {
    let lines = format_status_lines(host_name, run_at, status, true);
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
