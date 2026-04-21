use thiserror::Error;

/// The unified entry point for all GetLatestRepo errors.
///
/// Each module returns `Result<T, GetLatestRepoError>` instead of `anyhow::Error`,
/// so callers can match error types precisely for differentiated handling.
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum GetLatestRepoError {
    // ── I/O / Paths ───────────────────────────────────────────────
    #[error("Path does not exist: {0}")]
    PathNotFound(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Repository path does not exist: {0}")]
    RepoPathMissing(String),

    // ── Git Operations ────────────────────────────────────────────
    #[error("Not a valid Git repository: {0}")]
    NotGitRepo(String),

    #[error("Failed to open repository {path}: {source}")]
    OpenRepo {
        path: String,
        source: git2::Error,
    },

    #[error("Authentication required (401/403): {0}")]
    AuthRequired(String),

    #[error("Repository not found or made private (404): {0}")]
    RepoNotFound(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Currently not on any branch")]
    DetachedHead,

    #[error("Remote branch does not exist, please run fetch first")]
    RemoteBranchMissing,

    #[error("Remote branch has no target commit")]
    RemoteBranchNoTarget,

    #[error("Git operation failed: {0}")]
    GitOperation(#[from] git2::Error),

    // ── Pull safety ───────────────────────────────────────────────
    #[error("Potential repo deletion detected: {detail}")]
    RepoDeletionRisk { detail: String },

    #[error("Safety check failed: {source}")]
    SecurityCheckFailed { source: anyhow::Error },

    #[error("Security scan failed, skipped")]
    SecurityScanFailed,

    #[error("User cancelled")]
    UserCancelled,

    // ── Database ──────────────────────────────────────────────────
    #[error("Database operation failed: {0}")]
    Database(#[from] rusqlite::Error),

    // ── Scan ──────────────────────────────────────────────────────
    #[error("Scan path does not exist: {0}")]
    ScanPathMissing(String),

    #[error("No repositories found")]
    NoRepos,

    #[error("No enabled scan sources")]
    NoSources,

    // ── Config ────────────────────────────────────────────────────
    #[error("Not initialized. Please run: getlatestrepo init <path>")]
    NotInitialized,

    #[error("Path already exists: {0}")]
    DuplicatePath(String),

    #[error("No matching scan source found: {0}")]
    SourceNotFound(String),

    // ── General IO ────────────────────────────────────────────────
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WalkDir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Convenience type alias
pub type Result<T> = std::result::Result<T, GetLatestRepoError>;

/// Convert from FetchStatus to GetLatestRepoError
/// 
/// Only error statuses should be converted; Success should not be converted.
/// The caller should check for Success before converting.
impl TryFrom<crate::git::FetchStatus> for GetLatestRepoError {
    type Error = anyhow::Error;

    fn try_from(status: crate::git::FetchStatus) -> std::result::Result<Self, Self::Error> {
        use crate::git::FetchStatus;
        match status {
            FetchStatus::AuthenticationRequired { message } => Ok(GetLatestRepoError::AuthRequired(message)),
            FetchStatus::RepositoryNotFound { message } => Ok(GetLatestRepoError::RepoNotFound(message)),
            FetchStatus::NetworkError { message } => Ok(GetLatestRepoError::Network(message)),
            FetchStatus::OtherError { message } => Ok(GetLatestRepoError::Other(anyhow::anyhow!(message))),
            FetchStatus::Success => Err(anyhow::anyhow!(
                "Cannot convert FetchStatus::Success to GetLatestRepoError, please check status before converting"
            )),
        }
    }
}
