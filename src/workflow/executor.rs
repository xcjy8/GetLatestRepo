use anyhow::Result;
use colored::*;
use std::time::Instant;

use crate::cli::OutputFormat;
use crate::config::AppConfig;
use crate::db::Database;
use crate::fetcher::{FetchSummary, Fetcher};
use crate::git::ProxyConfig;
use crate::models::{Freshness, RepoSummary};
use crate::scanner::Scanner;

use super::types::*;

/// Unified view for printing repository change trees (used by both Repository and DirtyRepoInfo)
trait RepoChangeView {
    fn name(&self) -> &str;
    fn path(&self) -> &str;
    fn branch(&self) -> Option<&str>;
    fn file_changes(&self) -> &[crate::models::FileChange];
    fn change_summary(&self) -> String;
}

impl RepoChangeView for crate::models::Repository {
    fn name(&self) -> &str { &self.name }
    fn path(&self) -> &str { &self.path }
    fn branch(&self) -> Option<&str> { self.branch.as_deref() }
    fn file_changes(&self) -> &[crate::models::FileChange] { &self.file_changes }
    fn change_summary(&self) -> String { self.change_summary() }
}

impl RepoChangeView for crate::workflow::types::DirtyRepoInfo {
    fn name(&self) -> &str { &self.name }
    fn path(&self) -> &str { &self.path }
    fn branch(&self) -> Option<&str> { self.branch.as_deref() }
    fn file_changes(&self) -> &[crate::models::FileChange] { &self.file_changes }
    fn change_summary(&self) -> String { self.change_summary() }
}

/// Convert a Repository into a DirtyRepoInfo
fn repo_to_dirty_info(r: crate::models::Repository) -> DirtyRepoInfo {
    DirtyRepoInfo::new(r.name, r.path, r.branch.clone(), r.file_changes.clone())
}

/// Print a single repository's change tree (shared between execute() and execute_pull_safe())
fn print_repo_change_tree(repo: &impl RepoChangeView, is_last: bool, base_indent: usize) {
    let pad = " ".repeat(base_indent);
    let repo_connector = if is_last { "└─" } else { "├─" };
    println!("{}{} 📦 {}", pad, repo_connector, repo.name().bold());

    let meta = if is_last { "      " } else { "   │  " };
    println!("{}📁 {}", meta, repo.path().dimmed());

    let branch_info = repo.branch().unwrap_or("unknown");
    println!("{}🌿 Branch: {} | Status: {}", meta, branch_info.cyan(), repo.change_summary().yellow());

    if !repo.file_changes().is_empty() {
        println!("{}📝 Changed files ({}):", meta, repo.file_changes().len());

        for (j, change) in repo.file_changes().iter().enumerate() {
            let is_last_file = j == repo.file_changes().len() - 1;
            let file_pad = if is_last { "       " } else { "   │   " };
            let file_tree = if is_last_file { "└─" } else { "├─" };

            let status_icon = match change.status.as_str() {
                "added" => "✚",
                "deleted" => "✗",
                "modified" => "✎",
                "renamed" => "➜",
                _ => "?",
            };

            println!("{}{} {} {} {}",
                file_pad, file_tree, status_icon, change.path,
                if change.staged { "(staged)".green() } else { "(unstaged)".dimmed() }
            );

            let detail = if is_last_file { "         " } else { "   │     " };
            println!("{}Impact: {}", detail, change.impact.dimmed());
            println!("{}If pull-force is executed: {}", detail, change.stash_effect.dimmed());

            if !is_last_file {
                println!("{}", file_pad);
            }
        }
    }
}

/// Workflow executor
pub struct WorkflowExecutor {
    workflow: Workflow,
    jobs: usize,
    timeout: u64,
    dry_run: bool,
    silent: bool,
    security_check: bool,
    pull_safety_check: bool,  // Pull safety check (prevent repo deletion)
    proxy: ProxyConfig,
}

impl WorkflowExecutor {
    pub fn new(
        workflow: Workflow,
        jobs: Option<usize>,
        timeout: Option<u64>,
        dry_run: bool,
        silent: bool,
    ) -> Self {
        Self {
            jobs: jobs.unwrap_or(workflow.default_jobs),
            timeout: timeout.unwrap_or(workflow.default_timeout),
            workflow,
            dry_run,
            silent,
            security_check: true,  // Enabled by default
            pull_safety_check: true,  // Enabled repo-deletion detection by default
            proxy: ProxyConfig::default(),
        }
    }

    /// Set whether to enable the security scan
    pub fn with_security_check(mut self, enable: bool) -> Self {
        self.security_check = enable;
        self
    }

    /// Set whether to enable the pull safety check (repo-deletion detection)
    pub fn with_pull_safety_check(mut self, enable: bool) -> Self {
        self.pull_safety_check = enable;
        self
    }

