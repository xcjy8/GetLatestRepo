//! Config command handling

use anyhow::Result;
use colored::Colorize;
use crate::cli::ConfigCommands;
use crate::config::AppConfig;
use crate::db::Database;

/// Execute config command
pub async fn execute(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Add { path } => {
            let mut config = AppConfig::load()?;
            config.add_scan_source(&path)?;

            // Sync to database
            let db = Database::open()?;
            if let Some(source) = config.scan_sources.last() {
                let mut source_clone = source.clone();
                db.upsert_scan_source(&mut source_clone)?;
            }

            println!("{} Added scan source: {}", "✓".green(), path.display());
        }
        ConfigCommands::List => {
            let config = AppConfig::load()?;

            if config.scan_sources.is_empty() {
                println!("{} No scan sources configured", "!".yellow());
            } else {
                println!("{} Configured scan sources:\n", "ℹ".blue());
                for (idx, source) in config.scan_sources.iter().enumerate() {
                    println!("  {}. {}", idx + 1, source.root_path);
                    println!(
                        "     Depth: {} | Ignore: {:?}",
                        source.max_depth, source.ignore_patterns
                    );
                }
            }
        }
        ConfigCommands::Remove { path_or_id } => {
            let mut config = AppConfig::load()?;

            // Try to delete from database first
            let db = Database::open()?;
            if let Ok(id) = path_or_id.parse::<i64>() {
                db.delete_scan_source(id)?;
            }
            // Also update config file
            config.remove_scan_source(&path_or_id)?;
            println!("{} Removed scan source", "✓".green());
        }
        ConfigCommands::Ignore { patterns } => {
            let mut config = AppConfig::load()?;
            let pattern_list: Vec<String> =
                patterns.split(',').map(|s| s.trim().to_string()).collect();
            config.set_ignore_patterns(pattern_list.clone())?;
            println!("{} Set ignore rules: {:?}", "✓".green(), pattern_list);
        }
        ConfigCommands::Path => {
            println!("Config file: {}", AppConfig::config_path()?.display());
            println!("Database: {}", Database::db_path()?.display());
        }
    }

    Ok(())
}
