//! `composer start|stop|restart`: control the composer daemon installed by
//! `composer install systemd` / `composer install launchd`.

use crate::install_commands::{
    launchd_plist_path, launchd_state, systemd_state, LAUNCHD_LABEL, SYSTEMD_UNIT_PATH,
};
use anyhow::{bail, Context, Result};
use std::{env, fmt, path::PathBuf, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl fmt::Display for ServiceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceAction::Start => write!(f, "start"),
            ServiceAction::Stop => write!(f, "stop"),
            ServiceAction::Restart => write!(f, "restart"),
        }
    }
}

/// Which service manager composer is installed under, detected the same way
/// `composer install status` does: by the presence of the unit / plist.
enum ServiceManager {
    Systemd,
    Launchd { plist_path: PathBuf },
}

fn detect_service_manager() -> Result<ServiceManager> {
    let home = env::var("HOME").unwrap_or_default();
    let systemd = PathBuf::from(SYSTEMD_UNIT_PATH);
    let launchd = launchd_plist_path(&home);
    let native_is_launchd = cfg!(target_os = "macos");
    // Prefer the platform-native manager if, improbably, both are present.
    let manager = match (systemd.exists(), launchd.exists()) {
        (true, true) if native_is_launchd => {
            Some(ServiceManager::Launchd { plist_path: launchd.clone() })
        }
        (true, _) => Some(ServiceManager::Systemd),
        (false, true) => Some(ServiceManager::Launchd { plist_path: launchd.clone() }),
        (false, false) => None,
    };
    if let Some(manager) = manager {
        return Ok(manager);
    }
    let hint = if native_is_launchd {
        "composer install launchd"
    } else {
        "composer install systemd"
    };
    bail!(
        "composer is not installed as a service (no {} or {}); run `{hint}` first",
        systemd.display(),
        launchd.display()
    );
}

pub fn control(action: ServiceAction) -> Result<()> {
    match detect_service_manager()? {
        ServiceManager::Systemd => systemd_control(action),
        ServiceManager::Launchd { plist_path } => launchd_control(action, &plist_path),
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Run a command, echoing it first, and fail with its exit status if it
/// does not succeed.
fn run(program: &str, args: &[&str]) -> Result<()> {
    println!("$ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("`{program} {}` failed with {status}", args.join(" "));
    }
    Ok(())
}

fn systemd_control(action: ServiceAction) -> Result<()> {
    let verb = action.to_string();
    if is_root() {
        run("systemctl", &[&verb, "composer"])?;
    } else {
        run("sudo", &["systemctl", &verb, "composer"])?;
    }
    if let Some(state) = systemd_state() {
        println!("systemd: composer.service is {state}");
    }
    Ok(())
}

/// Whether the launchd service is currently bootstrapped (loaded) into the
/// user's GUI domain.
fn launchd_is_loaded() -> bool {
    Command::new("launchctl")
        .args(["list", LAUNCHD_LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn launchd_control(action: ServiceAction, plist_path: &std::path::Path) -> Result<()> {
    let domain = format!("gui/{}", current_uid());
    let target = format!("{domain}/{LAUNCHD_LABEL}");
    let plist = plist_path.to_string_lossy();
    let loaded = launchd_is_loaded();
    match action {
        // `kickstart -k` restarts a loaded service; a service that was never
        // bootstrapped (or was booted out) has to be bootstrapped instead,
        // which starts it because the plist sets RunAtLoad.
        ServiceAction::Restart => {
            if loaded {
                run("launchctl", &["kickstart", "-k", &target])?;
            } else {
                run("launchctl", &["bootstrap", &domain, &plist])?;
            }
        }
        ServiceAction::Start => {
            if loaded {
                run("launchctl", &["kickstart", &target])?;
            } else {
                run("launchctl", &["bootstrap", &domain, &plist])?;
            }
        }
        // The plist sets KeepAlive, so merely killing the process would
        // have launchd bring it straight back; unloading it is the only
        // way to actually stop it.  `composer start` bootstraps it again.
        ServiceAction::Stop => {
            if loaded {
                run("launchctl", &["bootout", &target])?;
            } else {
                println!("launchd: {LAUNCHD_LABEL} is not loaded; nothing to stop");
            }
        }
    }
    // After a (re)start, launchd respawns the process asynchronously (the
    // plist sets KeepAlive), so give it a moment to come back up before
    // reporting the state.
    let mut state = launchd_state();
    if action != ServiceAction::Stop {
        for _ in 0..20 {
            if state.as_deref().is_some_and(|s| s.starts_with("running")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            state = launchd_state();
        }
    }
    if let Some(state) = state {
        println!("launchd: {LAUNCHD_LABEL} is {state}");
    }
    Ok(())
}
