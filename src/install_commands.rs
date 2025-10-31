use anyhow::{Context, Result};
use clap::Subcommand;
use log::info;
use std::{env, fs, path::PathBuf};

#[derive(Subcommand)]
pub enum InstallCommands {
    /// Install bash aliases
    Bash,
}

pub fn install(command: InstallCommands) -> Result<()> {
    match command {
        InstallCommands::Bash => {
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
    }
}
