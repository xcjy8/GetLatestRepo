//! Status command handling

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

use crate::db::Database;
use crate::git::GitOps;
use crate::reporter::terminal::print_repo_detail;

/// Execute status command
pub async fn execute(path: PathBuf, show_diff: bool) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Unable to access path: {}", path.display()))?;

    if !GitOps::is_repository(&canonical) {
        anyhow::bail!("Not a valid Git repository: {}", canonical.display());
    }

    // Try getting from database
    let db = Database::open()?;
    let repo = match db.get_repository(&canonical.to_string_lossy())? {
        Some(r) => r,
        None => {
            // Realtime scan
            let parent = canonical
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            GitOps::inspect(&canonical, &parent)?
        }
    };

    print_repo_detail(&repo);

    if show_diff && repo.dirty {
        println!("\n{} Local changed files:", "📝".yellow());
        for file in &repo.dirty_files {
            println!("  - {}", file);
        }
    }

    Ok(())
}
