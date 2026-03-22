use anyhow::{Context, Result};
use clap::Subcommand;
use log::info;
use std::{env, fs, path::PathBuf, process::Command};

#[derive(Subcommand)]
pub enum InstallCommands {
    /// Install shell aliases to ~/.bashrc.d/composer.bash
    Bash,
    /// Install shell aliases to ~/.zshrc.d/composer.zsh
    Zsh,
    /// Install and enable a systemd service unit
    Systemd {
        /// User to run as (default: SUDO_USER or current user)
        #[clap(long, default_value_t = default_user())]
        user: String,
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
    /// Show installation status
    Status,
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

fn default_user() -> String {
    env::var("SUDO_USER").unwrap_or_else(|_| whoami::username())
}

pub fn install(command: InstallCommands) -> Result<()> {
    match command {
        InstallCommands::Bash => install_shell_aliases("bash", ".bashrc.d", "composer.bash"),
        InstallCommands::Zsh => install_shell_aliases("zsh", ".zshrc.d", "composer.zsh"),
        InstallCommands::Status => install_status(),
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

fn install_shell_aliases(shell: &str, dir_name: &str, file_name: &str) -> Result<()> {
    const ALIASES_CONTENT: &str = include_str!("aliases.bash");

    let home = env::var("HOME").context("HOME environment variable not set")?;
    let dir = PathBuf::from(&home).join(dir_name);
    let target_file = dir.join(file_name);

    info!("installing {shell} aliases to {}", target_file.display());

    if !dir.exists() {
        info!("creating directory: {}", dir.display());
        fs::create_dir_all(&dir).with_context(|| {
            format!("failed to create directory: {}", dir.display())
        })?;
    }

    fs::write(&target_file, ALIASES_CONTENT).with_context(|| {
        format!("failed to write file: {}", target_file.display())
    })?;

    info!("successfully installed {shell} aliases to {}", target_file.display());
    println!("Installed aliases to ~/{dir_name}/{file_name}");

    Ok(())
}

fn install_status() -> Result<()> {
    // Binary info
    let exe = env::current_exe().context("failed to get current executable path")?;
    let version = env!("CARGO_PKG_VERSION");
    println!("composer v{version}");
    println!("  binary: {}", exe.display());

    // Shell aliases
    let home = env::var("HOME").unwrap_or_default();
    let bash_aliases = PathBuf::from(&home).join(".bashrc.d/composer.bash");
    let zsh_aliases = PathBuf::from(&home).join(".zshrc.d/composer.zsh");
    if bash_aliases.exists() {
        println!("  bash aliases: {}", bash_aliases.display());
    } else {
        println!("  bash aliases: not installed");
    }
    if zsh_aliases.exists() {
        println!("  zsh aliases: {}", zsh_aliases.display());
    } else {
        println!("  zsh aliases: not installed");
    }

    // Platform-specific service status
    if cfg!(target_os = "linux") {
        print_systemd_status();
    } else if cfg!(target_os = "macos") {
        print_launchd_status(&home);
    }

    Ok(())
}

pub fn uninstall() -> Result<()> {
    let home = env::var("HOME").unwrap_or_default();
    let mut removed = vec![];

    // Stop and remove systemd service
    let unit_path = PathBuf::from("/etc/systemd/system/composer.service");
    if unit_path.exists() {
        let _ = Command::new("systemctl")
            .args(["stop", "composer"])
            .status();
        let _ = Command::new("systemctl")
            .args(["disable", "composer"])
            .status();
        if fs::remove_file(&unit_path).is_ok() {
            let _ = Command::new("systemctl")
                .arg("daemon-reload")
                .status();
            removed.push(format!("systemd unit: {}", unit_path.display()));
        }
    }

    // Unload and remove launchd plist
    let plist_path =
        PathBuf::from(&home).join("Library/LaunchAgents/com.architect.composer.plist");
    if plist_path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .status();
        if fs::remove_file(&plist_path).is_ok() {
            removed.push(format!("launchd plist: {}", plist_path.display()));
        }
    }

    // Remove shell aliases
    for (name, path) in [
        ("bash aliases", PathBuf::from(&home).join(".bashrc.d/composer.bash")),
        ("zsh aliases", PathBuf::from(&home).join(".zshrc.d/composer.zsh")),
    ] {
        if path.exists() {
            if fs::remove_file(&path).is_ok() {
                removed.push(format!("{name}: {}", path.display()));
            }
        }
    }

    if removed.is_empty() {
        println!("Nothing to uninstall.");
    } else {
        for item in &removed {
            println!("Removed {item}");
        }
        // Don't remove the binary since we're running from it
        let exe = env::current_exe().context("failed to get current executable path")?;
        println!("\nTo complete uninstall, remove the binary:");
        println!("  sudo rm {}", exe.display());
    }

    Ok(())
}

pub fn update() -> Result<()> {
    let exe = env::current_exe().context("failed to get current executable path")?;

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let artifact = format!(
        "composer-{}-{}",
        os,
        match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => anyhow::bail!("unsupported architecture: {arch}"),
        }
    );

    let url = format!(
        "https://github.com/architect-xyz/composer/releases/latest/download/{artifact}"
    );

    println!("Downloading {artifact}...");
    let status = Command::new("curl")
        .args(["-fsSL", &url, "-o", &exe.to_string_lossy()])
        .status()
        .context("failed to run curl")?;
    if !status.success() {
        anyhow::bail!("download failed");
    }

    // Ensure executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755))?;
    }

    println!("Updated composer at {}", exe.display());

    // Print new version
    let _ = Command::new(&exe).arg("--version").status();

    Ok(())
}

fn print_systemd_status() {
    let unit_path = PathBuf::from("/etc/systemd/system/composer.service");
    if !unit_path.exists() {
        println!("  systemd: not installed");
        return;
    }
    println!("  systemd: {}", unit_path.display());

    // Parse key fields from the unit file
    if let Ok(contents) = fs::read_to_string(&unit_path) {
        for line in contents.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("User=") {
                println!("    user: {val}");
            } else if let Some(val) = line.strip_prefix("WorkingDirectory=") {
                println!("    working dir: {val}");
            } else if let Some(val) = line.strip_prefix("ExecStart=") {
                println!("    exec: {val}");
            } else if let Some(val) = line.strip_prefix("Environment=") {
                if val != "RUST_LOG=composer=info" {
                    println!("    env: {val}");
                }
            }
        }
    }

