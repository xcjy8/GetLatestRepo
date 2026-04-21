//! Status command handling

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

use crate::db::Database;
use crate::git::GitOps;
use crate::reporter::terminal::{print_issues_view, print_repo_detail};

/// Execute status command
pub async fn execute(path: PathBuf, show_diff: bool, issues: bool) -> Result<()> {
    let db = Database::open()?;
    
    if issues {
        let repos = tokio::task::spawn_blocking(move || {
            db.list_repositories()
        }).await
            .map_err(|e| anyhow::anyhow!("Failed to list repositories: {}", e))??;
        print_issues_view(&repos);
        return Ok(());
    }
    
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Unable to access path: {}", path.display()))?;

    if !GitOps::is_repository(&canonical) {
        anyhow::bail!("Not a valid Git repository: {}", canonical.display());
    }

    let repo = match db.get_repository(&canonical.to_string_lossy())? {
        Some(r) => r,
        None => {
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
