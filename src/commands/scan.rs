//! Scan command handling

use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::OutputFormat;
use crate::commands::ensure_initialized;
// Database returned by ensure_initialized
use crate::fetcher::Fetcher;
use crate::models::RepoSummary;
use crate::reporter::{
    html::HtmlReporter, markdown::MarkdownReporter, save_report, terminal::TerminalReporter,
    terminal::print_scan_summary, Reporter,
};
use crate::scanner::Scanner;

/// Execute scan command
pub async fn execute(
    should_fetch: bool,
    format: OutputFormat,
    out_path: Option<PathBuf>,
    _depth: Option<usize>,
    jobs: usize,
    no_security_check: bool,
) -> Result<()> {
    let start = Instant::now();

    let (config, db) = ensure_initialized()?;

    println!("{} Starting scan...", "▶".cyan());

    // Get scan sources from config
    let sources = config.scan_sources.clone();
    if sources.is_empty() {
        anyhow::bail!("No enabled scan sources");
    }

    // Scan repositories
    let repos = Scanner::scan_all(&sources, &db, true).await?;

    if repos.is_empty() {
        println!("{} No Git repositories found", "!".yellow());
        return Ok(());
    }

    let _scan_duration = start.elapsed().as_millis();

    // Optional: fetch all repositories first
    let repos = if should_fetch {
        println!("\n{} Starting fetch all repositories...", "▶".cyan());
        let fetcher = Fetcher::new(jobs, 30).with_security_scan(!no_security_check);
        fetcher.fetch_and_rescan(&repos, &db, true).await?
    } else {
        repos
    };

    // Calculate summary
    let mut summary = RepoSummary::new();
    for repo in &repos {
        summary.add(repo);
    }

    // Generate report
    match format {
        OutputFormat::Terminal => {
            let reporter = TerminalReporter::new();
            let report_content = reporter.generate(&repos, &summary)?;
            println!("{}", report_content);
        }
        OutputFormat::Html => {
            let reporter = HtmlReporter::new();
            let report_content = reporter.generate(&repos, &summary)?;
            let extension = reporter.extension();
            let path = save_report(&report_content, out_path, extension)?;
            println!("{} HTML report saved: {}", "✓".green(), path.display());
        }
        OutputFormat::Markdown => {
            let reporter = MarkdownReporter::new();
            let report_content = reporter.generate(&repos, &summary)?;
            let extension = reporter.extension();
            let path = save_report(&report_content, out_path, extension)?;
            println!("{} Markdown report saved: {}", "✓".green(), path.display());
        }
    }

    let total_duration = start.elapsed().as_millis();

    // Print summary
    print_scan_summary(&repos, &summary, total_duration);

    Ok(())
}
