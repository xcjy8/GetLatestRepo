use anyhow::Context;
use colored::Colorize;
use git2::{BranchType, RemoteCallbacks, Repository as GitRepository, StatusOptions};
use std::path::Path;

use crate::error::{GetLatestRepoError, Result};
use crate::models::{DiffInfo, FileChange, Freshness, Repository};

/// Proxy configuration
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Whether proxy is enabled
    pub enabled: bool,
    /// HTTP proxy address
    pub http_proxy: String,
    /// HTTPS proxy address (reserved for future use)
    #[allow(dead_code)]
    pub https_proxy: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_proxy: "http://127.0.0.1:7890".to_string(),
            https_proxy: "http://127.0.0.1:7890".to_string(),
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
    /// Create default instance (no proxy)
    /// 
    /// Currently unused, prefer using `with_proxy` to create a proxied instance
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            proxy: ProxyConfig::default(),
        }
    }

    /// Create instance with proxy
    pub fn with_proxy(proxy: ProxyConfig) -> Self {
        Self { proxy }
    }

    /// Set proxy configuration
    /// 
    /// Currently unused, prefer using `with_proxy` when creating the instance
    #[allow(dead_code)]
    pub fn set_proxy(&mut self, proxy: ProxyConfig) {
        self.proxy = proxy;
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
            .map(|p| p.components().count())
            .unwrap_or(0) as i32;

        // Get current branch
        let branch = Self::get_current_branch(&repo)?;

        // Check local changes (get detailed info)
        let (dirty, file_changes) = Self::check_dirty(&repo)?;
        // Also generate a path list for database compatibility
        let dirty_files: Vec<String> = file_changes.iter()
            .map(|fc| fc.path.clone())
            .collect();

        // Get upstream info
        let (upstream_ref, upstream_url) = Self::get_upstream_info(&repo)?;

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

    /// Execute fetch and return detailed status
    /// 
    /// Use git2 ProxyOptions to configure proxy per request,
    /// avoiding global state pollution and concurrency safety issues from env::set_var.
    pub fn fetch_detailed(&self, path: &Path, _timeout_secs: u64) -> FetchStatus {
        let repo = match Self::open(path) {
            Ok(r) => r,
            Err(e) => return FetchStatus::OtherError { 
                message: format!("Unable to open repository: {}", e) 
            },
        };

        let mut remote = match repo.find_remote("origin") {
            Ok(r) => r,
            Err(_) => {
                let remotes = match repo.remotes() {
                    Ok(r) => r,
                    Err(e) => return FetchStatus::OtherError { 
                        message: format!("Unable to get remote list: {}", e) 
                    },
                };
                let Some(first) = remotes.get(0) else {
                    return FetchStatus::OtherError { 
                        message: "No remote repository configured".to_string() 
                    };
                };
                match repo.find_remote(first) {
                    Ok(r) => r,
                    Err(e) => return FetchStatus::OtherError { 
                        message: format!("Unable to find remote: {}", e) 
                    },
                }
            }
        };

        // Set timeout and callbacks
        let mut callbacks = RemoteCallbacks::new();
        callbacks.sideband_progress(|_data| true);

        let mut fetch_opts = git2::FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        
        // Configure proxy options (per-request, no global state pollution)
        let mut proxy_opts = git2::ProxyOptions::new();
        if self.proxy.enabled {
            // Use specified proxy URL
            proxy_opts.url(&self.proxy.http_proxy);
        } else {
            // Don't disable proxy, let libgit2 use default behavior (may read from environment variables)
            proxy_opts.auto();
        }
        fetch_opts.proxy_options(proxy_opts);

        // Execute fetch
        if let Err(e) = remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None) {
            return Self::classify_error(&e.to_string());
        }

        FetchStatus::Success
    }

    /// Classify error type
    fn classify_error(error_msg: &str) -> FetchStatus {
        let msg = error_msg.to_lowercase();
        
        // Authentication-related errors
        if msg.contains("401") || msg.contains("403") || 
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
        if msg.contains("timeout") || msg.contains("timed out") ||
           msg.contains("connection refused") || msg.contains("couldn't connect") ||
           msg.contains("network") || msg.contains("unreachable") {
            return FetchStatus::NetworkError { 
                message: error_msg.to_string() 
            };
        }
        
        // Other errors
        FetchStatus::OtherError { 
            message: error_msg.to_string() 
        }
    }

    /// Fetch repository (legacy interface)
    /// 
    /// Currently unused, prefer using `fetch_detailed` for detailed results
    #[allow(dead_code)]
    pub fn fetch(&self, path: &Path, timeout_secs: u64) -> Result<bool> {
        match self.fetch_detailed(path, timeout_secs) {
            FetchStatus::Success => Ok(true),
            _ => Ok(false),
        }
    }

    /// Get file change details
    /// 
    /// Currently unused, reserved for future `status` command extension
    #[allow(dead_code)]
    pub fn get_file_changes(path: &Path) -> Result<Vec<FileChange>> {
        let repo = Self::open(path)?;
        let mut opts = StatusOptions::new();
        opts.include_untracked(true);

        let statuses = repo.statuses(Some(&mut opts))?;
        let mut changes = Vec::new();

        for entry in statuses.iter() {
            if let Some(path) = entry.path() {
                let status = entry.status();
                
                let status_str = if status.contains(git2::Status::INDEX_NEW) || 
                                    status.contains(git2::Status::WT_NEW) {
                    "added"
                } else if status.contains(git2::Status::INDEX_DELETED) || 
                          status.contains(git2::Status::WT_DELETED) {
                    "deleted"
                } else if status.contains(git2::Status::INDEX_RENAMED) || 
                          status.contains(git2::Status::WT_RENAMED) {
                    "renamed"
                } else {
                    "modified"
                };

                let staged = status.intersects(
                    git2::Status::INDEX_NEW |
                    git2::Status::INDEX_MODIFIED |
                    git2::Status::INDEX_DELETED |
                    git2::Status::INDEX_RENAMED
                );

                changes.push(FileChange::new(
                    path.to_string(),
                    status_str,
                    staged
                ));
            }
        }

        Ok(changes)
    }

    /// Get diff content (simplified)
    /// 
    /// Currently unused, reserved for future diff display functionality
    #[allow(dead_code)]
    pub fn get_diff(path: &Path, max_files: usize) -> Result<Vec<DiffInfo>> {
        let repo = Self::open(path)?;
        let mut diff_infos = Vec::new();

        // Get working directory diff
        let head_tree = repo.head().ok()
            .and_then(|h| h.peel_to_tree().ok());

        let mut diff_opts = git2::DiffOptions::new();
        let diff = repo.diff_tree_to_workdir(head_tree.as_ref(), Some(&mut diff_opts))?;

        let mut count = 0;
        diff.foreach(
            &mut |delta, _| {
                if count >= max_files {
                    return false;
                }
                
                let file_path = delta.new_file().path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let old_path = delta.old_file().path()
                    .filter(|_| delta.status() == git2::Delta::Renamed)
                    .map(|p| p.to_string_lossy().to_string());

                let status = match delta.status() {
                    git2::Delta::Added => "added",
                    git2::Delta::Deleted => "deleted",
                    git2::Delta::Modified => "modified",
                    git2::Delta::Renamed => "renamed",
                    _ => "modified",
                };

                diff_infos.push(DiffInfo {
                    file_path,
                    old_path,
                    status: status.to_string(),
                    diff_content: String::new(), // Simplified version doesn't get specific content
                });

                count += 1;
                true
            },
            None, None, None,
        )?;

        Ok(diff_infos)
    }
}

