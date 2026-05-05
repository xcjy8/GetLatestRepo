use anyhow::Context;
use colored::Colorize;
use git2::{BranchType, Repository as GitRepository, StatusOptions};
use std::path::Path;

use crate::error::{GetLatestRepoError, Result};
use crate::models::{Freshness, Repository};

/// Proxy configuration
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Whether proxy is enabled
    pub enabled: bool,
    /// HTTP proxy address
    pub http_proxy: String,
    /// HTTPS proxy address
    pub https_proxy: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_proxy: crate::utils::DEFAULT_PROXY_URL.to_string(),
            https_proxy: crate::utils::DEFAULT_PROXY_URL.to_string(),
        }
    }
}

/// Fetch result types
#[derive(Debug, Clone)]
pub enum FetchStatus {
    /// Success
    Success,
    /// Authentication required (401/403)
    AuthenticationRequired { message: String },
    /// Repository not found/private (404)
    RepositoryNotFound { message: String },
    /// Network/timeout errors
    NetworkError { message: String },
    /// Other errors
    OtherError { message: String },
}

impl FetchStatus {
    /// Whether to move to needauth directory
    pub fn should_move_to_needauth(&self) -> bool {
        matches!(self, 
            FetchStatus::AuthenticationRequired { .. } | 
            FetchStatus::RepositoryNotFound { .. }
        )
    }

    /// Get error message
    pub fn error_message(&self) -> Option<String> {
        match self {
            FetchStatus::Success => None,
            FetchStatus::AuthenticationRequired { message } => {
                Some(format!("Authentication required (401/403): {}", message))
            }
            FetchStatus::RepositoryNotFound { message } => {
                Some(format!("Repository not found or made private (404): {}", message))
            }
            FetchStatus::NetworkError { message } => {
                Some(format!("Network error: {}", message))
            }
            FetchStatus::OtherError { message } => {
                Some(format!("Error: {}", message))
            }
        }
    }
}

/// Git operations wrapper
pub struct GitOps {
    proxy: ProxyConfig,
}

impl GitOps {
    /// Create instance with proxy
    pub fn with_proxy(proxy: ProxyConfig) -> Self {
        Self { proxy }
    }

    /// Open repository
    pub fn open(path: &Path) -> Result<GitRepository> {
        GitRepository::open(path)
            .map_err(|e| GetLatestRepoError::OpenRepo {
                path: path.display().to_string(),
                source: e,
            })
    }

    /// Check if path is a Git repository
    pub fn is_repository(path: &Path) -> bool {
        GitRepository::open(path).is_ok()
    }

    /// Get repository info
    pub fn inspect(path: &Path, root_path: &str) -> Result<Repository> {
        let repo = Self::open(path)?;
        let path_str = path.to_string_lossy().to_string();
        let name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Calculate depth relative to root_path
        let depth = path.strip_prefix(root_path)
            .map(|p| p.components().count() as u32)
            .unwrap_or(0);

        // Get current branch
        let branch = Self::get_current_branch(&repo)?;

        // Check local changes (get detailed info)
        let (dirty, file_changes) = Self::check_dirty(&repo)?;
        // Also generate a path list for database compatibility
        let dirty_files: Vec<String> = file_changes.iter()
            .map(|fc| fc.path.clone())
            .collect();

        // Get upstream info (sanitize URL to remove credentials before storage)
        let (upstream_ref, upstream_url) = Self::get_upstream_info(&repo)?;
        let upstream_url = upstream_url.map(|u| crate::utils::sanitize_url(&u));

        // Calculate ahead/behind
        let (ahead_count, behind_count, freshness) = 
            Self::calculate_sync_status(&repo)?;

        // Get last commit info
        let (last_commit_at, last_commit_message, last_commit_author) = 
            Self::get_last_commit_info(&repo)?;

        Ok(Repository {
            id: None,
            path: path_str,
            root_path: root_path.to_string(),
            name,
            depth,
            branch,
            dirty,
            file_changes,
            dirty_files,
            upstream_ref,
            upstream_url,
            ahead_count,
            behind_count,
            freshness,
            last_commit_at,
            last_commit_message,
            last_commit_author,
            last_scanned_at: Some(chrono::Local::now()),
            last_fetch_at: None,
            last_pull_at: None,
        })
    }

    /// Get current branch name
    fn get_current_branch(repo: &GitRepository) -> Result<Option<String>> {
        let head = match repo.head() {
            Ok(head) => head,
            Err(_) => return Ok(None),
        };

        if let Some(name) = head.shorthand() {
            return Ok(Some(name.to_string()));
        }

        Ok(None)
    }

