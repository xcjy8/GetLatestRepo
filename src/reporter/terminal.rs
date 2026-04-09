use anyhow::Result;
use colored::*;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, ContentArrangement, Table};

use crate::git::format_duration;
use crate::models::{Freshness, RepoSummary, Repository};
use super::Reporter;

pub struct TerminalReporter;

impl TerminalReporter {
    pub fn new() -> Self {
        Self
    }
}

impl Reporter for TerminalReporter {
    fn generate(&self, repos: &[Repository], summary: &RepoSummary) -> Result<String> {
        let mut output = String::new();

        // Title
        output.push_str(&format!("\n{}", "═".repeat(70).cyan()));
        output.push_str(&format!("\n  {}\n", "📦 GetLatestRepo Scan Report".bold()));
        output.push_str(&format!("{}\n\n", "═".repeat(70).cyan()));

        // Summary
        output.push_str(&format!("{}", "📊 Summary\n".bold().underline()));
        output.push_str(&format!("   Total repositories: {}\n", summary.total.to_string().cyan()));
        output.push_str(&format!("   {} Need updates  |  {} Synced  |  {} dirty  |  {} errors\n",
            format!("{}", summary.has_updates).red().bold(),
            format!("{}", summary.synced).green(),
            format!("{}", summary.dirty).yellow(),
            format!("{}", summary.unreachable).dimmed()
        ));
        output.push('\n');

        // Table
        if !repos.is_empty() {
            output.push_str(&format!("{}\n", "📋 Repository list\n".bold().underline()));

            let mut table = Table::new();
            table
                .set_content_arrangement(ContentArrangement::Dynamic)
                .apply_modifier(UTF8_ROUND_CORNERS)
                .set_header(vec![
                    "#".cell(),
                    "Repository".cell(),
                    "Branch".cell(),
                    "Status".cell(),
                    "Remote commits".cell(),
                    "Last update".cell(),
                ]);

            for (idx, repo) in repos.iter().enumerate() {
                let status = format!("{} {}", 
                    repo.freshness.emoji(),
                    if repo.dirty { "+dirty" } else { "" }
                );

                let commits = if repo.behind_count > 0 {
                    format!("{} behind", repo.behind_count).red().to_string()
                } else if repo.ahead_count > 0 {
                    format!("{} ahead", repo.ahead_count).yellow().to_string()
                } else {
                    "synced".green().to_string()
                };

                let last_update = format_duration(&repo.last_commit_at);

                table.add_row(vec![
                    (idx + 1).to_string().dimmed().to_string(),
                    repo.name.clone(),
                    repo.branch.clone().unwrap_or_else(|| "-".to_string()).dimmed().to_string(),
                    status,
                    commits,
                    last_update.dimmed().to_string(),
                ]);
            }

            output.push_str(&table.to_string());
            output.push('\n');
        }

        // Legend
        output.push('\n');
        output.push_str(&format!("{}", "Legend:\n".dimmed()));
        output.push_str(&format!("  {} Need updates  {} Synced  {} Unreachable  {} No remote  📝 have local changes\n",
            "🔴".red(), "🟢".green(), "⚫".dimmed(), "⚪".white()
        ));

        Ok(output)
    }

    fn extension(&self) -> &'static str {
        "txt"
    }
}

/// Print a concise scan summary
pub fn print_scan_summary(repos: &[Repository], summary: &RepoSummary, duration_ms: u128) {
    println!("\n{}", "─".repeat(60).cyan());
    println!("  {} Found {} repositories ({}ms)", 
        "✓".green().bold(),
        repos.len().to_string().cyan().bold(),
        duration_ms
    );
    
    if summary.has_updates > 0 {
        println!("  {} repositories need updates", summary.has_updates.to_string().red().bold());
    }
    if summary.dirty > 0 {
        println!("  {} repositories have local changes", summary.dirty.to_string().yellow());
    }
    if summary.unreachable > 0 {
        println!("  {} repositories remote unreachable", summary.unreachable.to_string().dimmed());
    }
    
    println!("{}", "─".repeat(60).cyan());
}

/// Print a single repository's details
pub fn print_repo_detail(repo: &Repository) {
    println!("\n{}", "═".repeat(60).cyan());
    println!("  {} {}", "📁".cyan(), repo.name.bold());
    println!("{}", "═".repeat(60).cyan());
    
    println!("  path: {}", repo.path.dimmed());
    println!("  Branch: {}", repo.branch.as_deref().unwrap_or("-").cyan());
    
    // Status
    let status_text = match repo.freshness {
        Freshness::HasUpdates => format!("{} Need updates ({} behind)", "🔴".red(), repo.behind_count),
        Freshness::Synced => format!("{} Synced", "🟢".green()),
        Freshness::Unreachable => format!("{} Remote unreachable", "⚫".dimmed()),
        Freshness::NoRemote => format!("{} No remote branch", "⚪".white()),
    };
    println!("  Status: {}", status_text);
    
    if repo.dirty {
        println!("  Local: {} {} files uncommitted", "📝".yellow(), repo.dirty_files.len());
    }
    
    if let Some(ref url) = repo.upstream_url {
        let safe_url = crate::utils::sanitize_url(url);
        println!("  Remote: {}", safe_url.dimmed());
    }
    
    if let Some(ref msg) = repo.last_commit_message {
        println!("\n  Last commit:");
        println!("    {} {}", "├─".dimmed(), msg.split('\n').next().unwrap_or(msg));
        if let Some(ref author) = repo.last_commit_author {
            println!("    {} {} - {}", "└─".dimmed(), author.dimmed(), 
                format_duration(&repo.last_commit_at).dimmed());
        }
    }
    
    println!("{}", "═".repeat(60).cyan());
}

// Helper trait for comfy-table
trait CellExt {
    fn cell(self) -> comfy_table::Cell;
}

impl CellExt for &str {
    fn cell(self) -> comfy_table::Cell {
        comfy_table::Cell::new(self)
    }
}

impl CellExt for String {
    fn cell(self) -> comfy_table::Cell {
        comfy_table::Cell::new(self)
    }
}
