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

/// Print a centralized view of all repositories with issues
pub fn print_issues_view(repos: &[Repository]) {
    use std::path::Path;
    
    let mut needauth = Vec::new();
    let mut unreachable = Vec::new();
    let mut dirty_behind = Vec::new();
    let mut missing = Vec::new();
    
    for repo in repos {
        if repo.path.contains(crate::utils::NEEDAUTH_DIR) {
            needauth.push(repo);
            continue;
        }
        // NOTE: Blocking filesystem I/O. For a large number of repos on slow storage,
        // this loop could take a noticeable amount of time. Consider spawn_blocking if needed.
        if !Path::new(&repo.path).exists() {
            missing.push(repo);
            continue;
        }
        if repo.freshness == Freshness::Unreachable {
            unreachable.push(repo);
            continue;
        }
        if repo.dirty && repo.behind_count > 0 {
            dirty_behind.push(repo);
        }
    }
    
    let total_issues = needauth.len() + unreachable.len() + dirty_behind.len() + missing.len();
    
    println!("\n{}", "═".repeat(62).cyan());
    println!("  {} {}", "⚠️".yellow(), "异常仓库总览".bold());
    println!("{}", "═".repeat(62).cyan());
    println!("  共 {} 个异常仓库\n", total_issues.to_string().yellow().bold());
    
    if total_issues == 0 {
        println!("  {} 所有仓库状态正常\n", "✓".green());
        return;
    }
    
    let print_group = |icon: &str, title: &str, items: &[&Repository], detail_fn: &dyn Fn(&Repository) -> String| {
        if items.is_empty() { return; }
        println!("  {} {} ({}个)", icon, title.bold(), items.len());
        for (i, repo) in items.iter().enumerate() {
            let is_last = i == items.len() - 1;
            let corner = if is_last { "└─" } else { "├─" };
            let detail = detail_fn(repo);
            println!("     {} {} {}", corner, repo.name.cyan(), detail.dimmed());
        }
        println!();
    };
    
    print_group("🔒", "认证隔离", &needauth, &|repo| {
        format!("[{}]", repo.upstream_url.as_deref().map(crate::utils::sanitize_url).unwrap_or_else(|| "-".to_string()))
    });
    
    print_group("⚫", "不可达", &unreachable, &|repo| {
        format!("[上次 fetch: {}]", crate::git::format_duration(&repo.last_fetch_at))
    });
    
    print_group("📝", "本地修改待同步", &dirty_behind, &|repo| {
        format!("[behind {}, {} files changed]", repo.behind_count, repo.dirty_files.len())
    });
    
    print_group("❌", "路径失效", &missing, &|_repo| {
        "[路径不存在]".to_string()
    });
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