    /// Check for uncommitted local changes (returns detailed change info)
    fn check_dirty(repo: &GitRepository) -> Result<(bool, Vec<crate::models::FileChange>)> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true);

        let statuses = repo.statuses(Some(&mut opts))?;
        let mut file_changes = Vec::new();

        for entry in statuses.iter() {
            if let Some(path) = entry.path() {
                let status = entry.status();
                
                // Determine change type
                let status_str = if status.contains(git2::Status::WT_NEW) || 
                                    status.contains(git2::Status::INDEX_NEW) {
                    "added"
                } else if status.contains(git2::Status::WT_DELETED) || 
                          status.contains(git2::Status::INDEX_DELETED) {
                    "deleted"
                } else if status.contains(git2::Status::WT_RENAMED) || 
                          status.contains(git2::Status::INDEX_RENAMED) {
                    "renamed"
                } else if status.contains(git2::Status::WT_TYPECHANGE) || 
                          status.contains(git2::Status::INDEX_TYPECHANGE) {
                    "typechange"
                } else if status.contains(git2::Status::WT_MODIFIED) || 
                          status.contains(git2::Status::INDEX_MODIFIED) {
                    "modified"
                } else if status.contains(git2::Status::IGNORED) {
                    "ignored"
                } else {
                    "unknown"
                };

                let staged = status.intersects(
                    git2::Status::INDEX_NEW |
                    git2::Status::INDEX_MODIFIED |
                    git2::Status::INDEX_DELETED |
                    git2::Status::INDEX_RENAMED |
                    git2::Status::INDEX_TYPECHANGE
                );

                file_changes.push(crate::models::FileChange::new(
                    path.to_string(),
                    status_str,
                    staged
                ));
            }
        }

        let is_dirty = !file_changes.is_empty();
        Ok((is_dirty, file_changes))
    }

    /// Get upstream info
    fn get_upstream_info(repo: &GitRepository) -> Result<(Option<String>, Option<String>)> {
        let branch = match Self::get_current_branch(repo)? {
            Some(b) => b,
            None => return Ok((None, None)),
        };

        let local_branch = match repo.find_branch(&branch, BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok((None, None)),
        };

        let upstream = match local_branch.upstream() {
            Ok(u) => u,
            Err(_) => return Ok((None, None)),
        };

        let upstream_ref = upstream.name()?
            .map(|s| s.to_string());

        // Get remote URL
        let upstream_ref_str = upstream.get().name()
            .map(|s| s.to_string())
            .unwrap_or_default();
        
        let upstream_url = if upstream_ref_str.starts_with("refs/remotes/") {
            let parts: Vec<&str> = upstream_ref_str.split('/').collect();
            if parts.len() >= 3 {
                let remote_name = parts[2];
                repo.find_remote(remote_name)
                    .ok()
                    .and_then(|r| r.url().map(|u| u.to_string()))
            } else {
                None
            }
        } else {
            None
        };

        Ok((upstream_ref, upstream_url))
    }

    /// Calculate sync status
    fn calculate_sync_status(repo: &GitRepository) -> Result<(i32, i32, Freshness)> {
        let local_branch = match Self::get_current_branch(repo)? {
            Some(b) => b,
            None => return Ok((0, 0, Freshness::NoRemote)),
        };

        let branch = match repo.find_branch(&local_branch, BranchType::Local) {
            Ok(b) => b,
            Err(_) => return Ok((0, 0, Freshness::NoRemote)),
        };

        let upstream = match branch.upstream() {
            Ok(u) => u,
            Err(_) => return Ok((0, 0, Freshness::NoRemote)),
        };

        let local_ref = branch.get().target();
        let upstream_ref = upstream.get().target();

        let (local_oid, upstream_oid) = match (local_ref, upstream_ref) {
            (Some(local), Some(upstream)) => (local, upstream),
            _ => return Ok((0, 0, Freshness::NoRemote)),
        };

        // Calculate ahead/behind
        let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;

        let freshness = if behind > 0 {
            Freshness::HasUpdates
        } else {
            Freshness::Synced
        };

        Ok((ahead as i32, behind as i32, freshness))
    }

    /// Get last commit info
    #[allow(clippy::type_complexity)]
    fn get_last_commit_info(repo: &GitRepository) -> Result<(Option<chrono::DateTime<chrono::Local>>, Option<String>, Option<String>)> {
        let head = match repo.head() {
            Ok(head) => head,
            Err(_) => return Ok((None, None, None)),
        };

        let oid = match head.target() {
            Some(oid) => oid,
            None => return Ok((None, None, None)),
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => return Ok((None, None, None)),
        };

        let time = commit.time();
        let dt = chrono::DateTime::from_timestamp(time.seconds(), 0)
            .map(|dt| dt.with_timezone(&chrono::Local));

        let message = commit.message()
            .map(|m| m.trim().to_string());

        let author = commit.author().name()
            .map(|n| n.to_string());

        Ok((dt, message, author))
    }

    /// 使用原生 git 命令执行 fetch（兜底路径）
    ///
    /// 当 git2 因认证、代理或网络配置问题失败时，使用原生 git 命令兜底。
    /// 原生 git 会读取 ~/.ssh/config、使用 ssh-agent、支持 credential-helper，
    /// 且可以通过 child.kill() 在超时后强制终止。
    fn fetch_with_git_command(&self, path: &Path, timeout_secs: u64) -> FetchStatus {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(path)
            .args(["fetch", "origin"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_HTTP_LOW_SPEED_TIME", "10")
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        // 使用环境变量传递代理，兼容旧版本 git（不支持 `git -c`）
        if self.proxy.enabled {
            cmd.env("HTTP_PROXY", &self.proxy.http_proxy)
               .env("HTTPS_PROXY", &self.proxy.https_proxy)
               .env("ALL_PROXY", &self.proxy.http_proxy);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return FetchStatus::OtherError {
                    message: format!("Failed to start git fetch: {e}"),
                };
            }
        };

        // Drain stderr in a background thread to prevent pipe buffer deadlock
        let stderr_handle = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut err, &mut buf);
                buf
            })
        });

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stderr_buf = stderr_handle
                        .and_then(|h| h.join().ok())
                        .unwrap_or_default();
                    let stderr = String::from_utf8_lossy(&stderr_buf);

                    if status.success() {
                        return FetchStatus::Success;
                    }

                    let exit_code = status.code().unwrap_or(-1);
                    let error_msg = format!("git fetch failed (exit {exit_code}): {}", stderr.trim());
                    return Self::classify_error(&error_msg);
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return FetchStatus::NetworkError {
                            message: format!("Timeout ({}s)", timeout_secs),
                        };
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return FetchStatus::OtherError {
                        message: format!("Failed waiting for git fetch: {e}"),
                    };
                }
            }
        }
    }

    /// 对外接口：使用原生 git 命令执行 fetch
    ///
    /// 使用原生 git 而非 git2 的原因（v0.1.5 验证结论）：
    /// 1. 原生 git 支持 SSH agent、credential-helper、~/.ssh/config 等完整认证链
    /// 2. git2 在认证兼容性上有局限（不支持 credential-helper、部分 SSH 配置）
    /// 3. 原生 git 可通过 child.kill() 在超时后强制终止，行为更可预测
    pub fn fetch_detailed(&self, path: &Path, timeout_secs: u64) -> (FetchStatus, Option<String>) {
        let status = self.fetch_with_git_command(path, timeout_secs);
        (status, None)
    }

    /// Classify error type
    fn classify_error(error_msg: &str) -> FetchStatus {
        let msg = error_msg.to_lowercase();

        // Rate limiting (must be checked before auth — GitHub returns 403 for rate limits)
        if msg.contains("rate limit") || msg.contains("too many requests") || msg.contains("429") {
            return FetchStatus::NetworkError {
                message: format!("Rate limited: {}", error_msg),
            };
        }

        // Authentication-related errors (403 excluded here — it could be rate limiting)
        if msg.contains("401") ||
           msg.contains("authentication") || msg.contains("credentials") ||
           msg.contains("authorization") || msg.contains("unauthorized") {
            return FetchStatus::AuthenticationRequired {
                message: error_msg.to_string()
            };
        }
        
        // 404 repository not found
        if msg.contains("404") || msg.contains("not found") || 
           msg.contains("could not resolve") || msg.contains("repository not found") {
            return FetchStatus::RepositoryNotFound { 
                message: error_msg.to_string() 
            };
        }
        
        // Network/timeout errors
        // Includes patterns from both git2 and native `git fetch` (curl/libcurl)
        if msg.contains("timeout") || msg.contains("timed out") ||
           msg.contains("connection refused") || msg.contains("couldn't connect") ||
           msg.contains("network") || msg.contains("unreachable") ||
           msg.contains("unable to access") || msg.contains("rpc failed") ||
           msg.contains("curl") || msg.contains("openssl") ||
           msg.contains("operation timed out") || msg.contains("failed to connect") {
            return FetchStatus::NetworkError { 
                message: error_msg.to_string() 
            };
        }
        
        // Other errors
        FetchStatus::OtherError { 
            message: error_msg.to_string() 
        }
    }

}

