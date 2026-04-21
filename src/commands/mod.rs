//! Command handler module
//! 
//! Separate CLI command handling logic from main.rs, one module per subcommand.

pub mod init;
pub mod scan;
pub mod fetch;
pub mod status;
pub mod config;
pub mod workflow;
pub mod discard;

use anyhow::Result;
use colored::Colorize;

/// Print command execution success message
pub fn print_success(message: &str) {
    println!("{} {}", "✓".green(), message);
}

/// Print informational message
pub fn print_info(message: &str) {
    println!("{} {}", "ℹ".blue(), message);
}

#[allow(dead_code)]
/// Print warning message
pub fn print_warning(message: &str) {
    println!("{} {}", "⚠".yellow(), message);
}

#[allow(dead_code)]
/// Print error message
pub fn print_error(message: &str) {
    eprintln!("{} {}", "✗".red(), message);
}

/// Check if application is initialized
pub fn ensure_initialized() -> Result<(crate::config::AppConfig, crate::db::Database)> {
    let config = crate::config::AppConfig::load()?;
    if !config.is_initialized() {
        anyhow::bail!("Not initialized. Please run: getlatestrepo init <path>");
    }
    let db = crate::db::Database::open()?;
    Ok((config, db))
}
