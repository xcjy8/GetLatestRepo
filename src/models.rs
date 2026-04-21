use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// Repository freshness status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Freshness {
    /// Has remote updates
    HasUpdates,
    /// Synced
    Synced,
    /// Remote unreachable
    Unreachable,
    /// No upstream branch
    NoRemote,
}

impl Freshness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Freshness::HasUpdates => "has_updates",
            Freshness::Synced => "synced",
            Freshness::Unreachable => "unreachable",
            Freshness::NoRemote => "no_remote",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Freshness::HasUpdates => "🔴",
            Freshness::Synced => "🟢",
            Freshness::Unreachable => "⚫",
            Freshness::NoRemote => "⚪",
        }
    }

    /// Get status label (for internal identification)
    /// 
    /// Currently unused, reserved for future CLI filtering functionality
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Freshness::HasUpdates => "behind",
            Freshness::Synced => "ok",
            Freshness::Unreachable => "error",
            Freshness::NoRemote => "no remote",
        }
    }
}

impl From<&str> for Freshness {
    fn from(s: &str) -> Self {
        match s {
            "has_updates" => Freshness::HasUpdates,
            "synced" => Freshness::Synced,
            "unreachable" => Freshness::Unreachable,
            "no_remote" => Freshness::NoRemote,
            _ => Freshness::NoRemote,
        }
    }
}

/// Scan source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSource {
    pub id: Option<i64>,
    pub root_path: String,
    pub max_depth: usize,
    pub ignore_patterns: Vec<String>,
    pub follow_symlinks: bool,
    pub enabled: bool,
    pub last_scan_at: Option<DateTime<Local>>,
}

impl Default for ScanSource {
    fn default() -> Self {
        Self {
            id: None,
            root_path: String::new(),
            max_depth: 5,
            ignore_patterns: vec![".git".to_string(), "node_modules".to_string(), "target".to_string()],
            follow_symlinks: false,
            enabled: true,
            last_scan_at: None,
        }
    }
}

/// Full repository info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: Option<i64>,
    pub path: String,
    pub root_path: String,
    pub name: String,
    pub depth: i32,
    
    // Git Status
    pub branch: Option<String>,
    pub dirty: bool,
    /// Changed file list (detailed metadata)
    #[serde(skip)] // Not serialized to database, regenerated during scan
    pub file_changes: Vec<FileChange>,
    /// Changed file path list (database compatibility, deprecated)
    #[serde(rename = "dirty_files")]
    pub dirty_files: Vec<String>,
    pub upstream_ref: Option<String>,
    pub upstream_url: Option<String>,
    
    // Sync status
    pub ahead_count: i32,
    pub behind_count: i32,
    pub freshness: Freshness,
    
    // Timestamps
    pub last_commit_at: Option<DateTime<Local>>,
    pub last_commit_message: Option<String>,
    pub last_commit_author: Option<String>,
    pub last_scanned_at: Option<DateTime<Local>>,
    pub last_fetch_at: Option<DateTime<Local>>,
    pub last_pull_at: Option<DateTime<Local>>,
}

impl Repository {
    /// Create repository instance with new path (for needauth move scenario)
    /// 
    /// Reuse all other fields, only update path-related info
    pub fn with_new_path(self, new_path: String, new_root_path: String) -> Self {
        Self {
            path: new_path,
            root_path: new_root_path,
            depth: 0, // repository depth in needauth is 0
            ..self
        }
    }

    /// Get change statistics summary
    pub fn change_summary(&self) -> String {
        if self.file_changes.is_empty() {
            return format!("{} changed files", self.dirty_files.len());
        }
        
        let staged = self.file_changes.iter().filter(|fc| fc.staged).count();
        let unstaged = self.file_changes.len() - staged;
        
        if staged > 0 && unstaged > 0 {
            format!("{} staged, {} unstaged", staged, unstaged)
        } else if staged > 0 {
            format!("{} staged", staged)
        } else {
            format!("{} unstaged", unstaged)
        }
    }
}

/// Repository status summary (for quick display)
#[derive(Debug, Clone, Default)]
pub struct RepoSummary {
    pub total: usize,
    pub has_updates: usize,
    pub synced: usize,
    pub unreachable: usize,
    pub no_remote: usize,
    pub dirty: usize,
}

impl RepoSummary {
    pub fn new() -> Self {
        Self {
            total: 0,
            has_updates: 0,
            synced: 0,
            unreachable: 0,
            no_remote: 0,
            dirty: 0,
        }
    }

    pub fn add(&mut self, repo: &Repository) {
        self.total += 1;
        match repo.freshness {
            Freshness::HasUpdates => self.has_updates += 1,
            Freshness::Synced => self.synced += 1,
            Freshness::Unreachable => self.unreachable += 1,
            Freshness::NoRemote => self.no_remote += 1,
        }
        if repo.dirty {
            self.dirty += 1;
        }
    }
}

/// Fetch task results
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub repo_path: String,
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
    /// Number of retries performed for network errors
    pub retry_count: u32,
}

/// File change info
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    /// File path
    pub path: String,
    /// Change status: modified, added, deleted, renamed, typechange
    pub status: String,
    /// Whether in staging area
    pub staged: bool,
    /// Change impact description (for display)
    pub impact: String,
    /// Predicted result after executing stash
    pub stash_effect: String,
}

impl FileChange {
    /// Create file change info
    pub fn new(path: impl Into<String>, status: impl Into<String>, staged: bool) -> Self {
        let path = path.into();
        let status = status.into();
        let (impact, stash_effect) = Self::describe_change(&status, staged);
        
        Self {
            path,
            status,
            staged,
            impact,
            stash_effect,
        }
    }

    /// Describe change impact and stash effect
    fn describe_change(status: &str, staged: bool) -> (String, String) {
        let stage_info = if staged { "(staged) " } else { "" };
        
        match status {
            "added" => {
                let impact = format!("{}New file, will be committed", stage_info);
                let stash = "After stash: file disappears, restored after pop (new file needs to be re-added)";
                (impact, stash.to_string())
            }
            "modified" => {
                let impact = format!("{}Content modified", stage_info);
                let stash = "After stash: changes disappear, restored after pop";
                (impact, stash.to_string())
            }
            "deleted" => {
                let impact = format!("{}File deleted", stage_info);
                let stash = "After stash: file restored, re-deleted after pop";
                (impact, stash.to_string())
            }
            "renamed" => {
                let impact = format!("{}File renamed", stage_info);
                let stash = "After stash: original filename restored, renamed after pop";
                (impact, stash.to_string())
            }
            "untracked" => {
                let impact = "Untracked new file (won't be committed)".to_string();
                let stash = "After stash -u: file disappears, restored after pop";
                (impact, stash.to_string())
            }
            "ignored" => {
                let impact = "File ignored by .gitignore".to_string();
                let stash = "Stash does not affect this file".to_string();
                (impact, stash.to_string())
            }
            _ => {
                let impact = format!("{}Unknown change", stage_info);
                let stash = "Stash effect unknown, recommend manual check";
                (impact, stash.to_string())
            }
        }
    }
}

/// Diff content
/// 
/// Currently unused, reserved for future diff display functionality
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct DiffInfo {
    pub file_path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub diff_content: String,
}