/// Pull force execution results
#[derive(Debug, Clone)]
pub enum PullForceOutcome {
    /// Success (clean repository directly pulled, or dirty repository stash-pull-pop succeeded)
    Success,
    /// Stash pop conflict (pull succeeded, but pop failed)
    Conflict {
        /// Stash message
        stash_name: String,
        /// Conflict file list
        conflict_files: Vec<String>,
        /// Stash index in stash list (e.g., stash@{2})
        stash_index: Option<usize>,
    },
}

impl GitOps {
    /// Safe pull: fast-forward only for clean repositories
    /// 
    /// Precondition checks:
    /// - Repository must exist
    /// - Must have a current branch
    /// - Remote branch must exist
    /// - Local must be clean (guaranteed by caller)
    pub fn pull_ff_only(path: &Path) -> Result<()> {
        let repo = Self::open(path)?;
        
        let branch = Self::get_current_branch(&repo)?;
        let branch_name = match branch {
            Some(b) => b,
            None => return Err(crate::error::GetLatestRepoError::DetachedHead),
        };

        // Check if remote branch exists
        let remote_branch = format!("origin/{}", branch_name);
        let remote_ref_name = format!("refs/remotes/{}", remote_branch);

        let remote_ref = match repo.find_reference(&remote_ref_name) {
            Ok(r) => r,
            Err(_) => return Err(crate::error::GetLatestRepoError::RemoteBranchMissing),
        };

        let remote_oid = remote_ref.target()
            .ok_or_else(|| GetLatestRepoError::Other(anyhow::anyhow!("Unable to get remote branch '{}' OID", remote_branch)))?;

        // Get local branch reference
        let local_ref_name = format!("refs/heads/{}", branch_name);
        let mut local_ref = repo.find_reference(&local_ref_name)
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("Unable to find local branch '{}': {}", branch_name, e)))?;

        // Fast-forward merge
        let remote_obj = repo.find_object(remote_oid, None)
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("Unable to find remote commit object: {}", e)))?;
        
        // Save original OID for potential rollback
        let original_oid = local_ref.target()
            .ok_or_else(|| GetLatestRepoError::Other(anyhow::anyhow!("Unable to get current branch OID")))?;

        // Verify this is actually a fast-forward (local is ancestor of remote)
        let (ahead, behind) = repo.graph_ahead_behind(original_oid, remote_oid)
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("计算 ahead/behind 失败: {}", e)))?;
        if ahead > 0 {
            if behind > 0 {
                return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                    "无法快进合并：分支已分叉，本地领先 {} 个提交，落后 {} 个提交", ahead, behind
                )));
            } else {
                return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                    "无法快进合并：本地分支有 {} 个未推送的提交", ahead
                )));
            }
        }

        // Update ref first, then checkout. If checkout fails, rollback the ref.
        // This ensures the ref always points to a valid commit.
        local_ref.set_target(remote_oid, "pull-safe: fast-forward")
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("Update local branch reference failed: {}", e)))?;

        if let Err(e) = repo.checkout_tree(&remote_obj, None) {
            // Rollback: restore ref to original OID
            if let Err(e2) = local_ref.set_target(original_oid, "pull-safe: rollback failed checkout") {
                return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                    "CRITICAL: Checkout failed ({}), and rollback ref also failed ({}). Repository may be in an inconsistent state.",
                    e, e2
                )));
            }
            // Restore working directory to original state
            let original_obj = repo.find_object(original_oid, None)
                .map_err(|e3| GetLatestRepoError::Other(anyhow::anyhow!(
                    "CRITICAL: Cannot find original commit object for working directory restore: {}", e3
                )))?;
            if let Err(e3) = repo.checkout_tree(&original_obj, None) {
                return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                    "CRITICAL: Checkout failed ({}), ref rolled back but working directory restore also failed ({}). Repository may be in an inconsistent state.",
                    e, e3
                )));
            }
            return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                "Checkout remote changes failed: {}. Branch reference and working directory have been restored to the original state.",
                e
            )));
        }

        Ok(())
    }

    /// Force pull: stash → pull → pop
    /// Returns PullForceOutcome
    pub fn pull_force(path: &Path) -> Result<PullForceOutcome> {
        let mut repo = Self::open(path)?;
        let stash_name = format!("getlatestrepo-before-pull-{}", 
            chrono::Local::now().format("%Y%m%d-%H%M%S"));
        
        // Check for local changes
        let (is_dirty, _) = Self::check_dirty(&repo)?;
        let stash_created = if is_dirty {
            // 1. Stash local changes
            let sig = repo.signature()?;
            repo.stash_save(
                &sig,
                &stash_name,
                Some(git2::StashFlags::INCLUDE_UNTRACKED)
            )?;
            true
        } else {
            false
        };

        // 2. Pull (ff-only, safest)
        let pull_result = (|| -> Result<()> {
            let branch = Self::get_current_branch(&repo)?;
            let branch_name = match branch {
                Some(name) => name,
                None => return Err(GetLatestRepoError::DetachedHead),
            };
            {
                let remote_branch = format!("origin/{}", branch_name);
                let remote_ref = repo.find_reference(&format!("refs/remotes/{}", remote_branch))?;
                let remote_oid = remote_ref.target().context("无法获取远程分支 OID")?;
                
                let mut local_ref = repo.find_reference(&format!("refs/heads/{}", branch_name))?;
                
                // Save original OID for potential rollback
                let original_oid = local_ref.target()
                    .ok_or_else(|| GetLatestRepoError::Other(anyhow::anyhow!("无法获取当前分支 OID")))?;

                // 安全检查：验证是否为 fast-forward，防止丢失本地未推送的提交
                let (ahead, behind) = repo.graph_ahead_behind(original_oid, remote_oid)
                    .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("计算 ahead/behind 失败: {}", e)))?;
                if ahead > 0 {
                    if behind > 0 {
                        return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                            "无法快进合并：分支已分叉，本地领先 {} 个提交，落后 {} 个提交。请先处理本地提交后再执行 pull-force", ahead, behind
                        )));
                    } else {
                        return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                            "无法快进合并：本地分支有 {} 个未推送的提交。请先推送或处理本地提交后再执行 pull-force", ahead
                        )));
                    }
                }

                // Update ref first, then checkout. If checkout fails, rollback the ref.
                let remote_obj = repo.find_object(remote_oid, None)?;
                local_ref.set_target(remote_oid, "pull-force: fast-forward")
                    .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("更新本地分支引用失败: {}", e)))?;

                if let Err(e) = repo.checkout_tree(&remote_obj, None) {
                    // Rollback: restore ref to original OID
                    if let Err(e2) = local_ref.set_target(original_oid, "pull-force: rollback failed checkout") {
                        return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                            "严重错误：检出失败 ({}), 回滚引用也失败 ({})。仓库可能处于不一致状态。",
                            e, e2
                        )));
                    }
                    return Err(GetLatestRepoError::Other(anyhow::anyhow!(
                        "检出远程变更失败: {}。分支引用已恢复至原始状态。",
                        e
                    )));
                }
            }
            Ok(())
        })();

        match pull_result {
            Ok(()) => {
                // 3. If stash exists, attempt pop
                if stash_created {
                    match repo.stash_pop(0, None) {
                        Ok(()) => Ok(PullForceOutcome::Success),
                        Err(_) => {
                            // Pop failed, collect conflict details for manual resolution
                            let conflict_files = Self::get_conflict_files(&mut repo);
                            let stash_index = Self::find_stash_index(&mut repo, &stash_name);
                            Ok(PullForceOutcome::Conflict {
                                stash_name,
                                conflict_files,
                                stash_index,
                            })
                        }
                    }
                } else {
                    Ok(PullForceOutcome::Success)
                }
            }
            Err(e) => {
                // Pull failed after stash was created — warn user about the orphan stash
                if stash_created {
                    eprintln!("   ⚠️ Pull failed, but local changes were saved to stash: {}", stash_name);
                    eprintln!("      You can restore them manually with: git stash pop stash@{{0}}");
                }
                Err(e)
            }
        }
    }

    /// Get conflicted files after a failed stash pop
    fn get_conflict_files(repo: &mut git2::Repository) -> Vec<String> {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false);
        match repo.statuses(Some(&mut opts)) {
            Ok(statuses) => statuses.iter()
                .filter(|entry| entry.status().contains(git2::Status::CONFLICTED))
                .filter_map(|entry| entry.path().map(|s| s.to_string()))
                .collect(),
            Err(e) => {
                eprintln!("Warning: failed to get conflict files: {}", e);
                Vec::new()
            }
        }
    }

    /// Find stash index by message
    fn find_stash_index(repo: &mut git2::Repository, stash_name: &str) -> Option<usize> {
        let mut result = None;
        if let Err(e) = repo.stash_foreach(|index, message, _oid| {
            if message == stash_name {
                result = Some(index);
                false // stop iterating
            } else {
                true
            }
        }) {
            eprintln!("Warning: failed to iterate stashes: {}", e);
        }
        result
    }

    /// Get recent N commits (used to display new commits after pull)
    pub fn get_recent_commits(path: &Path, count: usize) -> Result<Vec<String>> {
        let repo = Self::open(path)?;
        let mut commits = Vec::new();
        
        let mut revwalk = repo.revwalk()?;
        revwalk.push_head()?;
        
        for oid in revwalk.take(count) {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            
            let msg = commit.message()
                .map(|m| m.lines().next().unwrap_or(m).to_string())
                .unwrap_or_else(|| "(no message)".to_string());
            
            let oid_str = oid.to_string();
            let short_id = if oid_str.len() >= 7 {
                &oid_str[..7]
            } else {
                &oid_str
            };
            commits.push(format!("{} {}", short_id.dimmed(), msg));
        }
        
        Ok(commits)
    }

    /// Discard all local changes (git restore .)
    /// 
    /// # Warning
    /// This operation will permanently lose all uncommitted changes, including:
    /// - Working directory changes
    /// - Staged changes  
    /// - Untracked files (if include_untracked=true)
    ///
    /// # Parameters
    /// - `path`: Repository path
    /// - `include_untracked`: Whether to also delete untracked files
    pub fn discard_changes(path: &Path, include_untracked: bool) -> Result<Vec<String>> {
        let repo = Self::open(path)?;
        
        // Get current status to return the list of discarded files
        let mut status_opts = git2::StatusOptions::new();
        status_opts.include_untracked(include_untracked);
        let statuses = repo.statuses(Some(&mut status_opts))?;
        let mut discarded_files = Vec::new();
        
        for entry in statuses.iter() {
            if let Some(path) = entry.path() {
                discarded_files.push(path.to_string());
            }
        }
        
        // Get HEAD tree
        let head = repo.head()?;
        let head_tree = head.peel_to_tree()?;
        
        // Execute checkout to restore working directory to HEAD state
        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts
            .force()
            .remove_untracked(include_untracked)
            .remove_ignored(false);
        
        repo.checkout_tree(head_tree.as_object(), Some(&mut checkout_opts))?;
        
        // Reset staging area
        let head_commit = head.peel_to_commit()?;
        repo.reset(head_commit.as_object(), git2::ResetType::Mixed, None)?;
        
        Ok(discarded_files)
    }

    /// Check remote repository for anomalies (detect deletion or emptying)
    ///
    /// Use `graph_ahead_behind` O(1) comparison instead of revwalk counting,
    /// detecting whether remote history was force-pushed back.
    pub fn check_pull_safety(path: &Path) -> Result<PullSafetyReport> {
        let repo = Self::open(path)?;

        let branch = Self::get_current_branch(&repo)?;
        let branch_name = match branch {
            Some(b) => b,
            None => return Err(crate::error::GetLatestRepoError::DetachedHead),
        };

        // Get current remote HEAD
        let remote_ref_name = format!("refs/remotes/origin/{}", branch_name);
        let current_oid = match repo.find_reference(&remote_ref_name) {
            Ok(r) => match r.target() {
                Some(oid) => oid,
                None => return Err(crate::error::GetLatestRepoError::RemoteBranchNoTarget),
            },
            Err(_) => {
                return Ok(PullSafetyReport {
                    is_safe: false,
                    remote_commits: 0,
                    previous_remote_commits: 0,
                    change_ratio: 0.0,
                    warning: Some("Remote branch does not exist, please run fetch first".to_string()),
                    details: vec![],
                });
            }
        };

        // Get previous remote OID from reflog at last fetch
        let previous_oid = Self::previous_remote_oid(&repo, &remote_ref_name);

        let mut details = vec![];

        if let Some(prev_oid) = previous_oid {
            if prev_oid == current_oid {
                // No changes
                return Ok(PullSafetyReport {
                    is_safe: true,
                    remote_commits: 0,
                    previous_remote_commits: 0,
                    change_ratio: 0.0,
                    warning: None,
                    details: vec!["远程无新提交".to_string()],
                });
            }

            // O(1) ahead/behind comparison
            let (ahead, behind) = repo.graph_ahead_behind(current_oid, prev_oid)?;
            details.push(format!("新增 {} 个提交，丢失 {} 个提交", ahead, behind));

            if behind > 0 && behind > ahead {
                // Remote history regression
                let total = ahead + behind;
                let regression_ratio = if total > 0 {
                    behind as f64 / total as f64
                } else {
                    1.0
                };

                if regression_ratio > 0.5 {
                    return Ok(PullSafetyReport {
                        is_safe: false,
                        remote_commits: ahead,
                        previous_remote_commits: behind + ahead,
                        change_ratio: -regression_ratio,
                        warning: Some(format!(
                            "检测到疑似仓库删除！远程历史回退：丢失 {} 个提交，仅新增 {} 个提交",
                            behind, ahead,
                        )),
                        details,
                    });
                } else if regression_ratio > 0.2 {
                    return Ok(PullSafetyReport {
                        is_safe: true,
                        remote_commits: ahead,
                        previous_remote_commits: behind + ahead,
                        change_ratio: -regression_ratio,
                        warning: Some(format!(
                            "远程提交数减少：丢失 {} 个提交，新增 {} 个提交",
                            behind, ahead,
                        )),
                        details,
                    });
                }
            }

            // ahead > behind -> normal forward
            if ahead > 0 {
                details.push(format!("远程新增 {} 个提交（正常更新）", ahead));
            }

            Ok(PullSafetyReport {
                is_safe: true,
                remote_commits: ahead,
                previous_remote_commits: ahead + behind,
                change_ratio: 0.0,
                warning: None,
                details,
            })
        } else {
            // No reflog, cannot compare — treat as safe but with a warning
            details.push("首次获取，无历史数据用于比对".to_string());
            Ok(PullSafetyReport {
                is_safe: true,
                remote_commits: 0,
                previous_remote_commits: 0,
                change_ratio: 0.0,
                warning: Some("首次获取 — 无基准数据，无法检测异常".to_string()),
                details,
            })
        }
    }

    /// Get previous remote OID from reflog at last fetch
    ///
    /// Entry 0's `id_old()` is the state before the most recent update.
    /// Falls back to searching older entries if entry 0's `id_old()` is zero.
    fn previous_remote_oid(repo: &GitRepository, ref_name: &str) -> Option<git2::Oid> {
        let reflog = repo.reflog(ref_name).ok()?;
        if reflog.is_empty() {
            return None;
        }
        let current_oid = reflog.get(0)?.id_new();
        // Entry 0's id_old() is the state before the most recent fetch
        let old_oid = reflog.get(0)?.id_old();
        if !old_oid.is_zero() && old_oid != current_oid {
            return Some(old_oid);
        }
        // Fallback: search older entries for any non-current OID
        for i in 1..reflog.len() {
            let entry = reflog.get(i)?;
            let new_oid = entry.id_new();
            if !new_oid.is_zero() && new_oid != current_oid {
                return Some(new_oid);
            }
        }
        None
    }
}

