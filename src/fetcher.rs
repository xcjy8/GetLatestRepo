use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::db::Database;
use crate::git::{FetchStatus, GitOps, ProxyConfig};
use crate::models::{FetchResult as FetchResultModel, Repository};
use crate::security::{SecurityScanner, format_security_report};
use colored::Colorize;

/// Fetch execution results (includes path change info)
#[derive(Debug, Clone)]
pub struct FetchExecutionResult {
    /// Original repo info (state before fetch)
    pub original_repo: Repository,
    /// Current repository info (after fetch, path updated if moved)
    pub current_repo: Repository,
    /// Whether fetch succeeded
    pub success: bool,
    /// Error message
    pub error: Option<String>,
    /// Execution duration (milliseconds)
    pub duration_ms: u64,
    /// Whether moved to needauth
    pub moved_to_needauth: bool,
}

impl FetchExecutionResult {
    /// Get path for database operations (new path after move)
    pub fn db_path(&self) -> &str {
        &self.current_repo.path
    }
    
    /// Get original path
    /// 
    /// Currently unused, reserved for future debugging and audit functionality
    #[allow(dead_code)]
    pub fn original_path(&self) -> &str {
        &self.original_repo.path
    }
    
    /// Convert to FetchResultModel (for reporting)
    pub fn to_model(&self) -> FetchResultModel {
        FetchResultModel {
            repo_path: self.current_repo.path.clone(),
            success: self.success,
            error: self.error.clone(),
            duration_ms: self.duration_ms,
        }
    }
}

/// Concurrent fetch manager
pub struct Fetcher {
    concurrency: usize,
    timeout_secs: u64,
    security_scan: bool,
    auto_skip_high_risk: bool,
    proxy: ProxyConfig,
    move_to_needauth: bool,
    auto_sync: bool,
}

impl Fetcher {
    pub fn new(concurrency: usize, timeout_secs: u64) -> Self {
        Self {
            concurrency,
            timeout_secs,
            security_scan: true,
            auto_skip_high_risk: false,
            proxy: ProxyConfig::default(),
            move_to_needauth: true,
            auto_sync: true,
        }
    }

    pub fn with_security_scan(mut self, enable: bool) -> Self {
        self.security_scan = enable;
        self
    }