    /// Set proxy
    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = proxy;
        self
    }

    /// Execute the workflow
    pub async fn execute(&self) -> Result<WorkflowResult> {
        let start = Instant::now();

        if !self.silent {
            let title = format!("▶ workflow: {}", self.workflow.name);
            let desc = &self.workflow.description;
            println!("\n┌────────────────────────────────────────────────────────────┐");
            println!("│ {:<58} │", title.bold());
            println!("│ {:<58} │", desc.dimmed());
            println!("└────────────────────────────────────────────────────────────┘");
            println!();
        }

        if self.dry_run {
            self.print_dry_run();
            return Ok(WorkflowResult::success());
        }

        // Check initialization
        let config = AppConfig::load()?;
        if !config.is_initialized() {
            anyhow::bail!("Not initialized. Please run: getlatestrepo init <path>");
        }

        let db = Database::open()?;
        let sources = config.scan_sources;

        if sources.is_empty() {
            anyhow::bail!("No enabled scan sources");
        }

        let mut result = WorkflowResult::success();
        let total_steps = self.workflow.steps.len();

        for (idx, step) in self.workflow.steps.iter().enumerate() {
            // Check for graceful shutdown request before starting each step
            if crate::signal_handler::is_shutdown_requested() {
                if !self.silent {
                    println!("  {} Workflow interrupted by user, stopping early...", "⚠️".yellow());
                }
                result.add_error("Workflow interrupted by user".to_string());
                break;
            }

            let step_num = idx + 1;

            match step {
                WorkflowStep::Fetch { jobs, timeout } => {
                    let jobs = jobs.unwrap_or(self.jobs);
                    let timeout = timeout.unwrap_or(self.timeout);

                    if !self.silent {
                        println!("  [{}] Fetch all repositories", format!("{}/{}", step_num, total_steps).cyan());
                    }

                    match self.execute_fetch(&db, &sources, jobs, timeout).await {
                        Ok(summary) => {
                            if !self.silent {
                                // Proxy info
                                if self.proxy.enabled {
                                    println!("  ├─ {} {}", "ℹ".blue(), self.proxy.http_proxy.dimmed());
                                }

                                // Progress bar
                                println!("  ├─ ████████████████████████████████████████ {:>2}/{}",
                                    summary.total, summary.total);

                                // Result statistics
                                let success_str = format!("{}", summary.success).green();
                                let failed_str = if summary.failed > 0 {
                                    format!("{}", summary.failed).red()
                                } else {
                                    format!("{}", summary.failed).green()
                                };
                                println!("  ├─ {} Total: {} | succeeded: {} | failed: {}",
                                    "▶".blue(),
                                    summary.total,
                                    success_str,
                                    failed_str
                                );

                                // Failed details (tree view)
                                if summary.failed > 0 {
                                    println!("  │");
                                    println!("  └─ {} Failed details:", "⚠".yellow());
                                    let failed_repos: Vec<_> = summary.results.iter()
                                        .filter(|r| !r.success)
                                        .collect();
                                    for (i, repo) in failed_repos.iter().enumerate() {
                                        let is_last = i == failed_repos.len() - 1;
                                        let corner = if is_last { "└─" } else { "├─" };

                                        let error_msg = repo.error.as_deref().unwrap_or("Unknown error");
                                        let short_error = if error_msg.chars().count() > 42 {
                                            let truncated: String = error_msg.chars().take(42).collect();
                                            format!("{truncated}...")
                                        } else {
                                            error_msg.to_string()
                                        };
                                        let short_path = std::path::Path::new(&repo.repo_path)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or(&repo.repo_path);
                                        println!("     {} {} {}: {}",
                                            corner,
                                            short_path,
                                            "𐄂".dimmed(),
                                            short_error.dimmed()
                                        );
                                    }
                                }
                                println!();
                            }
                        }
                        Err(e) => {
                            if !self.silent {
                                println!("  └─ {} {}", "✗".red(), e);
                            }
                            result.add_error(format!("Fetch failed: {}", e));
                        }
                    }
                }

                WorkflowStep::Scan { output, open, only_dirty_or_behind } => {
                    if !self.silent {
                        let output_name = match output {
                            OutputFormat::Terminal => "Terminal",
                            OutputFormat::Html => "HTML",
                            OutputFormat::Markdown => "Markdown",
                        };
                        print!("[{}] Scan and generate {} report... ",
                            format!("{}/{}", step_num, total_steps).cyan(),
                            output_name
                        );
                    }

                    match self.execute_scan(&db, &sources, *output, *open, *only_dirty_or_behind).await {
                        Ok(summary) => {
                            if !self.silent {
                                println!("{} {} repos", "✓".green(), summary.total);
                            }

                            result.repo_summary = Some(summary);
                        }
                        Err(e) => {
                            if !self.silent {
                                println!("{} {}", "✗".red(), e);
                            }
                            result.add_error(format!("Scan failed: {}", e));
                        }
                    }
                }

                WorkflowStep::Check { condition, silent: check_silent } => {
                    if !self.silent && !check_silent {
                        print!("[{}] Check condition... ", format!("{}/{}", step_num, total_steps).cyan());
                    }

                    let check_result = self.execute_check(condition, &result);

                    match check_result {
                        Ok(()) => {
                            if !self.silent && !check_silent {
                                println!("{} Passed", "✓".green());
                            }
                        }
                        Err(msg) => {
                            if !self.silent && !check_silent {
                                println!("{} {}", "✗".red(), msg);
                            }
                            result.add_error(msg);
                            result.success = false;
                        }
                    }
                }

                WorkflowStep::PullSafe { jobs, confirm, diff_after } => {
                    let jobs = jobs.unwrap_or(self.jobs);

                    if !self.silent {
                        println!("  [{}] Security pull", format!("{}/{}", step_num, total_steps).cyan());
                    }

                    match self.execute_pull_safe(&db, &sources, jobs, *confirm && !self.dry_run, *diff_after).await {
                        Ok(pull_result) => {
                            if !self.silent {
                                if pull_result.total_count == 0 {
                                    println!("  └─ {} No repositories need updates", "ℹ".blue());
                                } else {
                                    let success_str = pull_result.success_count.to_string().green();
                                    let skip_count = pull_result.skipped_repos.len() + pull_result.dirty_repos.len();
                                    let skip_str = skip_count.to_string().dimmed();
                                    let failed_str = if pull_result.failed_count > 0 {
                                        pull_result.failed_count.to_string().red()
                                    } else {
                                        pull_result.failed_count.to_string().green()
                                    };
                                    println!("  └─ {} succeeded: {} | skipped: {} | failed: {}",
                                        "▶".blue(), success_str, skip_str, failed_str
                                    );

                                    // 展示成功拉取的仓库列表及最新提交时间
                                    if !pull_result.success_repos.is_empty() {
                                        println!("     {} Successfully pulled repos:", "✓".green());
                                        for (i, (name, time)) in pull_result.success_repos.iter().enumerate() {
                                            let is_last = i == pull_result.success_repos.len() - 1;
                                            let corner = if is_last { "└─" } else { "├─" };
                                            let time_str = time.as_deref().unwrap_or("(no time info)");
                                            println!("        {} {} {}",
                                                corner,
                                                name.green(),
                                                format!("- {}", time_str).dimmed()
                                            );
                                        }
                                        println!(); // 空行分隔
                                    }

                                    if !pull_result.dirty_repos.is_empty() {
                                        println!("     {} Repositories with local changes (manual handling needed):", "⚠️".yellow());
                                        println!();
                                        
                                        for (i, repo_info) in pull_result.dirty_repos.iter().enumerate() {
                                            let is_last = i == pull_result.dirty_repos.len() - 1;
                                            print_repo_change_tree(repo_info, is_last, 8);
                                            if !is_last {
                                                println!();
                                            }
                                        }
                                        
                                        // Add operation suggestions
                                        println!();
                                        println!("     💡 Suggestions:");
                                        println!("        ├─ Run 'pull-force' to auto stash → pull → pop");
                                        println!("        ├─ Run 'git restore .' to discard all local changes");
                                        println!("        └─ Or manually handle then run 'pull-safe'");
                                    }

                                    if *diff_after && !pull_result.pulled_repos.is_empty() {
                                        println!("     {} New commits after Pull:", "📋".cyan());
                                        for (name, commits) in &pull_result.pulled_repos {
                                            if !commits.is_empty() {
                                                println!("        {} {}:", "→".cyan(), name.bold());
                                                for commit in commits {
                                                    println!("           {}", commit);
                                                }
                                            }
                                        }
                                    }
                                }
                                println!();
                            }

                            if pull_result.failed_count > 0 {
                                result.success = false;
                            }
                        }
                        Err(e) => {
                            if !self.silent {
                                println!("  └─ {} {}", "✗".red(), e);
                            }
                            result.add_error(format!("Pull safe failed: {}", e));
                        }
                    }
                }

                WorkflowStep::PullForce { jobs, .. } => {
                    let jobs = jobs.unwrap_or(self.jobs);

                    if !self.silent {
                        print!("[{}] Force pull... ", format!("{}/{}", step_num, total_steps).cyan());
                    }

                    match self.execute_pull_force(&db, &sources, jobs).await {
                        Ok(pull_result) => {
                            if !self.silent {
                                println!("{} {}/{}", "✓".green(),
                                    pull_result.success_count,
                                    pull_result.total_count
                                );

                                if !pull_result.conflict_repos.is_empty() {
                                    println!("   {} repo(s) have stash pop conflicts, manual recovery needed:",
                                        pull_result.conflict_repos.len().to_string().yellow());
                                    for (i, info) in pull_result.conflict_repos.iter().enumerate() {
                                        let is_last = i == pull_result.conflict_repos.len() - 1;
                                        let repo_connector = if is_last { "└─" } else { "├─" };
                                        
                                        println!("     {} 📦 {}", repo_connector, info.name.bold());
                                        
                                        let stash_display = match info.stash_index {
                                            Some(idx) => format!("{} (stash@{{{}}})", info.stash_message, idx),
                                            None => info.stash_message.clone(),
                                        };
                                        println!("        ├─ stash: {}", stash_display);
                                        
                                        if !info.conflict_files.is_empty() {
                                            println!("        ├─ Conflict files ({}):", info.conflict_files.len());
                                            for (j, file) in info.conflict_files.iter().enumerate() {
                                                let is_last_file = j == info.conflict_files.len() - 1;
                                                let file_connector = if is_last_file { "└─" } else { "├─" };
                                                println!("        │  {} {}", file_connector, file);
                                            }
                                        }
                                        
                                        println!("        └─ Recovery command: git -C {} stash pop stash@{{index}}", info.path);
                                    }
                                }
                                if pull_result.failed_count > 0 {
                                    println!("   {} repositories failed",
                                        pull_result.failed_count.to_string().red());
                                }
                            }

                            if pull_result.has_errors() {
                                result.success = false;
                            }
                        }
                        Err(e) => {
                            if !self.silent {
                                println!("{} {}", "✗".red(), e);
                            }
                            result.add_error(format!("Pull force failed: {}", e));
                        }
                    }
                }
            }
        }

        let duration = start.elapsed();

        if !self.silent {
            println!();
            let status = if result.success {
                format!("{} Completed", "✓".green())
            } else {
                format!("{} Completed with errors", "⚠".yellow())
            };
            let time_info = format!("Duration {:.1}s", duration.as_secs_f32());
            println!("┌────────────────────────────────────────────────────────────┐");
            println!("│ {:<38} {:>17} │", status, time_info.dimmed());
            println!("└────────────────────────────────────────────────────────────┘");
        }

        Ok(result)
    }

    /// Execute the fetch step
    async fn execute_fetch(
        &self,
        db: &Database,
        sources: &[crate::models::ScanSource],
        jobs: usize,
        timeout: u64,
    ) -> Result<FetchSummary> {
        // Auto-sync: check for and scan new repositories
        let sync = crate::sync::RepoSync::new(true);
        let sync_status = sync.ensure_synced(sources, db, !self.silent).await?;
        
        if !self.silent && sync_status.needs_scan() {
            println!("  ├─ {}\n", sync_status.description());
        }

        let all_repos = db.list_repositories()?;
        let source_paths: std::collections::HashSet<_> = sources.iter()
            .map(|s| s.root_path.as_str())
            .collect();

        let mut repos: Vec<_> = all_repos.into_iter()
            .filter(|r| source_paths.contains(r.root_path.as_str()))
            .collect();

        if repos.is_empty() {
            let _ = Scanner::scan_all(sources, db, false, jobs).await?;

            let all_repos = db.list_repositories()?;
            repos = all_repos.into_iter()
                .filter(|r| source_paths.contains(r.root_path.as_str()))
                .collect();
        }
        if repos.is_empty() {
            anyhow::bail!("No repositories found");
        }

        let fetcher = Fetcher::new(jobs, timeout)
            .with_security_scan(self.security_check)
            .with_proxy(self.proxy.clone())
            .with_move_to_needauth(true)
            .with_auto_sync(false); // Already manually synced
        fetcher.fetch_and_update(&repos, db, !self.silent).await
    }

    /// Execute the scan step
    async fn execute_scan(
        &self,
        db: &Database,
        sources: &[crate::models::ScanSource],
        output: OutputFormat,
        open: bool,
        only_dirty_or_behind: bool,
    ) -> Result<RepoSummary> {
        use crate::reporter::{html::HtmlReporter, markdown::MarkdownReporter, terminal::TerminalReporter, Reporter, save_report_async};

        let repos = Scanner::scan_all(sources, db, false, crate::utils::DEFAULT_MAX_CONCURRENT_SCAN).await?;

        if repos.is_empty() {
            anyhow::bail!("No Git repositories found");
        }

        let filtered_repos: Vec<_> = if only_dirty_or_behind {
            repos.iter()
                .filter(|r| r.freshness == Freshness::HasUpdates || r.dirty)
                .cloned()
                .collect()
        } else {
            repos.clone()
        };

        let mut summary = RepoSummary::new();
        for repo in &repos {
            summary.add(repo);
        }

        match output {
            OutputFormat::Terminal => {
                let reporter = TerminalReporter::new();
                let report = reporter.generate(&filtered_repos, &summary)?;
                if !self.silent {
                    println!();
                    println!("{}", report);
                }
            }
            OutputFormat::Html => {
                let reporter = HtmlReporter::new();
                let report = reporter.generate(&repos, &summary)?;
                let path = save_report_async(report, None, "html".to_string()).await?;

                if let Err(e) = super::types::ensure_reports_dir(&path) {
                    eprintln!("   Warning: Failed to ensure reports directory: {}", e);
                }

                if !self.silent {
                    println!();
                    println!("{} HTML report: {}", "✓".green(), path.display());
                }

                if open {
                    super::types::open_report(&path)?;
                }
            }
            OutputFormat::Markdown => {
                let reporter = MarkdownReporter::new();
                let report = reporter.generate(&repos, &summary)?;
                let path = save_report_async(report, None, "md".to_string()).await?;
                if !self.silent {
                    println!();
                    println!("{} Markdown report: {}", "✓".green(), path.display());
                }
            }
        }

        Ok(summary)
    }

    /// Execute the check step
    fn execute_check(&self, condition: &Condition, result: &WorkflowResult) -> Result<(), String> {
        let summary = match &result.repo_summary {
            Some(s) => s,
            None => return Err("No scan result available for checking".to_string()),
        };

        match condition {
            Condition::HasBehind => {
                if summary.has_updates > 0 {
                    Err(format!("{} repositories behind remote", summary.has_updates))
                } else {
                    Ok(())
                }
            }
            Condition::HasDirty => {
                if summary.dirty > 0 {
                    Err(format!("{} repositories have local changes", summary.dirty))
                } else {
                    Ok(())
                }
            }
            Condition::HasError => {
                if summary.unreachable > 0 {
                    Err(format!("{} repositories remote unreachable", summary.unreachable))
                } else {
                    Ok(())
                }
            }
            Condition::AllSynced => {
                if summary.has_updates == 0 && summary.dirty == 0 && summary.unreachable == 0 {
                    Ok(())
                } else {
                    Err("Not all repositories synced".to_string())
                }
            }
        }
    }

    /// Execute safe pull (clean repositories only)
    #[allow(clippy::type_complexity)]
    async fn execute_pull_safe(
        &self,
        db: &Database,
        sources: &[crate::models::ScanSource],
        jobs: usize,
        confirm: bool,
        diff_after: bool,
    ) -> Result<PullSafeResult> {
        // Concurrency control uses standard library synchronization primitives

        let all_repos = db.list_repositories()?;
        let source_paths: std::collections::HashSet<_> = sources.iter()
            .map(|s| s.root_path.as_str())
            .collect();

        let repos: Vec<_> = all_repos.into_iter()
            .filter(|r| source_paths.contains(r.root_path.as_str()))
            .collect();
        if repos.is_empty() {
            anyhow::bail!("No repositories found");
        }

        let (behind_repos, up_to_date_repos): (Vec<_>, Vec<_>) = repos.into_iter()
            .partition(|r| r.freshness == Freshness::HasUpdates);

        if behind_repos.is_empty() {
            let mut result = PullSafeResult::new();
            result.skipped_repos = up_to_date_repos.into_iter().map(|r| r.name).collect();
            return Ok(result);
        }

        let mut clean_repos = Vec::new();
        let mut dirty_repos = Vec::new();

        for repo in behind_repos {
            if repo.dirty {
                dirty_repos.push(repo);
            } else {
                clean_repos.push(repo);
            }
        }

        if clean_repos.is_empty() {
            if !self.silent {
                println!();
                println!("{} All behind repositories have local changes, skipped", "⚠".yellow());
                println!();
                println!("{} Changed repository details:", "📋".cyan());
                println!();
                
                // Show tree hierarchy
                for (i, repo_info) in dirty_repos.iter().enumerate() {
                    let is_last = i == dirty_repos.len() - 1;
                    print_repo_change_tree(repo_info, is_last, 3);
                    if !is_last {
                        println!();
                    }
                }
                
                println!();
                println!("💡 Suggestions:");
                println!("   ├─ Run 'pull-force' to auto stash → pull → pop");
                println!("   ├─ Run 'git restore .' to discard all local changes");
                println!("   └─ Or manually handle then run 'pull-safe'");
            }
            let mut result = PullSafeResult::new();
            result.dirty_repos = dirty_repos.into_iter()
                .map(repo_to_dirty_info)
                .collect();
            return Ok(result);
        }

        // Pull safety check (prevents repo deletion)
        let mut unsafe_repos: Vec<(crate::models::Repository, crate::git::PullSafetyReport)> = Vec::new();

        if self.pull_safety_check {
            if !self.silent && !self.dry_run {
                println!("  ├─ {} Checking Pull safety...", "🔒".blue());
            }

            for repo in &clean_repos {
                if crate::signal_handler::is_shutdown_requested() {
                    anyhow::bail!("User interrupted, stopping pull operation");
                }

                let path = std::path::PathBuf::from(&repo.path);
                let repo = repo.clone();
                let result = match tokio::task::spawn_blocking(move || {
                    crate::git::GitOps::check_pull_safety(&path)
                }).await {
                    Ok(Ok(report)) => Ok(report),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(crate::error::GetLatestRepoError::Other(anyhow::anyhow!("Safety check task panicked"))),
                };
                match result {
                    Ok(report) => {
                        if !report.is_safe {
                            unsafe_repos.push((repo, report));
                        }
                    }
                    Err(e) => {
                        unsafe_repos.push((repo, crate::git::PullSafetyReport {
                            is_safe: false,
                            remote_commits: 0,
                            previous_remote_commits: 0,
                            change_ratio: 0.0,
                            warning: Some(format!("Safety check failed: {}", e)),
                            details: vec![],
                        }));
                    }
                }
            }

            if !unsafe_repos.is_empty() {
                let unsafe_names: std::collections::HashSet<_> = unsafe_repos
                    .iter()
                    .map(|(r, _)| r.name.clone())
                    .collect();
                clean_repos.retain(|r| !unsafe_names.contains(&r.name));

                if !self.silent {
                    println!("  │");
                    println!("  ├─ {} Skipping {} risky repositories:", "🚨".red(), unsafe_repos.len());
                    for (repo, report) in &unsafe_repos {
                        if let Some(ref warning) = report.warning {
                            println!("  │    ⚠ {}: {}", repo.name.red().bold(), warning);
                        } else {
                            println!("  │    ⚠ {}", repo.name.red().bold());
                        }
                    }

                    if clean_repos.is_empty() {
                        println!("  │");
                        println!("  └─ {}", "All behind repositories are risky or dirty, no safe pull possible".yellow());
                        let mut result = PullSafeResult::new();
                        result.dirty_repos = dirty_repos.into_iter()
                            .map(repo_to_dirty_info)
                            .collect();
                        return Ok(result);
                    }

                    println!("  │");
                    println!("  ├─ {} {} safe repositories will continue Pull", "✓".green(), clean_repos.len());
                } else if clean_repos.is_empty() {
                    let mut result = PullSafeResult::new();
                    result.dirty_repos = dirty_repos.into_iter()
                        .map(repo_to_dirty_info)
                        .collect();
                    return Ok(result);
                }
            }
        }

        // Dry-run preview
        if self.dry_run {
            if !self.silent {
                println!();
                println!("  ┌─ {} Dry-run preview ─────────────────────", "📋".cyan());

                if !dirty_repos.is_empty() {
                    println!("  │");
                    println!("  │ {} Repositories to skip (have local changes):", "○".dimmed());
                    println!("  │");
                    
                    for (i, repo) in dirty_repos.iter().enumerate() {
                        let is_last = i == dirty_repos.len() - 1;
                        let repo_connector = if is_last { "  │   └─" } else { "  │   ├─" };
                        
                        println!("{} 📦 {}", 
                            repo_connector,
                            repo.name.dimmed()
                        );
                        
                        let meta_connector = if is_last { "  │       " } else { "  │   │   " };
                        let branch_info = repo.branch.as_deref().unwrap_or("unknown");
                        println!("{}{} [{}] ({} files)", 
                            meta_connector,
                            "🌿".dimmed(),
                            branch_info.dimmed(),
                            repo.file_changes.len()
                        );
                        
                        // Show the first few changed files
                        for (j, change) in repo.file_changes.iter().take(2).enumerate() {
                            let is_last_file = is_last && j == repo.file_changes.len().min(2) - 1 && repo.file_changes.len() <= 2;
                            let file_connector = if is_last_file { "  │           └─" } else { "  │           ├─" };
                            
                            let status_icon = match change.status.as_str() {
                                "added" => "✚",
                                "deleted" => "✗",
                                "modified" => "✎",
                                "renamed" => "➜",
                                _ => "?",
                            };
                            
                            println!("{} {} {}", 
                                file_connector,
                                status_icon,
                                change.path.dimmed()
                            );
                        }
                        
                        if repo.file_changes.len() > 2 {
                            let more_connector = if is_last { "  │           └─" } else { "  │           ├─" };
                            println!("{} ... and {} files", 
                                more_connector,
                                repo.file_changes.len() - 2
                            );
                        }
                    }
                }

                if !unsafe_repos.is_empty() {
                    println!("  │");
                    println!("  │ {} Repositories to block (deletion risk detected):", "🚨".red());
                    for (repo, _) in &unsafe_repos {
                        println!("  │   • {}", repo.name.red());
                    }
                }

                if !clean_repos.is_empty() {
                    println!("  │");
                    println!("  │ {} Repositories to update (safe):", "▶".green());
                    for repo in &clean_repos {
                        println!("  │   • {} (behind {})",
                            repo.name.green(),
                            repo.behind_count.to_string().yellow()
                        );
                    }
                }

                println!("  │");
                println!("  └─ {} Preview complete, no actions were actually executed", "ℹ".blue());
            }

            let mut result = PullSafeResult::new();
            result.dirty_repos = dirty_repos.into_iter()
                .map(repo_to_dirty_info)
                .collect();
            result.skipped_repos = up_to_date_repos.into_iter().map(|r| r.name).collect();
            return Ok(result);
        }

        // Confirmation prompt
        if confirm && !self.silent && !clean_repos.is_empty() {
            println!();
            println!("{} Will update the following {} clean repositories:", "▶".cyan(), clean_repos.len());
            for repo in &clean_repos {
                println!("   - {} (behind {})", repo.name, repo.behind_count);
            }
            if !dirty_repos.is_empty() {
                println!();
                println!("{} The following {} repositories have local changes and will be skipped:", "!".yellow(), dirty_repos.len());
                println!();
                
                for (i, repo_info) in dirty_repos.iter().enumerate() {
                    let is_last = i == dirty_repos.len() - 1;
                    let repo_connector = if is_last { "└─" } else { "├─" };
                    
                    println!("   {} 📦 {}", 
                        repo_connector,
                        repo_info.name
                    );
                    
                    let meta_connector = if is_last { "      " } else { "   │  " };
                    let branch_info = repo_info.branch.as_deref().unwrap_or("unknown");
                    println!("{} {} [{}] ({} files)", 
                        meta_connector,
                        "🌿".dimmed(),
                        branch_info,
                        repo_info.file_changes.len()
                    );
                    
                    // Show the first 3 changed files
                    for (j, change) in repo_info.file_changes.iter().take(3).enumerate() {
                        let is_last_file = is_last && j == repo_info.file_changes.len().min(3) - 1 && repo_info.file_changes.len() <= 3;
                        let file_connector = if is_last_file { "       └─" } else { "       ├─" };
                        
                        let status_icon = match change.status.as_str() {
                            "added" => "✚",
                            "deleted" => "✗",
                            "modified" => "✎",
                            "renamed" => "➜",
                            _ => "?",
                        };
                        
                        println!("{}{} {} {}", 
                            file_connector,
                            status_icon,
                            change.path,
                            if change.staged { "(staged)".green() } else { "(unstaged)".dimmed() }
                        );
                    }
                    
                    if repo_info.file_changes.len() > 3 {
                        let more_connector = if is_last { "       └─" } else { "       ├─" };
                        println!("{} ... and {} files", more_connector, repo_info.file_changes.len() - 3);
                    }
                    
                    if !is_last {
                        println!();
                    }
                }
            }
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                anyhow::bail!("stdin is not a TTY, use --yes to skip confirmation");
            }

            print!("\nConfirm execution? [Y/n] ");
            use std::io::Write;
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().is_empty() && !input.trim().eq_ignore_ascii_case("y") {
                anyhow::bail!("User cancelled");
            }
        }

        // Concurrent pull using the unified concurrent executor
        // Features:
        // - Auto-handles panics
        // - Uses blocking wait (no busy-wait)
        // - Reasonable timeout
        use crate::concurrent::execute_concurrent_raw;
        
        // Build the task list
        let tasks: Vec<_> = clean_repos
            .into_iter()
            .map(|repo| {
                let path = std::path::PathBuf::from(&repo.path);
                let name = repo.name.clone();
                let repo_path = repo.path.clone();
                move || {
                    let result = crate::git::GitOps::pull_ff_only(&path);
                    (name, repo_path, result)
                }
            })
            .collect();

        // Execute concurrent tasks
        let results: Vec<Option<(String, String, Result<(), crate::error::GetLatestRepoError>)>> = execute_concurrent_raw(tasks, jobs);

        let mut pull_result = PullSafeResult::new();
        pull_result.dirty_repos = dirty_repos.into_iter()
            .map(repo_to_dirty_info)
            .collect();
        pull_result.skipped_repos = up_to_date_repos.into_iter().map(|r| r.name).collect();
        let mut success_paths: Vec<(String, String)> = Vec::new();

        // Process results (None means panicked)
        for result in results {
            pull_result.total_count += 1;
            
            match result {
                Some((name, path, Ok(()))) => {
                    pull_result.success_count += 1;
                    success_paths.push((name.clone(), path.clone()));

                    // Refresh the repository status and collect latest commit time
                    let mut latest_time = None;
                    if let Ok(Some(old_repo)) = db.get_repository(&path) {
                        let path_buf = std::path::PathBuf::from(&path);
                        let root_path = old_repo.root_path.clone();
                        if let Ok(Ok(mut fresh)) = tokio::task::spawn_blocking(move || {
                            crate::git::GitOps::inspect(&path_buf, &root_path)
                        }).await {
                            fresh.id = old_repo.id;
                            fresh.last_fetch_at = old_repo.last_fetch_at;
                            fresh.last_pull_at = Some(chrono::Local::now());
                            latest_time = fresh.last_commit_at;
                            if let Err(e) = db.upsert_repository(&mut fresh) {
                                eprintln!("   ⚠️ Update repository status failed '{}': {}", crate::utils::sanitize_path(&path), e);
                            } else {
                                // Only update pull time after upsert succeeds
                                if let Err(e) = db.update_pull_time(&path) {
                                    eprintln!("   ⚠️ Update pull time failed '{}': {}", crate::utils::sanitize_path(&path), e);
                                }
                            }
                        }
                    }
                    let time_str = latest_time.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string());
                    pull_result.success_repos.push((name, time_str));
                }
                Some((name, _, Err(e))) => {
                    pull_result.failed_count += 1;
                    if !self.silent {
                        eprintln!("   {} {} pull failed: {}", "✗".red(), name, e);
                    }
                }
                None => {
                    pull_result.failed_count += 1;
                    if !self.silent {
                        if crate::signal_handler::is_shutdown_requested() {
                            eprintln!("   {} pull task interrupted by signal", "⚠️".yellow());
                        } else {
                            eprintln!("   {} pull task panicked", "✗".red());
                        }
                    }
                }
            }
        }

        if diff_after && !success_paths.is_empty() {
            for (name, path) in success_paths {
                let path_buf = std::path::PathBuf::from(&path);
                match tokio::task::spawn_blocking(move || {
                    crate::git::GitOps::get_recent_commits(&path_buf, 10)
                }).await {
                    Ok(Ok(commits)) => {
                        pull_result.pulled_repos.push((name, commits));
                    }
                    _ => {
                        pull_result.pulled_repos.push((name, vec!["(Unable to get commit info)".to_string()]));
                    }
                }
            }
        }

        Ok(pull_result)
    }

    /// Execute force pull
    #[allow(clippy::type_complexity)]
    async fn execute_pull_force(
        &self,
        db: &Database,
        sources: &[crate::models::ScanSource],
        jobs: usize,
    ) -> Result<PullForceResult> {
        // Concurrency control uses standard library synchronization primitives

        let all_repos = db.list_repositories()?;
        let source_paths: std::collections::HashSet<_> = sources.iter()
            .map(|s| s.root_path.as_str())
            .collect();

        let repos: Vec<_> = all_repos.into_iter()
            .filter(|r| source_paths.contains(r.root_path.as_str()))
            .collect();
        if repos.is_empty() {
            anyhow::bail!("No repositories found");
        }

        let behind_repos: Vec<_> = repos.into_iter()
            .filter(|r| r.freshness == Freshness::HasUpdates)
            .collect();

        if behind_repos.is_empty() {
            return Ok(PullForceResult::new());
        }

        // Concurrent Pull (using unified concurrent executor)
        use crate::concurrent::execute_concurrent_raw;
        
        // Build the task list
        let tasks: Vec<_> = behind_repos
            .into_iter()
            .map(|repo| {
                let path = std::path::PathBuf::from(&repo.path);
                let name = repo.name.clone();
                let repo_path = repo.path.clone();
                move || {
                    let result = crate::git::GitOps::pull_force(&path);
                    (name, repo_path, result)
                }
            })
            .collect();

        // Execute concurrent tasks
        let results: Vec<Option<(String, String, Result<crate::git::PullForceOutcome, crate::error::GetLatestRepoError>)>> = execute_concurrent_raw(tasks, jobs);

        let mut pull_result = PullForceResult::new();
        let mut success_paths: Vec<(String, String)> = Vec::new();

        // Process results (None means panicked)
        for result in results {
            pull_result.total_count += 1;
            
            match result {
                Some((name, path, Ok(crate::git::PullForceOutcome::Success))) => {
                    pull_result.success_count += 1;
                    success_paths.push((name, path));
                }
                Some((name, path, Ok(crate::git::PullForceOutcome::Conflict { stash_name, conflict_files, stash_index }))) => {
                    pull_result.failed_count += 1;
                    pull_result.conflict_repos.push(crate::workflow::types::ConflictInfo {
                        name: name.clone(),
                        path: path.clone(),
                        stash_message: stash_name,
                        conflict_files,
                        stash_index,
                    });
                }
                Some((name, _, Err(e))) => {
                    pull_result.failed_count += 1;
                    eprintln!("   {} {} pull failed: {}", "✗".red(), name, e);
                }
                None => {
                    pull_result.failed_count += 1;
                    if crate::signal_handler::is_shutdown_requested() {
                        eprintln!("   {} pull task interrupted by signal", "⚠️".yellow());
                    } else {
                        eprintln!("   {} pull task panicked", "✗".red());
                    }
                }
            }
        }

        // Refresh the status of succeeded repositories
        for (_name, path) in success_paths {
            if let Ok(Some(old_repo)) = db.get_repository(&path) {
                let path_buf = std::path::PathBuf::from(&path);
                let root_path = old_repo.root_path.clone();
                if let Ok(Ok(mut fresh)) = tokio::task::spawn_blocking(move || {
                    crate::git::GitOps::inspect(&path_buf, &root_path)
                }).await {
                    fresh.id = old_repo.id;
                    fresh.last_fetch_at = old_repo.last_fetch_at;
                    fresh.last_pull_at = Some(chrono::Local::now());
                    if let Err(e) = db.upsert_repository(&mut fresh) {
                        eprintln!("   Warning: Failed to update repository after pull: {}", e);
                    } else {
                        // Only update pull time after upsert succeeds
                        if let Err(e) = db.update_pull_time(&path) {
                            eprintln!("   ⚠️ Update pull time failed '{}': {}", crate::utils::sanitize_path(&path), e);
                        }
                    }
                }
            }
        }

        Ok(pull_result)
    }

    /// Print dry-run plan
    fn print_dry_run(&self) {
        println!("{}", "[Dry Run] Execution plan:".yellow().bold());
        println!();

        for (idx, step) in self.workflow.steps.iter().enumerate() {
            let step_num = idx + 1;
            match step {
                WorkflowStep::Fetch { jobs, timeout } => {
                    let jobs = jobs.unwrap_or(self.jobs);
                    let timeout = timeout.unwrap_or(self.timeout);
                    println!("  [{}] Fetch", step_num);
                    println!("      Concurrency: {} | Timeout: {}s", jobs, timeout);
                }
                WorkflowStep::Scan { output, open, only_dirty_or_behind } => {
                    let output_name = match output {
                        OutputFormat::Terminal => "Terminal",
                        OutputFormat::Html => "HTML",
                        OutputFormat::Markdown => "Markdown",
                    };
                    println!("  [{}] Scan ({})", step_num, output_name);
                    println!("      Auto-open: {} | Show only attention-needed: {}", open, only_dirty_or_behind);
                }
                WorkflowStep::Check { condition, .. } => {
                    let cond_name = match condition {
                        Condition::HasBehind => "Has behind repositories",
                        Condition::HasDirty => "have local changes",
                        Condition::HasError => "Has errors",
                        Condition::AllSynced => "All synced",
                    };
                    println!("  [{}] Check ({})", step_num, cond_name);
                }
                WorkflowStep::PullSafe { jobs, confirm, diff_after } => {
                    let jobs = jobs.unwrap_or(self.jobs);
                    println!("  [{}] PullSafe", step_num);
                    println!("      Strategy: only pull clean repositories (ff-only)");
                    println!("      Dirty repos: skip and prompt");
                    println!("      Confirmation prompt: {}", if *confirm { "Yes" } else { "No" });
                    println!("      Show diff: {}", if *diff_after { "Yes" } else { "No" });
                    println!("      Concurrency: {}", jobs);
                }
                WorkflowStep::PullForce { jobs, diff_after } => {
                    let jobs = jobs.unwrap_or(self.jobs);
                    println!("  [{}] PullForce", step_num);
                    println!("      Flow: stash → pull --ff-only → stash pop");
                    println!("      Show diff: {}", if *diff_after { "Yes" } else { "No" });
                    println!("      Concurrency: {}", jobs);
                    println!("      Conflict handling: stop and prompt manual resolution");
                }
            }
            println!();
        }

        println!("{}", "Parameter overrides:".dimmed());
        println!("  Concurrency: {} (default: {})", self.jobs, self.workflow.default_jobs);
        println!("  Timeout: {}s (default: {}s)", self.timeout, self.workflow.default_timeout);
    }
}
