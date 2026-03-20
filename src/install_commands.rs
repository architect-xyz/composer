use anyhow::{Context, Result};
use clap::Subcommand;
use log::info;
use std::{env, fs, path::PathBuf};

#[derive(Subcommand)]
pub enum InstallCommands {
    /// Install bash aliases to ~/.bashrc.d/composer.bash
    Bash,
    /// Install and enable a systemd service unit
    Systemd {
        /// User to run as (default: current user)
        #[clap(long, default_value_t = whoami::username())]
        user: String,
        /// Working directory (default: user's home)
        #[clap(long)]
        working_dir: Option<String>,
        /// Compose file path relative to working dir (default: compose.yml)
        #[clap(long, default_value = "compose.yml")]
        compose_file: String,
        /// Extra environment variables (KEY=VALUE), repeatable
        #[clap(long)]
        env: Vec<String>,
    },
    /// Install a macOS launchd plist for running composer as a daemon
    Launchd {
        /// Working directory (default: current directory)
        #[clap(long)]
        working_dir: Option<String>,
        /// Compose file path relative to working dir (default: compose.yml)
        #[clap(long, default_value = "compose.yml")]
        compose_file: String,
        /// Extra environment variables (KEY=VALUE), repeatable
        #[clap(long)]
        env: Vec<String>,
    },
}

pub fn install(command: InstallCommands) -> Result<()> {
    match command {
        InstallCommands::Bash => install_bash(),
        InstallCommands::Systemd {
            user,
            working_dir,
            compose_file,
            env,
        } => install_systemd(&user, working_dir, &compose_file, &env),
        InstallCommands::Launchd {
            working_dir,
            compose_file,
            env,
        } => install_launchd(working_dir, &compose_file, &env),
    }
}

fn install_bash() -> Result<()> {
    const ALIASES_CONTENT: &str = include_str!("aliases.bash");

    let home = env::var("HOME").context("HOME environment variable not set")?;
    let bashrc_d = PathBuf::from(&home).join(".bashrc.d");
    let target_file = bashrc_d.join("composer.bash");

    info!("installing bash aliases to {}", target_file.display());

    // Create .bashrc.d directory if it doesn't exist
    if !bashrc_d.exists() {
        info!("creating directory: {}", bashrc_d.display());
        fs::create_dir_all(&bashrc_d).with_context(|| {
            format!("failed to create directory: {}", bashrc_d.display())
        })?;
    }

    // Write the aliases file
    fs::write(&target_file, ALIASES_CONTENT).with_context(|| {
        format!("failed to write file: {}", target_file.display())
    })?;

    info!("successfully installed bash aliases to {}", target_file.display());
    println!("Installed aliases to ~/.bashrc.d/composer.bash");

    Ok(())
}

fn install_systemd(
    user: &str,
    working_dir: Option<String>,
    compose_file: &str,
    extra_env: &[String],
) -> Result<()> {
    let working_dir = match working_dir {
        Some(dir) => dir,
        None => {
            // Default to user's home directory
            let home = home_dir_for_user(user)?;
            home.to_string_lossy().to_string()
        }
    };

    let composer_bin = env::current_exe()
        .context("failed to get current executable path")?
        .to_string_lossy()
        .to_string();

    let mut env_lines = vec!["Environment=RUST_LOG=composer=info".to_string()];
    for kv in extra_env {
        env_lines.push(format!("Environment={kv}"));
    }
    let env_section = env_lines.join("\n");

    let unit = format!(
        "\
[Unit]
Description=Composer – Docker Compose scheduler
After=docker.service
Requires=docker.service

[Service]
Type=simple
User={user}
WorkingDirectory={working_dir}
ExecStart={composer_bin} -f {compose_file}
Restart=on-failure
RestartSec=5s
{env_section}

[Install]
WantedBy=multi-user.target
"
    );

    let unit_path = PathBuf::from("/etc/systemd/system/composer.service");
    info!("writing systemd unit to {}", unit_path.display());
    fs::write(&unit_path, &unit).with_context(|| {
        format!(
            "failed to write unit file to {} (are you running as root?)",
            unit_path.display()
        )
    })?;

    // Reload systemd
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .context("failed to run systemctl daemon-reload")?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed with status {status}");
    }

    println!("Installed systemd unit to {}", unit_path.display());
    println!("Run `systemctl enable --now composer` to start the service.");

    Ok(())
}

fn install_launchd(
    working_dir: Option<String>,
    compose_file: &str,
    extra_env: &[String],
) -> Result<()> {
    let working_dir = match working_dir {
        Some(dir) => dir,
        None => env::current_dir()
            .context("failed to get current directory")?
            .to_string_lossy()
            .to_string(),
    };

    let composer_bin = env::current_exe()
        .context("failed to get current executable path")?
        .to_string_lossy()
        .to_string();

    let env_dict = if extra_env.is_empty() {
        String::new()
    } else {
        let mut entries = String::from("    <key>EnvironmentVariables</key>\n    <dict>\n");
        for kv in extra_env {
            if let Some((key, value)) = kv.split_once('=') {
                entries.push_str(&format!(
                    "        <key>{}</key>\n        <string>{}</string>\n",
                    xml_escape(key),
                    xml_escape(value)
                ));
            }
        }
        entries.push_str("    </dict>\n");
        entries
    };

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.architect.composer</string>
    <key>ProgramArguments</key>
    <array>
        <string>{composer_bin}</string>
        <string>-f</string>
        <string>{compose_file}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{working_dir}</string>
{env_dict}    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/composer.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/composer.err</string>
</dict>
</plist>
"#
    );

    let home = env::var("HOME").context("HOME environment variable not set")?;
    let launch_agents = PathBuf::from(&home).join("Library/LaunchAgents");
    if !launch_agents.exists() {
        fs::create_dir_all(&launch_agents).with_context(|| {
            format!("failed to create directory: {}", launch_agents.display())
        })?;
    }
    let plist_path = launch_agents.join("com.architect.composer.plist");

    info!("writing launchd plist to {}", plist_path.display());
    fs::write(&plist_path, &plist).with_context(|| {
        format!("failed to write plist to {}", plist_path.display())
    })?;

    println!("Installed launchd plist to {}", plist_path.display());
    println!("Run `launchctl load {}` to start the service.", plist_path.display());

    Ok(())
}

fn home_dir_for_user(user: &str) -> Result<PathBuf> {
    // Try current user's HOME first
    if user == whoami::username() {
        if let Ok(home) = env::var("HOME") {
            return Ok(PathBuf::from(home));
        }
    }
    // Fall back to /home/<user>
    Ok(PathBuf::from(format!("/home/{user}")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