    /// Set whether to auto-skip high-risk repositories (no interactive confirmation)
    /// 
    /// Currently unused, reserved for future non-interactive mode
    #[allow(dead_code)]
    pub fn with_auto_skip_high_risk(mut self, skip: bool) -> Self {
        self.auto_skip_high_risk = skip;
        self
    }

    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = proxy;
        self
    }

    pub fn with_move_to_needauth(mut self, enable: bool) -> Self {
        self.move_to_needauth = enable;
        self
    }

    /// Set whether to auto-sync before fetch (scan new repositories)
    pub fn with_auto_sync(mut self, enable: bool) -> Self {
        self.auto_sync = enable;
        self
    }

    /// Batch fetch all repositories (with security scan and needauth handling)
    ///
    /// Return detailed execution result for each repository, including path change info.
    /// Does not directly operate on database — caller is responsible for persistence.
    pub async fn fetch_all_detailed(&self, repos: &[Repository], progress: bool) -> Vec<FetchExecutionResult> {
        let semaphore = Arc::new(Semaphore::new(self.concurrency));
        let multi_progress = if progress {
            Some(MultiProgress::new())
        } else {
            None
        };

        let main_pb = if let Some(ref mp) = multi_progress {
            let pb = mp.add(ProgressBar::new(repos.len() as u64));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        let mut futures = FuturesUnordered::new();

        for repo in repos {
            let permit = Arc::clone(&semaphore);
            let repo = repo.clone();
            let timeout_secs = self.timeout_secs;
            let main_pb = main_pb.clone();
            let proxy = self.proxy.clone();
            let security_scan = self.security_scan;
            let auto_skip = self.auto_skip_high_risk;
            let move_to_needauth = self.move_to_needauth;

            let future = tokio::spawn(async move {
                let _permit = permit.acquire().await.expect("Semaphore should not be closed");
                let start = Instant::now();
                let original_repo = repo.clone();
                let original_path = repo.path.clone();
                
                if let Some(ref pb) = main_pb {
                    pb.set_message(format!("{}", repo.name));
                }

                // 1. Security scan (only if the path exists)
                let repo_path_for_scan = original_path.clone();
                if security_scan {
                    let path = std::path::Path::new(&repo_path_for_scan);
                    
                    // Check if the path exists (handle cases moved to needauth but directory deleted)
                    if !path.exists() {
                        let path_str = original_path.clone();
                        return (original_repo, repo, FetchResultModel {
                            repo_path: path_str.clone(),
                            success: false,
                            error: Some(format!("Repository path does not exist: {}", path_str)),
                            duration_ms: start.elapsed().as_millis() as u64,
                        }, None);
                    }
                    
                    match Self::scan_repository(path, &repo.name) {
                        Ok((is_safe, report)) => {
                            if !is_safe {
                                let error_msg = if auto_skip {
                                    "Security scan failed, skipped".to_string()
                                } else {
                                    "User cancelled (security reason)".to_string()
                                };
                                
                                if !auto_skip {
                                    if !report.is_empty() {
                                        eprintln!("\n{}", report);
                                    }
                                    eprint!("Continue fetch '{}'? [y/N] ", repo.name);
                                    use std::io::Write;
                                    let _ = std::io::stdout().flush();
                                    let mut input = String::new();
                                    if std::io::stdin().read_line(&mut input).is_ok() {
                                        if input.trim().eq_ignore_ascii_case("y") {
                                            // User confirmed to continue
                                        } else {
                                            return (original_repo, repo, FetchResultModel {
                                                repo_path: original_path,
                                                success: false,
                                                error: Some(error_msg),
                                                duration_ms: start.elapsed().as_millis() as u64,
                                            }, None);
                                        }
                                    } else {
                                        return (original_repo, repo, FetchResultModel {
                                            repo_path: original_path,
                                            success: false,
                                            error: Some(error_msg),
                                            duration_ms: start.elapsed().as_millis() as u64,
                                        }, None);
                                    }
                                } else {
                                    return (original_repo, repo, FetchResultModel {
                                        repo_path: original_path,
                                        success: false,
                                        error: Some(error_msg),
                                        duration_ms: start.elapsed().as_millis() as u64,
                                    }, None);
                                }
                            }
                        }
                        Err(e) => {
                            // Security scan failures are only logged; they don't interrupt the flow
                            eprintln!("\n   {} Security scan failed '{}': {}", "⚠️".yellow(), repo.name, e);
                        }
                    }
                }

                // 2. Execute the fetch
                let path = std::path::PathBuf::from(&original_path);
                let repo_path = original_path.clone();
                let repo_name = repo.name.clone();
                let root_path = repo.root_path.clone();
                
                // Execute fetch in a blocking thread to avoid blocking the async runtime
                let fetch_status = match timeout(
                    Duration::from_secs(timeout_secs),
                    tokio::task::spawn_blocking(move || {
                        // Each thread independently configures the proxy (thread-local effective)
                        let git_ops = GitOps::with_proxy(proxy);
                        git_ops.fetch_detailed(&path, timeout_secs)
                    })
                ).await {
                    Ok(Ok(status)) => status,
                    Ok(Err(_)) => FetchStatus::OtherError { message: "Task was cancelled".to_string() },
                    Err(_) => FetchStatus::NetworkError { message: format!("Timeout ({}s)", timeout_secs) },
                };

                // 3. Handle repositories that need to be moved to need-auth (delayed print for unified display)
                let (current_repo, result, moved_repo_name) = if fetch_status.should_move_to_needauth() && move_to_needauth {
                    let needauth_dir = std::path::PathBuf::from(&root_path).join("needauth");
                    let needauth_path = needauth_dir.join(&repo_name);
                    
                    match Self::move_repo_to_needauth(&repo_path, &needauth_path, repo.upstream_url.as_deref()) {
                        Ok(final_path) => {
                            // Build the new repository path (possibly renamed)
                            let new_repo_path = final_path.to_string_lossy().to_string();
                            let new_root_path = needauth_dir.to_string_lossy().to_string();
                            let final_name = final_path.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| repo_name.clone());
                            
                            // Use an efficient path update method; also update the name if renamed
                            let mut new_repo = repo.with_new_path(new_repo_path, new_root_path);
                            let name_for_result = final_name.clone();
                            new_repo.name = final_name;
                            
                            let result = FetchResultModel {
                                repo_path: original_path,
                                success: false,
                                error: fetch_status.error_message(),
                                duration_ms: start.elapsed().as_millis() as u64,
                            };
                            (new_repo, result, Some(name_for_result))
                        }
                        Err(e) => {
                            // Record error when move fails
                            let result = FetchResultModel {
                                repo_path: original_path,
                                success: false,
                                error: Some(format!("{} (Move failed: {})", 
                                    fetch_status.error_message().unwrap_or_default(), e)),
                                duration_ms: start.elapsed().as_millis() as u64,
                            };
                            (repo, result, None)
                        }
                    }
                } else {
                    let result = FetchResultModel {
                        repo_path: original_path,
                        success: matches!(fetch_status, FetchStatus::Success),
                        error: fetch_status.error_message(),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                    (repo, result, None)
                };

                if let Some(ref pb) = main_pb {
                    pb.inc(1);
                }

                (original_repo, current_repo, result, moved_repo_name)
            });

            futures.push(future);
        }

        let mut results = Vec::new();
        let mut moved_repos: Vec<String> = Vec::new();

        while let Some(join_result) = futures.next().await {
            match join_result {
                Ok((original_repo, current_repo, result_model, moved_name)) => {
                    let moved = moved_name.is_some();

                    let exec_result = FetchExecutionResult {
                        original_repo,
                        current_repo,
                        success: result_model.success,
                        error: result_model.error,
                        duration_ms: result_model.duration_ms,
                        moved_to_needauth: moved,
                    };

                    // Collect moved repositories
                    if let Some(name) = moved_name {
                        moved_repos.push(name);
                    }

                    results.push(exec_result);
                }
                Err(e) => eprintln!("  │   {} Task exception: {}", "⚠️".yellow(), e),
            }
        }

        // After the progress bar completes, display move info uniformly
        if let Some(pb) = main_pb {
            pb.finish_and_clear();
        }
        
        if !moved_repos.is_empty() {
            println!("  ├─ {} The following repositories were moved to needauth/:", "📁".yellow());
            for (i, name) in moved_repos.iter().enumerate() {
                let is_last = i == moved_repos.len() - 1;
                let corner = if is_last { "└─" } else { "├─" };
                println!("  │   {} {}", corner, name.dimmed());
            }
        }

        results
    }
    
    /// Batch fetch all repositories (legacy interface returning simplified results)
    ///
    /// Currently unused, reserved for future scenarios needing simplified results
    #[allow(dead_code)]
    pub async fn fetch_all(&self, repos: &[Repository], progress: bool) -> Vec<FetchResultModel> {
        self.fetch_all_detailed(repos, progress).await
            .into_iter()
            .map(|r| r.to_model())
            .collect()
    }

    /// Move repository to needauth directory
    ///
    /// Move from normal scan directory to `<root_path>/needauth/<repo_name>/`.
    ///
    /// # Same-name repository handling
    /// If a same-name repository already exists in needauth, compare upstream_url:
    /// - Same: considered same repository, overwrite old one
    /// - Different: different author's repository, rename with numeric suffix (e.g. `repo-2`, `repo-3`)
    ///
    /// # Atomicity guarantee
    /// Use two-phase strategy of "rename target to temp first, then rename source→target",
    /// avoiding data loss from being killed between `remove_dir_all` + `rename`.
    ///
    /// # Path equality protection
    /// Use `canonicalize` to compare source and target paths, preventing repos already in needauth
    /// from emptying themselves (self-reference problem).
    fn move_repo_to_needauth(
        from: &str, 
        to: &std::path::Path,
        upstream_url: Option<&str>,
    ) -> Result<std::path::PathBuf, anyhow::Error> {
        use std::fs;
        use std::path::Path;

        let from_path = Path::new(from);
        
        // ── Path traversal protection ────────────────────────────────────────────────
        // Ensure `to` path is within expected needauth directory, prevent ../../../etc attacks
        // 1. First verify target path doesn't contain path traversal components
        let to_str = to.to_string_lossy();
        if to_str.contains("..") || to_str.contains("//") {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' contains illegal components",
                to.display()
            ));
        }
        
        // 2. Ensure parent directory exists and is resolvable (create before move)
        let parent = to.parent()
            .ok_or_else(|| anyhow::anyhow!("Target path has no parent directory: {}", to.display()))?;
        
        // 3. Create parent directory (if it doesn't exist)
        fs::create_dir_all(parent)
            .with_context(|| format!("Unable to create target parent directory: {}", parent.display()))?;
        
        // 4. canonicalize parent directory to get absolute path
        let parent_canonical = parent.canonicalize()
            .with_context(|| format!("Unable to resolve parent directory: {}", parent.display()))?;
        
        // 5. Build canonicalized target path (using parent canonical path + target file name)
        let file_name = to.file_name()
            .ok_or_else(|| anyhow::anyhow!("Unable to get target file name: {}", to.display()))?;
        let to_canonical = parent_canonical.join(file_name);
        
        // 6. Strict verification: target path must be a child of parent directory
        // Note: starts_with on Path is safe here because parent directory is canonicalized
        if !to_canonical.starts_with(&parent_canonical) {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' is not inside expected directory '{}'  inside",
                to_canonical.display(),
                parent_canonical.display()
            ));
        }

        // ── Path equality protection ──────────────────────────────────────────────
        // When repository is already in needauth, `root_path` is still the original scan root,
        // the calculated needauth_path happens to equal from.
        // Note: from_path may not exist (already moved or deleted), so use try_canonicalize
        match Self::try_canonicalize(from_path) {
            Some(from_canon) if from_canon == to_canonical => {
                return Ok(to.to_path_buf());
            }
            _ => {
                // from doesn't exist or differs from to, continue move process
            }
        }

        // ── Determine final target path ────────────────────────────────────────────
        // If target exists, check if it's the same repository
        let final_to = if to.exists() {
            // Try to read target repository's remote URL
            let target_url = Self::get_repo_remote_url(to);
            
            if target_url.is_some() && upstream_url.is_some() && target_url.as_deref() != upstream_url {
                // Same name but different author, needs renaming
                let renamed = Self::find_unique_repo_name(to)?;
                eprintln!("   ⚠️ A same-name but different-author repo already exists in needauth, renamed to: {}", 
                    renamed.file_name().unwrap_or_default().to_string_lossy());
                renamed
            } else {
                // Same repository or unable to compare, overwrite
                to.to_path_buf()
            }
        } else {
            to.to_path_buf()
        };

        // ── Ensure parent directory exists ──────────────────────────────────────────────
        // Note: if final_to is renamed to repo-2 etc., may need to create new parent directory
        if let Some(parent) = final_to.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Unable to create final parent directory: {}", parent.display()))?;
        }

        // ── Two-phase atomic move ──────────────────────────────────────────────
        // Phase 1: if target exists, move to temp directory first
        // Phase 2: move source to target
        // Phase 3: clean up temp directory
        // 
        // Note: if process crashes after phase 2, temp files will remain.
        // Files ending with .getlatestrepo_swap can be deleted via periodic cleanup scripts.
        let tmp_path;
        if final_to.exists() {
            tmp_path = Self::unique_temp_path(&final_to);
            if let Err(e) = fs::rename(&final_to, &tmp_path) {
                return Err(anyhow::anyhow!(
                    "Unable to move existing repository to temp location '{}': {}",
                    tmp_path.display(),
                    e
                ));
            }
        } else {
            tmp_path = PathBuf::new();
        }

        // Execute move
        if let Err(e) = fs::rename(from, &final_to) {
            // If move fails, try to restore original repository
            if !tmp_path.as_os_str().is_empty() {
                let _ = fs::rename(&tmp_path, &final_to);
            }
            return Err(anyhow::anyhow!(
                "Unable to move repository to '{}': {}",
                final_to.display(),
                e
            ));
        }

        // Clean up temp directory (failure is not an error, as this is best-effort cleanup)
        if !tmp_path.as_os_str().is_empty() {
            if let Err(e) = fs::remove_dir_all(&tmp_path) {
                eprintln!(
                    "Warning: unable to clean up temp directory '{}': {}. Please delete it manually.",
                    tmp_path.display(),
                    e
                );
            }
        }

        Ok(final_to)
    }

    /// Try to canonicalize path, return None if it doesn't exist
    fn try_canonicalize(path: &std::path::Path) -> Option<std::path::PathBuf> {
        path.canonicalize().ok()
    }

    /// Get repository's remote URL
    fn get_repo_remote_url(path: &std::path::Path) -> Option<String> {
        let repo = git2::Repository::open(path).ok()?;
        repo.find_remote("origin")
            .ok()
            .and_then(|r| r.url().map(|u| u.to_string()))
    }

    /// Generate unique name for same-name different-author repositories
    /// Format: repo-name-2, repo-name-3, ...
    fn find_unique_repo_name(base: &std::path::Path) -> Result<std::path::PathBuf, anyhow::Error> {
        let parent = base.parent()
            .ok_or_else(|| anyhow::anyhow!("Unable to get parent directory"))?;
        let name = base.file_name()
            .ok_or_else(|| anyhow::anyhow!("Unable to get repository name"))?
            .to_string_lossy();
        
        // Try repo-name-2, repo-name-3, ...
        for i in 2u32.. {
            let candidate = parent.join(format!("{}-{}", name, i));
            if !candidate.exists() {
                return Ok(candidate);
            }
        }
        
        // Theoretically unreachable
        Err(anyhow::anyhow!("Unable to find a unique repository"))
    }

    /// Generate a non-conflicting temp path
    fn unique_temp_path(target: &std::path::Path) -> std::path::PathBuf {
        let base = target.with_extension("getlatestrepo_swap");
        if !base.exists() {
            return base;
        }
        for i in 1u32.. {
            let candidate = target.with_extension(format!("getlatestrepo_swap.{}", i));
            if !candidate.exists() {
                return candidate;
            }
        }
        // Theoretically unreachable
        target.with_extension(format!("getlatestrepo_swap.{}", std::process::id()))
    }

    /// Scan single repository's security
    fn scan_repository(path: &std::path::Path, _name: &str) -> Result<(bool, String), anyhow::Error> {
        let repo = git2::Repository::open(path)?;
        let local_oid = repo.head().ok().and_then(|h| h.target());
        let remote_oid = Self::get_remote_oid(&repo)?;
        
        if local_oid.is_none() || remote_oid.is_none() {
            return Ok((true, String::new()));
        }

        let result = SecurityScanner::scan_before_fetch(path, local_oid, remote_oid)?;
        let report = format_security_report(&result);
        Ok((result.is_safe, report))
    }

    /// Get remote branch OID
    fn get_remote_oid(repo: &git2::Repository) -> Result<Option<git2::Oid>, anyhow::Error> {
        let branch_names = ["origin/HEAD", "origin/main", "origin/master", "origin/develop"];
        
        for branch_name in &branch_names {
            if let Ok(reference) = repo.find_reference(&format!("refs/remotes/{}", branch_name)) {
                return Ok(reference.target());
            }
        }
        
        Ok(None)
    }

    /// Fetch and update database
    ///
    /// Correctly handle path updates after repository move:
    /// - If repository moved to needauth, update path, root_path, depth in database
    /// - Only update fetch time for successful fetches
    /// - Optional: auto-sync before fetch (scan new repositories)
    pub async fn fetch_and_update(
        &self,
        repos: &[Repository],
        db: &Database,
        progress: bool,
    ) -> Result<FetchSummary> {
        let exec_results = self.fetch_all_detailed(repos, progress).await;

        let mut summary = FetchSummary::new();

        for exec_result in &exec_results {
            if exec_result.success {
                summary.success += 1;
                if let Err(e) = db.update_fetch_time(exec_result.db_path()) {
                    eprintln!("Update fetch timefailed '{}': {}", crate::utils::sanitize_path(exec_result.db_path()), e);
                }
            } else {
                summary.failed += 1;
            }
            summary.total += 1;

            // If the repository was moved to need-auth, atomically update the database
            if exec_result.moved_to_needauth {
                let mut moved_repo = exec_result.current_repo.clone();
                if let Err(e) = db.move_repository(&exec_result.original_repo.path, &mut moved_repo) {
                    eprintln!("Move repository record failed '{}': {}", crate::utils::sanitize_path(exec_result.db_path()), e);
                }
            }
        }

        summary.results = exec_results.into_iter().map(|r| r.to_model()).collect();
        Ok(summary)
    }

    /// Rescan state after fetching
    /// 
    /// Correctly handles repository moves:
    /// - If repository moved to needauth, rescan with new path
    /// - Preserve metadata like fetch time
    pub async fn fetch_and_rescan(
        &self,
        repos: &[Repository],
        db: &Database,
        progress: bool,
    ) -> Result<Vec<Repository>> {
        // Get detailed execution results (include path change information)
        let exec_results = self.fetch_all_detailed(repos, progress).await;

        let mut updated_repos = Vec::new();
        
        // Build a mapping from original path to execution results for fast lookup
        let result_map: std::collections::HashMap<String, &FetchExecutionResult> = exec_results
            .iter()
            .map(|r| (r.original_repo.path.clone(), r))
            .collect();
        
        for repo in repos {
            // Find the corresponding execution results
            let exec_result = result_map.get(&repo.path);
            
            // Rescan using the current path (which may be a new path)
            let path_to_scan = exec_result.map(|r| r.db_path()).unwrap_or(&repo.path);
            let root_path = exec_result.map(|r| &r.current_repo.root_path).unwrap_or(&repo.root_path);
            
            // Check if path exists
            if !std::path::Path::new(path_to_scan).exists() {
                eprintln!("   {} Repository path does not exist, skip rescan: {}", "⚠️".yellow(), path_to_scan);
                // If the path does not exist, delete the record from the database
                if let Err(e) = db.delete_repository(&repo.path) {
                    eprintln!("   {} Delete from database failed: {}", "⚠️".yellow(), e);
                }
                continue;
            }
            
            match GitOps::inspect(std::path::Path::new(path_to_scan), root_path) {
                Ok(mut updated) => {
                    // Preserve the original metadata
                    updated.id = repo.id;
                    if let Some(exec_result) = exec_result {
                        // If fetch succeeded, use the original fetch time; otherwise keep the database value
                        if exec_result.success {
                            updated.last_fetch_at = Some(chrono::Local::now());
                        } else {
                            updated.last_fetch_at = repo.last_fetch_at;
                        }
                    } else {
                        updated.last_fetch_at = repo.last_fetch_at;
                    }
                    
                    if let Err(e) = db.upsert_repository(&mut updated) {
                        eprintln!("Update repository failed '{}': {}", updated.name, e);
                    }
                    updated_repos.push(updated);
                }
                Err(e) => {
                    // If the scan fails (e.g., repository moved to an invalid path), log the error but keep the original info
                    eprintln!("Rescan failed '{}': {}", repo.name, e);
                    
                    // If the repository was moved, try to use the moved info
                    if let Some(exec_result) = exec_result {
                        if exec_result.moved_to_needauth {
                            // Use the moved repository info, but mark it as possibly needing a manual check
                            let mut moved_repo = exec_result.current_repo.clone();
                            moved_repo.last_fetch_at = repo.last_fetch_at;
                            updated_repos.push(moved_repo);
                            continue;
                        }
                    }
                    
                    updated_repos.push(repo.clone());
                }
            }
        }

        Ok(updated_repos)
    }
}

