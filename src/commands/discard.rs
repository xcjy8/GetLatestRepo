//! Discard command - discard local changes
//!
//! Allows users to discard all local changes in a specified repository, then continue fetching or pulling

use anyhow::Result;
use colored::Colorize;
use std::io::Write;

use crate::db::Database;
use crate::git::GitOps;

/// Execute discard command
pub async fn execute(repo_path: Option<String>, yes: bool) -> Result<()> {
    let db = Database::open()?;
    
    // Determine target repository
    let target_path = match repo_path {
        Some(path) => path,
        None => {
            // If no path specified, show all repositories with local changes for user selection
            let repos = db.list_repositories()?;
            let dirty_repos: Vec<_> = repos.into_iter()
                .filter(|r| r.dirty)
                .collect();
            
            if dirty_repos.is_empty() {
                println!("{} No repositories with local changes found", "ℹ".blue());
                return Ok(());
            }
            
            println!("{} Found {} repositories with local changes:", "📋".cyan(), dirty_repos.len());
            println!();
            
            for (i, repo) in dirty_repos.iter().enumerate() {
                let branch_info = repo.branch.as_deref().unwrap_or("unknown");
                println!("  [{}] {} [{}] ({} files)", 
                    i + 1,
                    repo.name.bold(),
                    branch_info.dimmed(),
                    repo.dirty_files.len()
                );
                // Show first few changed files
                for file in repo.dirty_files.iter().take(3) {
                    println!("      - {}", file.dimmed());
                }
                if repo.dirty_files.len() > 3 {
                    println!("      ... and {} files", repo.dirty_files.len() - 3);
                }
                println!();
            }
            
            print!("Please select the repository number to discard changes (1-{}), or enter 0 to cancel: ", dirty_repos.len());
            std::io::stdout().flush()?;
            
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            
            let choice: usize = input.trim().parse()
                .map_err(|_| anyhow::anyhow!("Invalid input"))?;
            
            if choice == 0 || choice > dirty_repos.len() {
                println!("{} Cancelled", "✓".green());
                return Ok(());
            }
            
            dirty_repos[choice - 1].path.clone()
        }
    };
    
    // Validate path
    let path = std::path::PathBuf::from(&target_path);
    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", target_path);
    }
    
    // Get repository info for display
    let repo_name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| target_path.clone());
    
    // Confirmation prompt
    if !yes {
        println!();
        println!("{} Warning: this operation will permanently discard all local changes!", "⚠️".red().bold());
        println!();
        println!("  Repository: {}", repo_name.bold());
        println!("  path: {}", target_path.dimmed());
        println!();
        println!("  Content to be discarded includes:");
        println!("    - All working directory changes");
        println!("    - All staged changes");
        println!("    - Untracked files");
        println!();
        print!("{} Confirm to discard these changes? [y/N] ", "❓".yellow());
        std::io::stdout().flush()?;
        
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{} Cancelled", "✓".green());
            return Ok(());
        }
    }
    
    // Execute discard operation
    println!();
    println!("{} Discarding {} local changes...", "🗑️".yellow(), repo_name);
    
    match GitOps::discard_changes(&path, true) {
        Ok(discarded_files) => {
            println!("{} successfully discarded {} files' changes", "✓".green(), discarded_files.len());
            
            if !discarded_files.is_empty() {
                println!();
                println!("{} Discarded files:", "📄".dimmed());
                for (i, file) in discarded_files.iter().take(10).enumerate() {
                    println!("  {} {}", "-".dimmed(), file.dimmed());
                    if i == 9 && discarded_files.len() > 10 {
                        println!("  ... and {} files", discarded_files.len() - 10);
                        break;
                    }
                }
            }
            
            // Update repository status in database
            if let Ok(Some(mut repo)) = db.get_repository(&target_path) {
                repo.dirty = false;
                repo.dirty_files.clear();
                repo.file_changes.clear();
                if let Err(e) = db.upsert_repository(&mut repo) {
                    eprintln!("{} Update database status failed: {}", "⚠️".yellow(), e);
                }
            }
            
            println!();
            println!("{} Now you can run 'getlatestrepo fetch' or 'getlatestrepo pull-safe' ", "💡".cyan());
        }
        Err(e) => {
            anyhow::bail!("{} Failed to discard changes: {}", "✗".red(), e);
        }
    }
    
    Ok(())
}
