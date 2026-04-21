mod cli;
mod commands;
mod concurrent;
mod config;
mod db;
mod error;
mod fetcher;
mod git;
mod models;
#[cfg(test)]
mod network_test;
mod reporter;
mod scanner;
mod security;
mod signal_handler;
mod sync;
mod utils;
mod workflow;

use anyhow::{Result, Context};
use clap::Parser;
use crate::cli::{Cli, Commands};
use crate::git::ProxyConfig;
use std::fs::File;

/// Process lock file; automatically cleaned up on Drop
pub struct ProcessLock {
    #[cfg(unix)]
    _file: File,
    #[cfg(not(unix))]
    pid_path: std::path::PathBuf,
}

#[cfg(not(unix))]
impl Drop for ProcessLock {
    fn drop(&mut self) {
        // Windows: clean up PID file
        if let Err(e) = std::fs::remove_file(&self.pid_path) {
            eprintln!("Warning: unable to clean up PID file '{}': {}", self.pid_path.display(), e);
        }
    }
}

/// Acquire a process lock to prevent duplicate execution
fn acquire_process_lock() -> Result<ProcessLock> {
    let lock_path = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("getlatestrepo.lock");
    
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        use libc::{flock, LOCK_EX, LOCK_NB};
        
        let file = File::create(&lock_path)
            .with_context(|| format!("Unable to create lock file: {:?}", lock_path))?;
        
        let fd = file.as_raw_fd();
        let result = unsafe { flock(fd, LOCK_EX | LOCK_NB) };
        
        if result != 0 {
            anyhow::bail!("Another getlatestrepo instance is already running, cannot execute concurrently");
        }
        
        Ok(ProcessLock { _file: file })
    }
    
    #[cfg(not(unix))]
    {
        // Windows: use PID file + process existence check
        let pid_file = lock_path.with_extension("pid");
        
        // Check if lock file already exists
        if pid_file.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
                let pid_str = pid_str.trim();
                if !pid_str.is_empty() {
                    // Check if the process still exists
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if is_process_running(pid) {
                            anyhow::bail!("Another getlatestrepo instance is running (PID: {})", pid);
                        } else {
                            // Process no longer exists, removing old PID file
                            let _ = std::fs::remove_file(&pid_file);
                        }
                    }
                }
            }
        }
        
        // Create PID file
        let current_pid = std::process::id();
        std::fs::write(&pid_file, current_pid.to_string())
            .with_context(|| format!("Unable to write PID file: {:?}", pid_file))?;
        
        Ok(ProcessLock { pid_path: pid_file })
    }
}

#[cfg(not(unix))]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    
    // Use tasklist to check if process exists
    let output = Command::new("tasklist")
        .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
        .output();
    
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains(&pid.to_string())
        }
        Err(_) => {
            // If unable to check, assume process exists (conservative strategy)
            true
        }
    }
}

#[tokio::main]
async fn main() -> Result<std::process::ExitCode> {
    // Initialize colored output
    colored::control::set_override(true);

    // Initialize signal handling for Ctrl+C
    signal_handler::init();

    // Prevent duplicate execution via file lock
    let _lock = acquire_process_lock()?;

    let cli = Cli::parse();
    let no_security_check = cli.no_security_check;

    // Build proxy config
    let proxy_config = build_proxy_config(cli.proxy, cli.proxy_url);

    let exit_code = match cli.command {
        Commands::Init { path } => {
            commands::init::execute(path).await.map(|_| 0)
        }
        Commands::Scan {
            fetch,
            output,
            out,
            depth,
            jobs,
        } => {
            commands::scan::execute(fetch, output, out, depth, validate_jobs(jobs), no_security_check)
                .await
                .map(|_| 0)
        }
        Commands::Fetch { jobs, timeout } => {
            commands::fetch::execute(validate_jobs(jobs), timeout, no_security_check, proxy_config)
                .await
                .map(|_| 0)
        }
        Commands::Status { path, diff } => {
            commands::status::execute(path, diff).await.map(|_| 0)
        }
        Commands::Config { command } => {
            commands::config::execute(command).await.map(|_| 0)
        }
        Commands::Workflow {
            name,
            list,
            dry_run,
            silent,
            jobs,
            timeout,
            diff_after,
            yes,
            no_pull_guard,
        } => {
            commands::workflow::execute(
                name,
                list,
                dry_run,
                silent,
                jobs.map(validate_jobs),
                timeout,
                diff_after,
                yes,
                no_security_check,
                no_pull_guard,
                proxy_config,
            )
            .await
        }
        Commands::Discard { path, yes } => {
            commands::discard::execute(path, yes).await.map(|_| 0)
        }
    }?;

    Ok(std::process::ExitCode::from(exit_code as u8))
}

/// Build proxy configuration
/// Validate and limit concurrency
fn validate_jobs(jobs: usize) -> usize {
    const MAX_JOBS: usize = 100;
    const MIN_JOBS: usize = 1;
    
    if jobs > MAX_JOBS {
        eprintln!("Warning: concurrency {} exceeds maximum limit {}, adjusted to {}", jobs, MAX_JOBS, MAX_JOBS);
        MAX_JOBS
    } else if jobs < MIN_JOBS {
        eprintln!("Warning: concurrency {} is below minimum limit {}, adjusted to {}", jobs, MIN_JOBS, MIN_JOBS);
        MIN_JOBS
    } else {
        jobs
    }
}

fn build_proxy_config(proxy: bool, proxy_url: Option<String>) -> Option<ProxyConfig> {
    if proxy || proxy_url.is_some() {
        Some(ProxyConfig {
            enabled: true,
            http_proxy: proxy_url
                .clone()
                .unwrap_or_else(|| crate::utils::DEFAULT_PROXY_URL.to_string()),
            https_proxy: proxy_url
                .unwrap_or_else(|| crate::utils::DEFAULT_PROXY_URL.to_string()),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cli() {
        // CLI test
    }
}
