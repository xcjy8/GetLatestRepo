use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

use crate::concurrent::execute_concurrent_raw;
use crate::db::Database;
use crate::git::GitOps;
use crate::models::{Repository, ScanSource};

/// Repository scanner
pub struct Scanner;

impl Scanner {
    /// Scan single source directory (concurrent inspect)
    pub async fn scan_source(
        source: &ScanSource,
        db: &Database,
        progress: bool,
    ) -> Result<Vec<Repository>> {
        let root = Path::new(&source.root_path);

        if !root.exists() {
            anyhow::bail!("ScanPath does not exist: {}", source.root_path);
        }

        // Find all .git directories (synchronous, IO-bound but fast)
        let git_dirs = Self::find_git_dirs(root, source)?;

        let pb: Option<Arc<Mutex<ProgressBar>>> = if progress {
            let bar = ProgressBar::new(git_dirs.len() as u64);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")?
                    .progress_chars("#>-")
            );
            Some(Arc::new(Mutex::new(bar)))
        } else {
            None
        };

        // ── Concurrent inspect ──────────────────────────────────────────────
        // Use unified concurrent executor, solving the following problems:
        // - Auto-handle panics (won't cause hung)
        // - Uses blocking wait (no busy-wait)
        // - Reasonable timeout (5 seconds)
        const MAX_CONCURRENT: usize = 8;
        
        // Build task list
        let tasks: Vec<_> = git_dirs
            .into_iter()
            .map(|git_dir| {
                let repo_path = git_dir.parent().unwrap_or(&git_dir).to_path_buf();
                let root_path = source.root_path.clone();
                let pb = pb.clone();
                
                move || {
                    let repo_name = repo_path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    
                    if let Some(ref bar) = pb {
                        if let Ok(bar) = bar.lock() {
                            bar.set_message(format!("Scan {}", repo_name));
                        }
                    }

                    let result = GitOps::inspect(&repo_path, &root_path);

                    if let Some(ref bar) = pb {
                        if let Ok(bar) = bar.lock() {
                            bar.inc(1);
                        }
                    }

                    result.map_err(|e| e.to_string())
                }
            })
            .collect();

        // Execute concurrent tasks
        let results = execute_concurrent_raw(tasks, MAX_CONCURRENT);
        
        let mut repos = Vec::new();
        let mut errors = Vec::new();

        for result in results {
            match result {
                Some(Ok(repo)) => repos.push(repo),
                Some(Err(e)) => errors.push(e),
                None => errors.push("Scan task panicked".to_string()),
            }
        }

        if let Some(ref bar) = pb {
            if let Ok(bar) = bar.lock() {
                bar.finish_with_message("Scan complete");
            }
        }

        // Display errors
        for err in &errors {
            eprintln!("⚠ {}", err);
        }

        // Batch write to the database serially to ensure SQLite consistency
        for repo in &mut repos {
            if let Err(e) = db.upsert_repository(repo) {
                eprintln!("Warning: failed to save repository '{}': {}", crate::utils::sanitize_path(&repo.path), e);
            }
        }

        // Clean up deleted repository records
        Self::cleanup_deleted_repos(db, &source.root_path, &repos)?;

        Ok(repos)
    }

    /// Find all Git repository directories
    fn find_git_dirs(root: &Path, source: &ScanSource) -> Result<Vec<std::path::PathBuf>> {
        let mut git_dirs = Vec::new();
        let max_depth = source.max_depth as usize;

        let walker = WalkDir::new(root)
            .max_depth(max_depth)
            .follow_links(source.follow_symlinks)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                // Skip ignored directories (but keep .git for detection)
                if name == ".git" {
                    return true; // Keep .git directory for detection
                }
                !source.ignore_patterns.iter().any(|p| {
                    name == *p || name.starts_with(p.trim_end_matches('*'))
                })
            });

        for entry in walker {
            match entry {
                Ok(e) => {
                    // Check for .git directory
                    if e.file_name() == ".git" && e.file_type().is_dir() {
                        git_dirs.push(e.path().to_path_buf());
                    }
                }
                Err(e) => {
                    // Log WalkDir errors but don't interrupt the scan
                    if let Some(path) = e.path() {
                        eprintln!("   Warning: Unable to access path '{}': {}", path.display(), e);
                    } else {
                        eprintln!("   Warning: Scan error: {}", e);
                    }
                }
            }
        }

        Ok(git_dirs)
    }

    /// Clean up repository records that no longer exist in the database
    fn cleanup_deleted_repos(
        db: &Database,
        root_path: &str,
        current_repos: &[Repository],
    ) -> Result<()> {
        // Get all records under this root_path
        let existing = db.list_repositories()?;
        let current_paths: std::collections::HashSet<String> = current_repos
            .iter()
            .map(|r| r.path.clone())
            .collect();

        for repo in existing {
            if repo.root_path == root_path && !current_paths.contains(&repo.path) {
                // Repository has been deleted
                db.delete_repository(&repo.path)?;
            }
        }

        Ok(())
    }

    /// Scan all configured sources
    pub async fn scan_all(
        sources: &[ScanSource],
        db: &Database,
        progress: bool,
    ) -> Result<Vec<Repository>> {
        let mut all_repos = Vec::new();

        for source in sources {
            if !source.enabled {
                continue;
            }

            if progress {
                println!("\n📁 Scan: {}", source.root_path);
            }

            match Self::scan_source(source, db, progress).await {
                Ok(mut repos) => {
                    all_repos.append(&mut repos);
                }
                Err(e) => {
                    eprintln!("❌ Scanfailed {}: {}", source.root_path, e);
                }
            }
        }

        Ok(all_repos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_git_dirs() {
        // Test directory found logic
    }
}
