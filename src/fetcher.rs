use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
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
    /// Number of retries performed for network errors
    pub retry_count: u32,
    /// Whether restored from needauth (auth issue resolved)
    pub restored_from_needauth: bool,
    #[allow(dead_code)]
    /// Whether this fetch fell back from git2 to native git command (保留字段，当前不再使用)
    pub fallback_from_git2: bool,
    #[allow(dead_code)]
    /// Reason for fallback (None if git2 succeeded directly) (保留字段，当前不再使用)
    pub fallback_reason: Option<String>,
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
            retry_count: self.retry_count,
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
        let main_pb = if progress {
            let pb = ProgressBar::new(repos.len() as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                    .expect("进度条模板格式错误，这是硬编码常量，不应失败")
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        let mut futures = FuturesUnordered::new();

        for repo in repos {
            // 若收到关闭请求，跳过生成新任务
            if crate::signal_handler::is_shutdown_requested() {
                break;
            }

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
                let repo_name = repo.name.clone();
                
                if let Some(ref pb) = main_pb {
                    pb.set_message(repo_name.clone());
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
                            retry_count: 0,
                        }, None, false);
                    }
                    
                    let path_buf = path.to_path_buf();
                    let repo_name = repo.name.clone();
                    let scan_result = match timeout(
                        Duration::from_secs(timeout_secs),
                        tokio::task::spawn_blocking(move || {
                            Self::scan_repository(&path_buf, &repo_name)
                        })
                    ).await {
                        Ok(Ok(Ok(r))) => Ok(r),
                        Ok(Ok(Err(e))) => Err(e),
                        Ok(Err(_)) => Err(anyhow::anyhow!("Security scan task panicked")),
                        Err(_) => Err(anyhow::anyhow!("Security scan timed out ({}s)", timeout_secs)),
                    };
                    
                    match scan_result {
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
                                    
                                    let confirmed: bool = tokio::task::spawn_blocking(|| {
                                        use std::io::{IsTerminal, Write};
                                        if !std::io::stdin().is_terminal() {
                                            eprintln!("Warning: stdin is not a TTY, defaulting to 'no' for security confirmation");
                                            return false;
                                        }
                                        let _ = std::io::stdout().flush();
                                        let mut input = String::new();
                                        match std::io::stdin().read_line(&mut input) {
                                            Ok(_) => input.trim().eq_ignore_ascii_case("y"),
                                            Err(_) => false,
                                        }
                                    }).await.unwrap_or_default();
                                    
                                    if !confirmed {
                                        return (original_repo, repo, FetchResultModel {
                                            repo_path: original_path,
                                            success: false,
                                            error: Some(error_msg),
                                            duration_ms: start.elapsed().as_millis() as u64,
                                            retry_count: 0,
                                        }, None, false);
                                    }
                                } else {
                                    return (original_repo, repo, FetchResultModel {
                                        repo_path: original_path,
                                        success: false,
                                        error: Some(error_msg),
                                        duration_ms: start.elapsed().as_millis() as u64,
                                        retry_count: 0,
                                    }, None, false);
                                }
                            }
                        }
                        Err(_e) => {
                            // Security scan failures are only logged; they don't interrupt the flow
                        }
                    }
                }

                // 2. Execute the fetch with exponential backoff retry for NetworkError
                let path = std::path::PathBuf::from(&original_path);
                let repo_path = original_path.clone();
                let repo_name = repo.name.clone();
                let root_path = repo.root_path.clone();
                let proxy_for_retry = proxy.clone();
                
                const MAX_RETRIES: u32 = 3;
                let mut retry_count = 0u32;
                let mut fetch_status = FetchStatus::Success;
                
                // Overall deadline for all attempts combined (prevents unbounded retry time)
                let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs.saturating_mul(2));
                
                for attempt in 0..=MAX_RETRIES {
                    let path = path.clone();
                    let proxy = proxy_for_retry.clone();
                    
                    let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        fetch_status = FetchStatus::NetworkError {
                            message: format!("Overall retry timeout exceeded (>{}s)", timeout_secs.saturating_mul(2))
                        };
                        break;
                    }

                    let attempt_timeout = std::cmp::min(Duration::from_secs(timeout_secs), remaining);

                    fetch_status = match timeout(
                        attempt_timeout,
                        tokio::task::spawn_blocking(move || {
                            let git_ops = GitOps::with_proxy(proxy);
                            git_ops.fetch_detailed(&path, attempt_timeout.as_secs())
                        })
                    ).await {
                        Ok(Ok((status, _))) => status,
                        Ok(Err(_)) => {
                            FetchStatus::OtherError { message: "Task was cancelled".to_string() }
                        }
                        Err(_) => {
                            FetchStatus::NetworkError { message: format!("Timeout ({}s)", attempt_timeout.as_secs()) }
                        }
                    };

                    match &fetch_status {
                        FetchStatus::NetworkError { .. } if attempt < MAX_RETRIES => {
                            let delay_secs = 2u64.pow(attempt);
                            let delay = std::cmp::min(Duration::from_secs(delay_secs), remaining.saturating_sub(Duration::from_millis(100)));
                            tokio::time::sleep(delay).await;
                            retry_count = attempt + 1;
                        }
                        _ => break,
                    }
                }
                // 3. Handle needauth move or restore
                let (current_repo, result, moved_repo_name, restored_from_needauth) = if fetch_status.should_move_to_needauth() && move_to_needauth {
                    let needauth_dir = std::path::PathBuf::from(&root_path).join(crate::utils::NEEDAUTH_DIR);
                    let needauth_path = needauth_dir.join(&repo_name);
                    
                    let repo_path_clone = repo_path.clone();
                    let upstream_url_clone = repo.upstream_url.clone();
                    let needauth_path_clone = needauth_path.clone();
                    
                    let needauth_dir_clone = needauth_dir.clone();
                    let move_result = match timeout(
                        Duration::from_secs(timeout_secs),
                        tokio::task::spawn_blocking(move || {
                            Self::move_repo_to_needauth(&repo_path_clone, &needauth_path_clone, &needauth_dir_clone, upstream_url_clone.as_deref())
                        })
                    ).await {
                        Ok(Ok(Ok(path))) => Ok(path),
                        Ok(Ok(Err(e))) => Err(e),
                        Ok(Err(_)) => Err(anyhow::anyhow!("Move task panicked")),
                        Err(_) => Err(anyhow::anyhow!("Move operation timed out ({}s)", timeout_secs)),
                    };
                    
                    match move_result {
                        Ok(final_path) => {
                            let new_repo_path = final_path.to_string_lossy().to_string();
                            let new_root_path = needauth_dir.to_string_lossy().to_string();
                            let final_name = final_path.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| repo_name.clone());
                            
                            let mut new_repo = repo.with_new_path(new_repo_path, new_root_path);
                            let name_for_result = final_name.clone();
                            new_repo.name = final_name;
                            
                            let result = FetchResultModel {
                                repo_path: original_path,
                                success: false,
                                error: fetch_status.error_message(),
                                duration_ms: start.elapsed().as_millis() as u64,
                                retry_count,
                            };
                            (new_repo, result, Some(name_for_result), false)
                        }
                        Err(e) => {
                            let result = FetchResultModel {
                                repo_path: original_path,
                                success: false,
                                error: Some(format!("{} (Move failed: {})", 
                                    fetch_status.error_message().unwrap_or_default(), e)),
                                duration_ms: start.elapsed().as_millis() as u64,
                                retry_count,
                            };
                            (repo, result, None, false)
                        }
                    }
                } else if matches!(fetch_status, FetchStatus::Success) && original_path.contains(crate::utils::NEEDAUTH_DIR) {
                    // NOTE: Design limitation — this assumes the original repository was a direct child
                    // of the scan root. If the repo was originally at `<root>/sub/myrepo`, recovery will
                    // move it to `<root>/myrepo` instead. Preserving the full relative path would require
                    // storing it in the database (schema change).
                    let needauth_parent = std::path::Path::new(&original_path).parent()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from(&root_path));
                    let original_repo_path = needauth_parent.join(&repo_name);
                    
                    let from_path = original_path.clone();
                    let upstream = repo.upstream_url.clone();
                    let needauth_parent_clone = needauth_parent.clone();
                    let restore_result = match timeout(
                        Duration::from_secs(timeout_secs),
                        tokio::task::spawn_blocking(move || {
                            Self::move_repo_from_needauth(&from_path, &original_repo_path, &needauth_parent_clone, upstream.as_deref())
                        })
                    ).await {
                        Ok(Ok(Ok(path))) => Ok(path),
                        Ok(Ok(Err(e))) => Err(e),
                        Ok(Err(_)) => Err(anyhow::anyhow!("Restore task panicked")),
                        Err(_) => Err(anyhow::anyhow!("Restore operation timed out ({}s)", timeout_secs)),
                    };
                    
                    match restore_result {
                        Ok(restored_path) => {
                            let new_path = restored_path.to_string_lossy().to_string();
                            let new_root = restored_path.parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| needauth_parent.to_string_lossy().to_string());
                            let mut restored_repo = repo.with_new_path(new_path, new_root);
                            restored_repo.name = repo_name.clone();
                            
                            let result = FetchResultModel {
                                repo_path: restored_repo.path.clone(),
                                success: true,
                                error: None,
                                duration_ms: start.elapsed().as_millis() as u64,
                                retry_count,
                            };
                            (restored_repo, result, None, true)
                        }
                        Err(e) => {
                            let result = FetchResultModel {
                                repo_path: original_path,
                                success: true,
                                error: Some(format!("Fetch succeeded, but restore from needauth failed: {}", e)),
                                duration_ms: start.elapsed().as_millis() as u64,
                                retry_count,
                            };
                            (repo, result, None, false)
                        }
                    }
                } else {
                    let result = FetchResultModel {
                        repo_path: original_path,
                        success: matches!(fetch_status, FetchStatus::Success),
                        error: fetch_status.error_message(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        retry_count,
                    };
                    (repo, result, None, false)
                };

                if let Some(ref pb) = main_pb {
                    pb.inc(1);
                }

                (original_repo, current_repo, result, moved_repo_name, restored_from_needauth)
            });

            futures.push(future);
        }

        let mut results = Vec::new();
        let mut moved_repos: Vec<String> = Vec::new();
        let mut restored_repos: Vec<String> = Vec::new();

        while !futures.is_empty() {
            match timeout(Duration::from_millis(200), futures.next()).await {
                Ok(Some(join_result)) => {
                    match join_result {
                        Ok((original_repo, current_repo, result_model, moved_name, restored)) => {
                            let moved = moved_name.is_some();

                            // 在移动 current_repo 之前收集已恢复仓库的名称
                            if restored {
                                restored_repos.push(current_repo.name.clone());
                            }

                            let exec_result = FetchExecutionResult {
                                original_repo,
                                current_repo,
                                success: result_model.success,
                                error: result_model.error,
                                duration_ms: result_model.duration_ms,
                                moved_to_needauth: moved,
                                retry_count: result_model.retry_count,
                                restored_from_needauth: restored,
                                fallback_from_git2: false,
                                fallback_reason: None,
                            };

                            // 收集已移动的仓库
                            if let Some(name) = moved_name {
                                moved_repos.push(name);
                            }

                            results.push(exec_result);
                        }
                        Err(e) => eprintln!("  │   {} Task exception: {}", "⚠️".yellow(), e),
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    if crate::signal_handler::is_shutdown_requested() {
                        eprintln!("  ⚠️  收到中断信号，取消等待剩余任务");
                        break;
                    }
                }
            }
        }

        // 进度条完成后，统一显示各类信息
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
        
        if !restored_repos.is_empty() {
            println!("  ├─ {} The following repositories were restored from needauth/:", "📁".green());
            for (i, name) in restored_repos.iter().enumerate() {
                let is_last = i == restored_repos.len() - 1;
                let corner = if is_last { "└─" } else { "├─" };
                println!("  │   {} {}", corner, name.green());
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
        expected_parent: &std::path::Path,
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
        
        // 7. Verify target path is within the expected parent directory (defense in depth)
        if !expected_parent.exists() {
            fs::create_dir_all(expected_parent)
                .with_context(|| format!("Unable to create expected parent directory: {}", expected_parent.display()))?;
        }
        let expected_canonical = expected_parent.canonicalize()
            .with_context(|| format!("Unable to resolve expected parent directory: {}", expected_parent.display()))?;
        if !to_canonical.starts_with(&expected_canonical) {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' is not inside expected directory '{}'",
                to_canonical.display(),
                expected_canonical.display()
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
            // If move fails, try to restore original target from temp
            if !tmp_path.as_os_str().is_empty() {
                if let Err(restore_err) = fs::rename(&tmp_path, &final_to) {
                    return Err(anyhow::anyhow!(
                        "CRITICAL: Unable to move repository to '{}': {}. Additionally, restoring the original target from temp '{}' failed: {}. Original data may be at '{}'.",
                        final_to.display(), e, tmp_path.display(), restore_err, tmp_path.display()
                    ));
                }
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

    /// Move repository from needauth directory back to original location
    ///
    /// Used when a previously authentication-failed repository successfully fetches again,
    /// indicating the authentication issue has been resolved.
    ///
    /// # Safety rules
    /// - If target path exists and is the same repository (upstream_url matches) → skip (don't overwrite)
    /// - If target path exists but is a different repository → skip (preserve user's new clone)
    /// - If target path exists but is not a git repository → skip (don't overwrite non-git data)
    /// - If target path does not exist → execute two-phase atomic move
    fn move_repo_from_needauth(
        from: &str,
        to: &std::path::Path,
        expected_parent: &std::path::Path,
        upstream_url: Option<&str>,
    ) -> Result<std::path::PathBuf, anyhow::Error> {
        use std::fs;
        use std::path::Path;

        let from_path = Path::new(from);

        // ── Path traversal protection ────────────────────────────────────────────────
        let to_str = to.to_string_lossy();
        if to_str.contains("..") || to_str.contains("//") {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' contains illegal components",
                to.display()
            ));
        }

        let parent = to.parent()
            .ok_or_else(|| anyhow::anyhow!("Target path has no parent directory: {}", to.display()))?;

        let parent_canonical = parent.canonicalize()
            .with_context(|| format!("Unable to resolve parent directory: {}", parent.display()))?;

        let file_name = to.file_name()
            .ok_or_else(|| anyhow::anyhow!("Unable to get target file name: {}", to.display()))?;
        let to_canonical = parent_canonical.join(file_name);

        if !to_canonical.starts_with(&parent_canonical) {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' is not inside expected directory '{}'",
                to_canonical.display(),
                parent_canonical.display()
            ));
        }
        
        // Verify target path is within the expected parent directory (defense in depth)
        if !expected_parent.exists() {
            fs::create_dir_all(expected_parent)
                .with_context(|| format!("Unable to create expected parent directory: {}", expected_parent.display()))?;
        }
        let expected_canonical = expected_parent.canonicalize()
            .with_context(|| format!("Unable to resolve expected parent directory: {}", expected_parent.display()))?;
        if !to_canonical.starts_with(&expected_canonical) {
            return Err(anyhow::anyhow!(
                "Path traversal detected: target path '{}' is not inside expected directory '{}'",
                to_canonical.display(),
                expected_canonical.display()
            ));
        }

        // ── Self-reference protection ──────────────────────────────────────────────
        match Self::try_canonicalize(from_path) {
            Some(from_canon) if from_canon == to_canonical => {
                return Ok(to.to_path_buf());
            }
            _ => {}
        }

        // ── Target existence check ──────────────────────────────────────────────
        if to.exists() {
            let target_url = Self::get_repo_remote_url(to);
            if target_url.is_some() && upstream_url.is_some() && target_url.as_deref() == upstream_url {
                // Same repository already exists at target — user likely re-cloned it
                return Err(anyhow::anyhow!(
                    "Target path '{}' already contains the same repository, skipping restore",
                    to.display()
                ));
            } else if to.join(".git").exists() {
                // Different repository exists at target — don't overwrite
                return Err(anyhow::anyhow!(
                    "Target path '{}' already contains a different repository, skipping restore",
                    to.display()
                ));
            } else {
                // Non-git directory exists at target — don't overwrite
                return Err(anyhow::anyhow!(
                    "Target path '{}' already exists and is not a git repository, skipping restore",
                    to.display()
                ));
            }
        }

        // ── Ensure parent directory exists ──────────────────────────────────────────────
        fs::create_dir_all(parent)
            .with_context(|| format!("Unable to create parent directory: {}", parent.display()))?;

        // ── Two-phase atomic move ──────────────────────────────────────────────
        if let Err(e) = fs::rename(from, &to_canonical) {
            return Err(anyhow::anyhow!(
                "Unable to move repository from '{}' to '{}': {}",
                from, to_canonical.display(), e
            ));
        }

        Ok(to_canonical)
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
        _db: &Database,
        progress: bool,
    ) -> Result<FetchSummary> {
        let exec_results = self.fetch_all_detailed(repos, progress).await;

        let mut summary = FetchSummary::new();

        for exec_result in &exec_results {
            if exec_result.success {
                summary.success += 1;
            } else {
                summary.failed += 1;
            }
            summary.total += 1;
        }

        summary.results = exec_results.iter().map(|r| r.to_model()).collect();

        // DB operations in blocking task to avoid blocking the async runtime
        tokio::task::spawn_blocking(move || {
            let db = Database::open()?;
            for exec_result in exec_results {
                if exec_result.success {
                    if let Err(e) = db.update_fetch_time(exec_result.db_path()) {
                        eprintln!("Update fetch time failed '{}': {}", crate::utils::sanitize_path(exec_result.db_path()), e);
                    }
                }
                // If the repository was moved to need-auth, atomically update the database
                if exec_result.moved_to_needauth {
                    let mut moved_repo = exec_result.current_repo.clone();
                    if let Err(e) = db.move_repository(&exec_result.original_repo.path, &mut moved_repo) {
                        eprintln!("Move repository record failed '{}': {}", crate::utils::sanitize_path(exec_result.db_path()), e);
                    }
                }
                // If the repository was restored from needauth (auth resolved), update the database path
                if exec_result.restored_from_needauth {
                    let mut restored_repo = exec_result.current_repo.clone();
                    if let Err(e) = db.move_repository(&exec_result.original_repo.path, &mut restored_repo) {
                        eprintln!("Restore repository record failed '{}': {}", crate::utils::sanitize_path(exec_result.db_path()), e);
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        }).await??;

        Ok(summary)
    }

    /// fetch 后重新扫描状态
    /// 
    /// 正确处理仓库移动：
    /// - 若仓库已移动到 needauth，使用新路径重新扫描
    /// - 保留 fetch 时间等元数据
    pub async fn fetch_and_rescan(
        &self,
        repos: &[Repository],
        db: &Database,
        progress: bool,
    ) -> Result<Vec<Repository>> {
        // Get detailed execution results (include path change information)
        let exec_results = self.fetch_all_detailed(repos, progress).await;

        let mut updated_repos = Vec::new();
        
        // 构建从原始路径到执行结果的映射，用于快速查找
        let result_map: std::collections::HashMap<String, &FetchExecutionResult> = exec_results
            .iter()
            .map(|r| (r.original_repo.path.clone(), r))
            .collect();
        
        for repo in repos {
            if crate::signal_handler::is_shutdown_requested() {
                eprintln!("  ⚠️  收到中断信号，跳过后续仓库扫描");
                break;
            }

            // 查找对应的执行结果
            let exec_result = result_map.get(&repo.path);
            
            // 使用当前路径（可能是新路径）重新扫描
            let path_to_scan = exec_result.map(|r| r.db_path()).unwrap_or(&repo.path);
            let root_path = exec_result.map(|r| &r.current_repo.root_path).unwrap_or(&repo.root_path);
            
            // 检查路径是否存在
            if !std::path::Path::new(path_to_scan).exists() {
                eprintln!("   {} Repository path does not exist, skip rescan: {}", "⚠️".yellow(), path_to_scan);
                // 若路径不存在，从数据库中删除该记录
                if let Err(e) = db.delete_repository(path_to_scan) {
                    eprintln!("   {} Delete from database failed: {}", "⚠️".yellow(), e);
                }
                continue;
            }
            
            let path_buf = std::path::PathBuf::from(path_to_scan);
            let root_path_str = root_path.to_string();
            let inspect_result = match timeout(
                Duration::from_secs(self.timeout_secs),
                tokio::task::spawn_blocking(move || {
                    GitOps::inspect(&path_buf, &root_path_str)
                })
            ).await {
                Ok(Ok(Ok(r))) => Ok(r),
                Ok(Ok(Err(e))) => Err(e),
                Ok(Err(_)) => Err(crate::error::GetLatestRepoError::Other(anyhow::anyhow!("Inspect task panicked"))),
                Err(_) => Err(crate::error::GetLatestRepoError::Other(anyhow::anyhow!("Inspect timed out ({}s)", self.timeout_secs))),
            };
            
            match inspect_result {
                Ok(mut updated) => {
                    // 保留原始元数据
                    updated.id = repo.id;
                    if let Some(exec_result) = exec_result {
                        // 若 fetch 成功，使用原始 fetch 时间；否则保留数据库中的值
                        if exec_result.success {
                            updated.last_fetch_at = Some(chrono::Local::now());
                        } else {
                            updated.last_fetch_at = repo.last_fetch_at;
                        }
                    } else {
                        updated.last_fetch_at = repo.last_fetch_at;
                    }
                    
                    // 若从 needauth 恢复，原子性地移动数据库记录（删除旧记录 + 插入新记录）
                    // 避免在旧 needauth 路径留下孤儿记录
                    let db_result = if exec_result.map(|r| r.restored_from_needauth).unwrap_or(false) {
                        db.move_repository(&repo.path, &mut updated)
                    } else {
                        db.upsert_repository(&mut updated)
                    };
                    if let Err(e) = db_result {
                        eprintln!("Update repository failed '{}': {}", updated.name, e);
                    }
                    updated_repos.push(updated);
                }
                Err(e) => {
                    // 若扫描失败（例如仓库移动到了无效路径），记录错误但保留原始信息
                    eprintln!("Rescan failed '{}': {}", repo.name, e);
                    
                    // If the repository was moved or restored, try to use the current info
                    if let Some(exec_result) = exec_result {
                        if exec_result.moved_to_needauth {
                            // Use the moved repository info, but mark it as possibly needing a manual check
                            let mut moved_repo = exec_result.current_repo.clone();
                            moved_repo.last_fetch_at = repo.last_fetch_at;
                            updated_repos.push(moved_repo);
                            continue;
                        } else if exec_result.restored_from_needauth {
                            // Use the restored repository info and atomically update the DB record
                            let mut restored_repo = exec_result.current_repo.clone();
                            restored_repo.last_fetch_at = repo.last_fetch_at;
                            if let Err(e) = db.move_repository(&exec_result.original_repo.path, &mut restored_repo) {
                                eprintln!("Restore repository record failed (rescan error) '{}': {}", restored_repo.name, e);
                            }
                            updated_repos.push(restored_repo);
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
        
        // 按错误类型分类
        let mut network_failures = Vec::new();
        let mut auth_failures = Vec::new();
        let mut other_failures = Vec::new();
        
        for result in &self.results {
            if !result.success {
                if let Some(ref error) = result.error {
                    if error.contains("Network error") || error.contains("Timeout") {
                        network_failures.push(result);
                    } else if error.contains("Authentication required") 
                        || error.contains("Repository not found")
                        || error.contains("Move failed")
                        || error.contains("Move task panicked")
                    {
                        auth_failures.push(result);
                    } else {
                        other_failures.push(result);
                    }
                }
            }
        }
        
        if self.failed > 0 {
            println!("   Total: {} | succeeded: {} | failed: {} (network: {}, auth: {}, other: {})", 
                self.total, self.success, self.failed,
                network_failures.len(), auth_failures.len(), other_failures.len());
        } else {
            println!("   Total: {} | succeeded: {} | failed: {}", 
                self.total, self.success, self.failed);
        }
        
        if self.failed > 0 {
            println!("\n⚠️ 失败详情:");
            
            let print_group = |label: &str, icon: &str, items: &[&FetchResultModel]| {
                if items.is_empty() { return; }
                println!("   {} {} ({}个):", icon, label, items.len());
                for (i, result) in items.iter().enumerate() {
                    let is_last = i == items.len() - 1;
                    let corner = if is_last { "└─" } else { "├─" };
                    let short_path = std::path::Path::new(&result.repo_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&result.repo_path);
                    let retry_info = if result.retry_count > 0 {
                        format!(" (重试{}次)", result.retry_count)
                    } else {
                        String::new()
                    };
                    println!("      {} {}{}: {}", 
                        corner, short_path, retry_info,
                        result.error.as_deref().unwrap_or("Unknown error"));
                }
            };
            
            print_group("网络错误", "🔌", &network_failures);
            print_group("认证/仓库错误", "🔒", &auth_failures);
            print_group("其他错误", "❌", &other_failures);
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
            &needauth_dir,
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
            target.parent().unwrap_or(tmp.path()),
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
            target.parent().unwrap_or(tmp.path()),
            None,
        ).unwrap();

        assert!(target.exists());
        assert!(target.join(".git").exists());
        assert!(!target.join("stale.txt").exists(), "old target content must be gone");
    }
}