/// Pull safety check report
/// 
/// Note: some fields are currently only used for debugging, reserved for future detailed reporting
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PullSafetyReport {
    /// Whether safe (can pull)
    pub is_safe: bool,
    /// Number of new commits on remote (ahead of local)
    pub remote_commits: usize,
    /// Total commits involved in the change (ahead + behind)
    pub previous_remote_commits: usize,
    /// Change ratio (reserved for debugging)
    pub change_ratio: f64,
    /// Warning message (if any)
    pub warning: Option<String>,
    /// Detailed description (reserved for detailed reporting)
    pub details: Vec<String>,
}

/// Format time difference into human-readable format
pub fn format_duration(dt: &Option<chrono::DateTime<chrono::Local>>) -> String {
    match dt {
        Some(dt) => {
            let now = chrono::Local::now();
            let duration = now.signed_duration_since(*dt);
            
            if duration.num_minutes() < 1 {
                "刚刚".to_string()
            } else if duration.num_hours() < 1 {
                format!("{} 分钟前", duration.num_minutes())
            } else if duration.num_days() < 1 {
                format!("{} 小时前", duration.num_hours())
            } else if duration.num_days() < 30 {
                format!("{} 天前", duration.num_days())
            } else if duration.num_days() < 365 {
                format!("{} 个月前", duration.num_days() / 30)
            } else {
                format!("{} 年前", duration.num_days() / 365)
            }
        }
        None => "-".to_string(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a repo with main branch and origin/main tracking ref at same commit
    fn create_repo_with_tracking() -> (TempDir, std::path::PathBuf, git2::Oid) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = git2::Repository::init(&path).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();

        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let c1 = repo.commit(Some("HEAD"), &sig, &sig, "c1", &tree, &[]).unwrap();

        repo.branch("main", &repo.find_commit(c1).unwrap(), false).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.reference("refs/remotes/origin/main", c1, true, "tracking").unwrap();

        (tmp, path, c1)
    }

    /// Helper: add a commit on current branch (HEAD)
    fn add_commit(path: &std::path::Path, message: &str) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parent = repo.head().unwrap().target().unwrap();
        let parent_commit = repo.find_commit(parent).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent_commit]).unwrap()
    }

    /// Helper: add a commit from a specific parent (detached, not on HEAD)
    fn add_commit_from_parent(path: &std::path::Path, parent_oid: git2::Oid, message: &str) -> git2::Oid {
        let repo = git2::Repository::open(path).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let parent_commit = repo.find_commit(parent_oid).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(None, &sig, &sig, message, &tree, &[&parent_commit]).unwrap()
    }

    /// Helper: move refs/remotes/origin/main to target commit
    fn move_tracking_ref(path: &std::path::Path, target_oid: git2::Oid) {
        let repo = git2::Repository::open(path).unwrap();
        repo.reference("refs/remotes/origin/main", target_oid, true, "update tracking").unwrap();
    }

    #[test]
    fn test_pull_ff_only_behind_remote_succeeds() {
        let (_tmp, path, c1) = create_repo_with_tracking();
        let c2 = add_commit_from_parent(&path, c1, "remote commit");
        move_tracking_ref(&path, c2);
        let result = GitOps::pull_ff_only(&path);
        assert!(result.is_ok(), "Expected success when local is behind remote, got: {:?}", result);
    }

    #[test]
    fn test_pull_ff_only_up_to_date_succeeds() {
        let (_tmp, path, _c1) = create_repo_with_tracking();
        let result = GitOps::pull_ff_only(&path);
        assert!(result.is_ok(), "Expected success when up to date, got: {:?}", result);
    }

    #[test]
    fn test_pull_ff_only_ahead_of_remote_fails() {
        let (_tmp, path, _c1) = create_repo_with_tracking();
        add_commit(&path, "local commit");
        let result = GitOps::pull_ff_only(&path);
        assert!(result.is_err(), "Expected failure when local is ahead of remote");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("无法快进合并") || err_msg.contains("分叉") || err_msg.contains("未推送"),
            "Error message should mention fast-forward or diverged, got: {}", err_msg
        );
    }

    #[test]
    fn test_pull_ff_only_diverged_fails() {
        let (_tmp, path, c1) = create_repo_with_tracking();
        add_commit(&path, "local commit");
        let c2_remote = add_commit_from_parent(&path, c1, "remote commit");
        move_tracking_ref(&path, c2_remote);
        let result = GitOps::pull_ff_only(&path);
        assert!(result.is_err(), "Expected failure when branches diverged");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("无法快进合并") || err_msg.contains("分叉") || err_msg.contains("未推送"),
            "Error message should mention fast-forward or diverged, got: {}", err_msg
        );
    }

    #[test]
    fn test_pull_force_behind_remote_succeeds() {
        let (_tmp, path, c1) = create_repo_with_tracking();
        let c2 = add_commit_from_parent(&path, c1, "remote commit");
        move_tracking_ref(&path, c2);
        let result = GitOps::pull_force(&path);
        assert!(result.is_ok(), "Expected success when local is behind remote, got: {:?}", result);
    }

    #[test]
    fn test_pull_force_up_to_date_succeeds() {
        let (_tmp, path, _c1) = create_repo_with_tracking();
        let result = GitOps::pull_force(&path);
        assert!(result.is_ok(), "Expected success when up to date, got: {:?}", result);
    }

    #[test]
    fn test_pull_force_ahead_of_remote_fails() {
        let (_tmp, path, _c1) = create_repo_with_tracking();
        add_commit(&path, "local commit");
        let result = GitOps::pull_force(&path);
        assert!(result.is_err(), "Expected failure when local is ahead of remote");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("无法快进合并") || err_msg.contains("未推送"),
            "Error message should mention fast-forward, got: {}", err_msg
        );
    }

    #[test]
    fn test_pull_force_diverged_fails() {
        let (_tmp, path, c1) = create_repo_with_tracking();
        add_commit(&path, "local commit");
        let c2_remote = add_commit_from_parent(&path, c1, "remote commit");
        move_tracking_ref(&path, c2_remote);
        let result = GitOps::pull_force(&path);
        assert!(result.is_err(), "Expected failure when branches diverged");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("无法快进合并") || err_msg.contains("分叉") || err_msg.contains("未推送"),
            "Error message should mention fast-forward or diverged, got: {}", err_msg
        );
    }
}
