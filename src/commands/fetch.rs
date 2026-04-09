//! Fetch command handling

use anyhow::Result;
use colored::Colorize;

use crate::commands::ensure_initialized;
use crate::fetcher::Fetcher;
use crate::git::ProxyConfig;

/// Execute fetch command
pub async fn execute(
    jobs: usize,
    timeout: u64,
    no_security_check: bool,
    proxy_config: Option<ProxyConfig>,
) -> Result<()> {
    let (_config, db) = ensure_initialized()?;

    let repos = db.list_repositories()?;

    if repos.is_empty() {
        println!(
            "{} No repository records in database, please run: getlatestrepo scan",
            "!".yellow()
        );
        return Ok(());
    }

    println!(
        "{} Starting fetch of {} repositories (concurrency: {}, timeout: {}s)...",
        "▶".cyan(),
        repos.len(),
        jobs,
        timeout
    );

    let mut fetcher = Fetcher::new(jobs, timeout).with_security_scan(!no_security_check);

    if let Some(ref proxy) = proxy_config {
        if proxy.enabled {
            fetcher = fetcher.with_proxy(proxy.clone());
            println!("{} Using proxy: {}", "ℹ".blue(), proxy.http_proxy);
        } else {
            println!("{} Proxy configured but not enabled: {} (use --proxy to enable)", "ℹ".blue(), proxy.http_proxy);
        }
    }

    let summary = fetcher.fetch_and_update(&repos, &db, true).await?;

    summary.print_summary();

    Ok(())
}
