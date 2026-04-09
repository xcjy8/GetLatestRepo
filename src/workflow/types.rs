use colored::*;
use std::path::PathBuf;

use crate::cli::OutputFormat;
use crate::models::RepoSummary;

// ==================== Workflow definition ====================

/// Workflow definition
#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub steps: Vec<WorkflowStep>,
    pub default_jobs: usize,
    pub default_timeout: u64,
}

/// Workflow step
#[derive(Debug, Clone)]
pub enum WorkflowStep {
    /// Fetch all repositories
    Fetch { jobs: Option<usize>, timeout: Option<u64> },
    /// Scan and generate report
    Scan {
        output: OutputFormat,
        open: bool,
        only_dirty_or_behind: bool,
    },
    /// Condition check
    Check { condition: Condition, silent: bool },
    /// Security pull (clean repositories only)
    PullSafe { jobs: Option<usize>, confirm: bool, diff_after: bool },
    /// Force pull (stash → pull → pop)
    PullForce { jobs: Option<usize>, diff_after: bool },
}

/// Check condition
///
/// Note: some conditions are currently unused, reserved for future workflow extension
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Condition {
    /// Has repositories behind remote
    HasBehind,
    /// Has dirty files - currently unused, reserved for future condition checking
    HasDirty,
    /// Has errors - currently unused, reserved for future condition checking
    HasError,
    /// All synced - currently unused, reserved for future condition checking
    AllSynced,
}

// ==================== Built-in workflows ====================

/// Built-in workflows
pub struct BuiltInWorkflows;

impl BuiltInWorkflows {
    /// Get all built-in workflows
    pub fn all() -> Vec<Workflow> {
        vec![
            Self::daily(),
            Self::check(),
            Self::report(),
            Self::ci(),
            Self::pull_safe(),
            Self::pull_force(),
        ]
    }

    /// Daily check
    fn daily() -> Workflow {
        Workflow {
            name: "daily".to_string(),
            description: "Daily check: fetch all repositories and generate terminal report".to_string(),
            steps: vec![
                WorkflowStep::Fetch {
                    jobs: Some(5),
                    timeout: Some(30),
                },
                WorkflowStep::Scan {
                    output: OutputFormat::Terminal,
                    open: false,
                    only_dirty_or_behind: false,
                },
            ],
            default_jobs: 5,
            default_timeout: 30,
        }
    }

    /// Quick view (no fetch)
    fn check() -> Workflow {
        Workflow {
            name: "check".to_string(),
            description: "Quick view: no fetch, only show repositories needing attention".to_string(),
            steps: vec![WorkflowStep::Scan {
                output: OutputFormat::Terminal,
                open: false,
                only_dirty_or_behind: true,
            }],
            default_jobs: 5,
            default_timeout: 30,
        }
    }

    /// Full report
    fn report() -> Workflow {
        Workflow {
            name: "report".to_string(),
            description: "Generate full HTML report and open browser".to_string(),
            steps: vec![
                WorkflowStep::Fetch {
                    jobs: Some(10),
                    timeout: Some(60),
                },
                WorkflowStep::Scan {
                    output: OutputFormat::Html,
                    open: true,
                    only_dirty_or_behind: false,
                },
            ],
            default_jobs: 10,
            default_timeout: 60,
        }
    }

    /// CI check
    fn ci() -> Workflow {
        Workflow {
            name: "ci".to_string(),
            description: "CI check: return error code if there are behind repositories".to_string(),
            steps: vec![
                WorkflowStep::Fetch {
                    jobs: Some(10),
                    timeout: Some(30),
                },
                WorkflowStep::Scan {
                    output: OutputFormat::Markdown,
                    open: false,
                    only_dirty_or_behind: false,
                },
                WorkflowStep::Check {
                    condition: Condition::HasBehind,
                    silent: false,
                },
            ],
            default_jobs: 10,
            default_timeout: 30,
        }
    }

    /// Safe update (only pull clean repositories)
    fn pull_safe() -> Workflow {
        Workflow {
            name: "pull-safe".to_string(),
            description: "Safe update: only pull clean repositories (dirty repos auto-skipped)".to_string(),
            steps: vec![
                WorkflowStep::Fetch {
                    jobs: Some(5),
                    timeout: Some(30),
                },
                WorkflowStep::PullSafe {
                    jobs: Some(5),
                    confirm: true,  // Confirm by default
                    diff_after: false,
                },
            ],
            default_jobs: 5,
            default_timeout: 30,
        }
    }

    /// Force update (stash → pull → pop)
    fn pull_force() -> Workflow {
        Workflow {
            name: "pull-force".to_string(),
            description: "Force update: stash local changes → pull → pop (stop on conflict)".to_string(),
            steps: vec![
                WorkflowStep::Fetch {
                    jobs: Some(5),
                    timeout: Some(30),
                },
                WorkflowStep::PullForce {
                    jobs: Some(5),
                    diff_after: false,
                },
            ],
            default_jobs: 5,
            default_timeout: 30,
        }
    }

    /// Get workflow by name
    pub fn get(name: &str) -> Option<Workflow> {
        Self::all().into_iter().find(|w| w.name == name)
    }
}