    // Check service state via systemctl
    if let Ok(output) = Command::new("systemctl")
        .args(["is-active", "composer"])
        .output()
    {
        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let enabled = Command::new("systemctl")
            .args(["is-enabled", "composer"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        println!("    state: {state}, {enabled}");
    }
}

fn print_launchd_status(home: &str) {
    let plist_path =
        PathBuf::from(home).join("Library/LaunchAgents/com.architect.composer.plist");
    if !plist_path.exists() {
        println!("  launchd: not installed");
        return;
    }
    println!("  launchd: {}", plist_path.display());

    // Parse key fields from the plist (just grep for the values after known keys)
    if let Ok(contents) = fs::read_to_string(&plist_path) {
        let lines: Vec<&str> = contents.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed == "<key>WorkingDirectory</key>" {
                if let Some(next) = lines.get(i + 1) {
                    if let Some(val) = extract_plist_string(next) {
                        println!("    working dir: {val}");
                    }
                }
            }
        }
    }

    // Check service state via launchctl
    if let Ok(output) = Command::new("launchctl")
        .args(["list", "com.architect.composer"])
        .output()
    {
        if output.status.success() {
            // Parse PID from launchctl list output (format: PID\tStatus\tLabel)
            let out = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = out.lines().last().unwrap_or("").split('\t').collect();
            match parts.first().copied() {
                Some("-") => println!("    state: not running"),
                Some(pid) if !pid.is_empty() => println!("    state: running (pid {pid})"),
                _ => println!("    state: loaded"),
            }
        } else {
            println!("    state: not loaded");
        }
    }
}

fn extract_plist_string(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    trimmed.strip_prefix("<string>")?.strip_suffix("</string>")
}

fn install_systemd(
    user: &str,
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

    let mut env_lines = vec!["Environment=RUST_LOG=composer=info".to_string()];
    for kv in extra_env {
        env_lines.push(format!("Environment={kv}"));
    }
    let env_section = env_lines.join("\n");

    println!("Installing systemd service:");
    println!("  user: {user}");
    println!("  working dir: {working_dir}");
    println!("  compose file: {compose_file}");
    for kv in extra_env {
        println!("  env: {kv}");
    }
    let confirm = inquire::Confirm::new("Proceed?")
        .with_default(true)
        .prompt()
        .context("failed to read confirmation")?;
    if !confirm {
        println!("Aborted.");
        return Ok(());
    }

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

    println!("Installing launchd service:");
    println!("  working dir: {working_dir}");
    println!("  compose file: {compose_file}");
    for kv in extra_env {
        println!("  env: {kv}");
    }
    let confirm = inquire::Confirm::new("Proceed?")
        .with_default(true)
        .prompt()
        .context("failed to read confirmation")?;
    if !confirm {
        println!("Aborted.");
        return Ok(());
    }

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

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