/// Pull force execution results
#[derive(Debug, Clone)]
pub enum PullForceOutcome {
    /// Success (clean repository directly pulled, or dirty repository stash-pull-pop succeeded)
    Success,
    /// Stash pop conflict (pull succeeded, but pop failed)
    /// Parameter is the stash name
    Conflict(String),
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
        repo.checkout_tree(&remote_obj, None)
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("Checkout remote changes failed: {}", e)))?;
        local_ref.set_target(remote_oid, "pull-safe: fast-forward")
            .map_err(|e| GetLatestRepoError::Other(anyhow::anyhow!("Update local branch reference failed: {}", e)))?;
        
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
        let branch = Self::get_current_branch(&repo)?;
        if let Some(ref branch_name) = branch {
            let remote_branch = format!("origin/{}", branch_name);
            let remote_ref = repo.find_reference(&format!("refs/remotes/{}", remote_branch))?;
            let remote_oid = remote_ref.target().context("Unable to get remote branch OID")?;
            
            let mut local_ref = repo.find_reference(&format!("refs/heads/{}", branch_name))?;
            
            // Try fast-forward merge
            repo.checkout_tree(&repo.find_object(remote_oid, None)?, None)?;
            local_ref.set_target(remote_oid, "pull-force: fast-forward")?;
        }

        // 3. If stash exists, attempt pop
        if stash_created {
            match repo.stash_pop(0, None) {
                Ok(()) => Ok(PullForceOutcome::Success),
                Err(_) => {
                    // Pop failed, returning Conflict for manual resolution
                    Ok(PullForceOutcome::Conflict(stash_name))
                }
            }
        } else {
            Ok(PullForceOutcome::Success)
        }
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
        let statuses = repo.statuses(None)?;
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
                    details: vec!["Remote no new commits".to_string()],
                });
            }

            // O(1) ahead/behind comparison
            let (ahead, behind) = repo.graph_ahead_behind(current_oid, prev_oid)?;
            details.push(format!("Added: {} commits | Lost: {} commits", ahead, behind));

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
                            "Potential repo deletion detected! Remote history regression: lost {} commits, only added {} commits",
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
                            "Remote commit count decreased: lost {} commits, added {} commits",
                            behind, ahead,
                        )),
                        details,
                    });
                }
            }

            // ahead > behind -> normal forward
            if ahead > 0 {
                details.push(format!("Remote added {} commits (normal update)", ahead));
            }
        } else {
            // No reflog, cannot compare
            details.push("First fetch, no historical data to compare".to_string());
        }

        Ok(PullSafetyReport {
            is_safe: true,
            remote_commits: 0,
            previous_remote_commits: 0,
            change_ratio: 0.0,
            warning: None,
            details,
        })
    }

    /// Get previous remote OID from reflog at last fetch
    fn previous_remote_oid(repo: &GitRepository, ref_name: &str) -> Option<git2::Oid> {
        let reflog = repo.reflog(ref_name).ok()?;
        if reflog.len() < 2 {
            return None;
        }
        let entry = reflog.get(reflog.len() - 2)?;
        let oid = entry.id_old();
        if oid.is_zero() {
            None
        } else {
            Some(oid)
        }
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
    /// Current remote commit count (reserved for debugging)
    pub remote_commits: usize,
    /// Previous remote commit count at last fetch (reserved for debugging)
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
                "Just now".to_string()
            } else if duration.num_hours() < 1 {
                format!("{} minutes ago", duration.num_minutes())
            } else if duration.num_days() < 1 {
                format!("{} hours ago", duration.num_hours())
            } else if duration.num_days() < 30 {
                format!("{} days ago", duration.num_days())
            } else if duration.num_days() < 365 {
                format!("{} months ago", duration.num_days() / 30)
            } else {
                format!("{} years ago", duration.num_days() / 365)
            }
        }
        None => "-".to_string(),
    }
}