// ==================== Result types ====================

/// Dirty repository info
#[derive(Debug, Clone)]
pub struct DirtyRepoInfo {
    /// Repository
    pub name: String,
    /// Repository path
    pub path: String,
    /// Branch name
    pub branch: Option<String>,
    /// Detailed changed file list
    pub file_changes: Vec<crate::models::FileChange>,
}

impl DirtyRepoInfo {
    pub fn new(
        name: impl Into<String>, 
        path: impl Into<String>, 
        branch: Option<String>,
        file_changes: Vec<crate::models::FileChange>
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            branch,
            file_changes,
        }
    }

    /// Get change statistics summary
    pub fn change_summary(&self) -> String {
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

/// Pull-safe results
#[derive(Debug, Clone)]
pub struct PullSafeResult {
    pub total_count: usize,
    pub success_count: usize,
    pub failed_count: usize,
    pub skipped_repos: Vec<String>,  // Already up to date
    pub dirty_repos: Vec<DirtyRepoInfo>,    // Skipped due to local changes (includes file list)
    pub pulled_repos: Vec<(String, Vec<String>)>, // (repository name, new commit list)
}

impl PullSafeResult {
    pub fn new() -> Self {
        Self {
            total_count: 0,
            success_count: 0,
            failed_count: 0,
            skipped_repos: Vec::new(),
            dirty_repos: Vec::new(),
            pulled_repos: Vec::new(),
        }
    }
}

/// Pull-force results
#[derive(Debug, Clone)]
pub struct PullForceResult {
    pub total_count: usize,
    pub success_count: usize,
    pub failed_count: usize,
    pub conflict_repos: Vec<String>,
}

impl PullForceResult {
    pub fn new() -> Self {
        Self {
            total_count: 0,
            success_count: 0,
            failed_count: 0,
            conflict_repos: Vec::new(),
        }
    }

    pub fn has_errors(&self) -> bool {
        self.failed_count > 0 || !self.conflict_repos.is_empty()
    }
}

/// Workflow execution results
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub success: bool,
    pub errors: Vec<String>,
    pub repo_summary: Option<RepoSummary>,
}

impl WorkflowResult {
    pub fn success() -> Self {
        Self {
            success: true,
            errors: Vec::new(),
            repo_summary: None,
        }
    }

    pub fn add_error(&mut self, msg: String) {
        self.errors.push(msg);
        self.success = false;
    }

    pub fn exit_code(&self) -> i32 {
        if self.success { 0 } else { 1 }
    }
}

// ==================== Utility functions ====================

/// List all workflows
pub fn list_workflows() {
    println!("{} Available workflows:\n", "ℹ".blue());

    for workflow in BuiltInWorkflows::all() {
        println!("  {} {}", workflow.name.cyan().bold(), workflow.description.dimmed());
        println!("     Steps: {} | Default concurrency: {} | Timeout: {}s\n",
            workflow.steps.len(),
            workflow.default_jobs,
            workflow.default_timeout
        );
    }

    println!("Usage: getlatestrepo workflow <name>");
    println!("      getlatestrepo workflow daily");
    println!("      getlatestrepo workflow report --jobs 10");
}

/// Open report file
/// 
/// # Security note
/// Use `Command` instead of `system()` to avoid shell injection risks.
/// Path uses `--` argument to stop option parsing, preventing paths starting with `-` from being interpreted as options.
pub fn open_report(path: &std::path::Path) -> anyhow::Result<()> {
    // Ensure path is absolute, avoid relative path resolution issues
    let canonical_path = path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let path_str = canonical_path.to_string_lossy();

    #[cfg(target_os = "macos")]
    {
        // Use `--` to stop option parsing, preventing paths starting with `-` from being interpreted as options
        std::process::Command::new("open")
            .arg("--")
            .arg(&*path_str)
            .spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg("--")
            .arg(&*path_str)
            .spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        // Windows: use explorer command instead of cmd /C start
        // explorer command is safer, doesn't need cmd parsing
        std::process::Command::new("explorer")
            .arg(&*path_str)
            .spawn()?;
    }

    Ok(())
}

/// Ensure reports directory exists and create latest symlink
pub fn ensure_reports_dir(path: &std::path::Path) -> anyhow::Result<()> {
    let latest_link = PathBuf::from("reports/latest.html");

    if let Err(e) = std::fs::remove_file(&latest_link) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("   Warning: Failed to delete old symlink: {}", e);
        }
    }

    let path_str = path.to_string_lossy();
    let relative_path = path_str.to_string();

    #[cfg(unix)]
    {
        if let Err(e) = std::os::unix::fs::symlink(&relative_path, &latest_link) {
            eprintln!("   Warning: Failed to create latest.html symlink: {}", e);
            eprintln!("   Tip: you can manually create symlink: ln -s {} {}", relative_path, latest_link.display());
        }
    }
    #[cfg(windows)]
    {
        if let Err(e) = std::os::windows::fs::symlink_file(&relative_path, &latest_link) {
            eprintln!("   Warning: Failed to create latest.html symlink: {}", e);
            eprintln!("   Tip: Windows requires admin privileges or developer mode to create symlinks");
        }
    }

    Ok(())
}