/// Fetch result summary
#[derive(Debug)]
pub struct FetchSummary {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub results: Vec<FetchResultModel>,
}

impl FetchSummary {
    pub fn new() -> Self {
        Self {
            total: 0,
            success: 0,
            failed: 0,
            results: Vec::new(),
        }
    }

    pub fn print_summary(&self) {
        println!("\n📊 Fetch Results:");
        println!("   Total: {} | succeeded: {} | failed: {}", 
            self.total, self.success, self.failed);
        
        if self.failed > 0 {
            println!("\n❌ Failed repositories:");
            for result in &self.results {
                if !result.success {
                    if let Some(ref error) = result.error {
                        // Use Path method to get file name, supports Windows and Unicode
                        let short_path = std::path::Path::new(&result.repo_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&result.repo_path);
                        println!("   - {}: {}", short_path, error);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a minimal git repo with one commit
    fn init_git_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).expect("init git repo");
        let sig = git2::Signature::now("test", "test@test.com")
            .expect("create signature");
        let tree_id = {
            let mut index = repo.index().expect("get index");
            index.write_tree().expect("write tree")
        };
        let tree = repo.find_tree(tree_id).expect("find tree");
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "init",
            &tree,
            &[],
        ).expect("create commit");
    }

    #[test]
    fn move_repo_skips_when_already_at_target() {
        let tmp = TempDir::new().unwrap();
        let needauth_dir = tmp.path().join("needauth");
        let repo_dir = needauth_dir.join("test-repo");
        fs::create_dir_all(&repo_dir).unwrap();
        init_git_repo(&repo_dir);

        let target = needauth_dir.join("test-repo");

        // should not panic, should not delete contents
        Fetcher::move_repo_to_needauth(
            repo_dir.to_str().unwrap(),
            &target,
            None,
        ).unwrap();

        assert!(repo_dir.exists(), "repo directory must still exist");
        assert!(repo_dir.join(".git").exists(), ".git directory must still exist");
    }

    #[test]
    fn move_repo_successfully_relocates_from_outside() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("repos").join("my-repo");
        fs::create_dir_all(&source_dir).unwrap();
        init_git_repo(&source_dir);

        let test_marker = source_dir.join("test-file.txt");
        fs::write(&test_marker, "hello").unwrap();

        let target = tmp.path().join("needauth").join("my-repo");

        Fetcher::move_repo_to_needauth(
            source_dir.to_str().unwrap(),
            &target,
            None,
        ).unwrap();

        assert!(!source_dir.exists(), "source should be gone after move");
        assert!(target.exists(), "target should exist after move");
        assert!(target.join(".git").exists(), ".git should be at target");
        assert!(target.join("test-file.txt").exists(), "content should be at target");
    }

    #[test]
    fn move_repo_overwrites_existing_target() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("repos").join("repo");
        fs::create_dir_all(&source_dir).unwrap();
        init_git_repo(&source_dir);

        let target = tmp.path().join("needauth").join("repo");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("stale.txt"), "stale").unwrap();

        Fetcher::move_repo_to_needauth(
            source_dir.to_str().unwrap(),
            &target,
            None,
        ).unwrap();

        assert!(target.exists());
        assert!(target.join(".git").exists());
        assert!(!target.join("stale.txt").exists(), "old target content must be gone");
    }
}