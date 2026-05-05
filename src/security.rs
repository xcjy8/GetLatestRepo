use anyhow::Result;
use colored::Colorize;
use git2::{Oid, Repository};
use std::sync::LazyLock;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Pre-compiled sensitive file patterns (global cache)
static SENSITIVE_PATTERNS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        ".gitignore", ".gitmodules", "Cargo.toml", "package.json",
        "requirements.txt", "setup.py", "Makefile", "Dockerfile",
        ".github/workflows", ".gitlab-ci.yml", "build.gradle", "pom.xml", "go.mod",
        // Added: credential and key files
        ".env", ".env.local", ".env.production", ".env.development",
        "*.pem", "*.key", "id_rsa", "id_rsa.pub", "id_ed25519", "id_ed25519.pub",
        ".aws/credentials", ".docker/config.json", "kubeconfig", "*.p12", "*.pfx",
        // Added: CI config files (high risk for supply chain attacks)
        "Jenkinsfile", ".circleci/config.yml", ".travis.yml", "azure-pipelines.yml",
    ]
    .iter()
    .cloned()
    .collect()
});

/// Pre-compiled code file extensions (global cache)
static CODE_EXTENSIONS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "rs", "py", "js", "ts", "java", "go", "c", "cpp", "h", "hpp",
        "rb", "php", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd",
        "pl", "pm", "t", "swift", "kt", "scala", "groovy", "gradle",
        "xml", "json", "yaml", "yml", "toml", "ini", "cfg", "conf",
    ]
    .iter()
    .cloned()
    .collect()
});

/// Pre-compiled suspicious code patterns (global cache, avoid repeated compilation)
static SUSPICIOUS_PATTERNS: LazyLock<Vec<(regex::Regex, &'static str)>> = LazyLock::new(|| {
    // 使用有限量词（如 {0,200}）替代 .* / \s*，显著降低 ReDoS 回溯风险
    let raw: &[(&str, &str)] = &[
        (r"eval\s{0,20}\(", "eval function call"),
        (r"exec\s{0,20}\(", "exec function call"),
        (r"system\s{0,20}\(", "system function call"),
        (r"os\.system", "os.system call"),
        (r"subprocess\.call", "subprocess call"),
        (r"Runtime\.getRuntime\(\)\.exec", "Java Runtime exec"),
        (r"child_process", "Node.js child_process"),
        (r"\bbase64\b.{0,200}\bdecode\b", "Base64 decode (may hide malicious code)"),
        (r"http://[^\s]{0,200}\.onion", "Dark web address"),
        (r"wget\s+http", "wget download"),
        (r"curl\s+\S{0,200}\|\S{0,200}sh", "curl pipe to shell"),
        (r"fetch\([^)]{0,200}\.txt\)[^)]{0,200}eval", "fetch text and eval"),
        (r"document\.write.{0,200}unescape", "document.write + unescape"),
        (r"fromCharCode", "String.fromCharCode (possible obfuscation)"),
    ];
    raw.iter()
        .filter_map(|(pat, desc)| {
            regex::Regex::new(pat).ok().map(|re| (re, *desc))
        })
        .collect()
});

/// SecurityRisk level
/// 
/// Note: some levels are currently unused, reserved for future extension
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RiskLevel {
    /// Safe
    Safe,
    /// Low risk (warning) - currently unused, reserved for future low-sensitivity detection
    Low,
    /// Medium risk (confirmation recommended)
    Medium,
    /// High risk (blocks operation)
    High,
    /// Critical (confirmed danger)
    Critical,
}

impl RiskLevel {
    pub fn emoji(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "✅",
            RiskLevel::Low => "⚡",
            RiskLevel::Medium => "⚠️",
            RiskLevel::High => "🚨",
            RiskLevel::Critical => "☠️",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "Safe",
            RiskLevel::Low => "Low risk",
            RiskLevel::Medium => "Medium risk",
            RiskLevel::High => "High risk",
            RiskLevel::Critical => "Critical danger",
        }
    }

    pub fn should_block(&self) -> bool {
        matches!(self, RiskLevel::High | RiskLevel::Critical)
    }
}

/// Security risk details
#[derive(Debug, Clone)]
pub struct SecurityRisk {
    /// Risk level
    pub level: RiskLevel,
    /// Risk type
    pub risk_type: RiskType,
    /// Risk description
    pub description: String,
    /// Details (file list, committer, etc.)
    pub details: Vec<String>,
}

/// Risk type
/// 
/// Note: some types currently unused, reserved for future safety detection extension
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum RiskType {
    /// Remote repository inaccessible (deleted/private) - currently unused, handled by fetch errors
    RemoteInaccessible,
    /// File count anomaly
    FileCountAnomaly,
    /// Sensitive file changes
    SensitiveFileModified,
    /// Suspicious code pattern
    SuspiciousCodePattern,
    /// Committer anomaly
    CommitterAnomaly,
    /// Signature verification failed - currently unused, reserved for future GPG verification
    SignatureVerificationFailed,
}

/// SecurityScan result
#[derive(Debug, Clone)]
pub struct SecurityScanResult {
    /// Is safe
    pub is_safe: bool,
    /// Risk list
    pub risks: Vec<SecurityRisk>,
    /// Highest risk level
    pub max_level: RiskLevel,
}

/// Security scanner
pub struct SecurityScanner;

impl SecurityScanner {
    /// Execute security scan
    /// 
    /// Called before fetch/pull, detects safety of remote changes
    pub fn scan_before_fetch(
        path: &Path,
        local_oid: Option<Oid>,
        remote_oid: Option<Oid>,
    ) -> Result<SecurityScanResult> {
        let mut risks = Vec::new();
        let repo = Repository::open(path)?;

        // 1. Check if remote is accessible (requires actual fetch to know, skip here for now)
        // Actually handled by caller on fetch failure

        // 2. If local and remote OIDs exist, analyze differences
        if let (Some(local), Some(remote)) = (local_oid, remote_oid) {
            // Check file count changes
            if let Ok(risk) = Self::check_file_count_anomaly(&repo, local, remote)
                && risk.level != RiskLevel::Safe {
                    risks.push(risk);
                }

            // Check sensitive file changes
            if let Ok(risk) = Self::check_sensitive_files(&repo, local, remote)
                && risk.level != RiskLevel::Safe {
                    risks.push(risk);
                }

            // Check suspicious code patterns
            if let Ok(risk) = Self::check_suspicious_patterns(&repo, local, remote)
                && risk.level != RiskLevel::Safe {
                    risks.push(risk);
                }

            // Check committer anomalies
            if let Ok(risk) = Self::check_committer_anomaly(&repo, local, remote)
                && risk.level != RiskLevel::Safe {
                    risks.push(risk);
                }
        }

        let max_level = risks.iter()
            .map(|r| r.level)
            .max_by_key(|l| match l {
                RiskLevel::Critical => 4,
                RiskLevel::High => 3,
                RiskLevel::Medium => 2,
                RiskLevel::Low => 1,
                RiskLevel::Safe => 0,
            })
            .unwrap_or(RiskLevel::Safe);

        Ok(SecurityScanResult {
            is_safe: risks.is_empty() || !max_level.should_block(),
            risks,
            max_level,
        })
    }

    /// Detect file count anomaly
    fn check_file_count_anomaly(repo: &Repository, local: Oid, remote: Oid) -> Result<SecurityRisk> {
        let local_tree = repo.find_commit(local)?.tree()?;
        let remote_tree = repo.find_commit(remote)?.tree()?;

        let local_count = Self::count_files_in_tree(repo, &local_tree)?;
        let remote_count = Self::count_files_in_tree(repo, &remote_tree)?;

        if local_count == 0 {
            return Ok(SecurityRisk {
                level: RiskLevel::Safe,
                risk_type: RiskType::FileCountAnomaly,
                description: String::new(),
                details: Vec::new(),
            });
        }

        let change_ratio = (remote_count as f64 - local_count as f64) / local_count as f64;

        // File decrease > 50% - possible repo deletion
        if change_ratio < -0.5 {
            return Ok(SecurityRisk {
                level: RiskLevel::High,
                risk_type: RiskType::FileCountAnomaly,
                description: format!(
                    "File count sharply decreased: {} → {} (decreased {:.1}%)",
                    local_count, remote_count, -change_ratio * 100.0
                ),
                details: vec!["⚠️ Possible remote repository emptying or malicious deletion".to_string()],
            });
        }

        // File increase > 200% - possible poisoning (injecting many files)
        if change_ratio > 2.0 {
            return Ok(SecurityRisk {
                level: RiskLevel::Medium,
                risk_type: RiskType::FileCountAnomaly,
                description: format!(
                    "File count abnormally increased: {} → {} (increased {:.1}%)",
                    local_count, remote_count, change_ratio * 100.0
                ),
                details: vec!["⚠️ Too many new files on remote, please check for malicious content".to_string()],
            });
        }

        Ok(SecurityRisk {
            level: RiskLevel::Safe,
            risk_type: RiskType::FileCountAnomaly,
            description: String::new(),
            details: Vec::new(),
        })
    }

    /// Count files in tree
    fn count_files_in_tree(_repo: &Repository, tree: &git2::Tree) -> Result<usize> {
        let mut count = 0;
        tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob) {
                count += 1;
            }
            git2::TreeWalkResult::Ok
        })?;
        Ok(count)
    }

    /// Check sensitive file changes
    fn check_sensitive_files(repo: &Repository, local: Oid, remote: Oid) -> Result<SecurityRisk> {

        let local_tree = repo.find_commit(local)?.tree()?;
        let remote_tree = repo.find_commit(remote)?.tree()?;

        let mut modified_sensitive_files = Vec::new();

        // Get diff
        let diff = repo.diff_tree_to_tree(Some(&local_tree), Some(&remote_tree), None)?;

        for delta in diff.deltas() {
            if let Some(path) = delta.new_file().path() {
                let path_str = path.to_string_lossy();
                for pattern in SENSITIVE_PATTERNS.iter() {
                    let matched = if let Some(suffix) = pattern.strip_prefix("*.") {
                        // Glob pattern: match by extension suffix
                        path_str.ends_with(suffix)
                    } else {
                        // 路径组件精确匹配：避免子串误报（如 my-Cargo.toml 不匹配 Cargo.toml）
                        let pattern_components: Vec<&str> = pattern.split('/').collect();
                        let path_components: Vec<&str> = path_str.split('/').collect();
                        pattern_components.len() <= path_components.len()
                            && path_components.windows(pattern_components.len())
                                .any(|window| window == pattern_components.as_slice())
                    };
                    if matched {
                        modified_sensitive_files.push(path_str.to_string());
                        break;
                    }
                }
            }
        }

        if !modified_sensitive_files.is_empty() {
            // Check if credential files or CI configs were modified (critical/high risk)
            let credential_modified = modified_sensitive_files.iter().any(|p| {
                p.contains(".env") || p.ends_with(".pem") || p.ends_with(".key") ||
                p.contains("id_rsa") || p.contains("kubeconfig") || p.contains("credentials")
            });
            let ci_modified = modified_sensitive_files.iter().any(|p| {
                p.contains("workflows") || p.contains("Jenkinsfile") || p.contains(".gitlab-ci")
            });
            let gitignore_modified = modified_sensitive_files.iter().any(|p| p.contains(".gitignore"));
            
            let level = if credential_modified {
                RiskLevel::Critical
            } else if ci_modified || gitignore_modified {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            };

            return Ok(SecurityRisk {
                level,
                risk_type: RiskType::SensitiveFileModified,
                description: format!("Sensitive config file modified: {} files", modified_sensitive_files.len()),
                details: modified_sensitive_files.into_iter().take(5).collect(),
            });
        }

        Ok(SecurityRisk {
            level: RiskLevel::Safe,
            risk_type: RiskType::SensitiveFileModified,
            description: String::new(),
            details: Vec::new(),
        })
    }

    /// Check suspicious code patterns
    fn check_suspicious_patterns(repo: &Repository, local: Oid, remote: Oid) -> Result<SecurityRisk> {
        // Pre-compiled regex (created on each call, but avoids per-file repeated compilation)
        let patterns = Self::suspicious_patterns();

        let local_tree = repo.find_commit(local)?.tree()?;
        let remote_tree = repo.find_commit(remote)?.tree()?;

        let diff = repo.diff_tree_to_tree(Some(&local_tree), Some(&remote_tree), None)?;

        let mut found_patterns: Vec<(String, String)> = Vec::new();
        let mut oversized_files: Vec<(String, String)> = Vec::new();

        for delta in diff.deltas() {
            // Skip deleted files — removing code is generally safe
            if delta.status() == git2::Delta::Deleted {
                continue;
            }

            if let Some(path) = delta.new_file().path() {
                let path_str = path.to_string_lossy();

                // Only check code files
                if !Self::is_code_file(&path_str) {
                    continue;
                }

                // Track oversized files separately (Medium risk, not Critical)
                let file_size = delta.new_file().size() as usize;
                if file_size > Self::MAX_FILE_SIZE {
                    oversized_files.push((
                        path_str.to_string(),
                        format!("Oversized file ({}KB) bypasses pattern scanning", file_size / 1024),
                    ));
                    continue;
                }

                // Try to get file content
                if let Ok(content) = Self::get_file_content(repo, delta.new_file().id()) {
                    for (re, desc) in patterns.iter() {
                        if re.is_match(&content) {
                            found_patterns.push((path_str.to_string(), desc.to_string()));
                            break;
                        }
                    }
                }
            }
        }

        if !found_patterns.is_empty() {
            let details: Vec<String> = found_patterns
                .iter()
                .take(5)
                .map(|(file, pattern)| format!("{}: {}", file, pattern))
                .collect();

            return Ok(SecurityRisk {
                level: RiskLevel::Critical,
                risk_type: RiskType::SuspiciousCodePattern,
                description: format!("Detected {} suspicious code patterns", found_patterns.len()),
                details,
            });
        }

        if !oversized_files.is_empty() {
            let details: Vec<String> = oversized_files
                .iter()
                .take(5)
                .map(|(file, desc)| format!("{}: {}", file, desc))
                .collect();

            return Ok(SecurityRisk {
                level: RiskLevel::Medium,
                risk_type: RiskType::SuspiciousCodePattern,
                description: format!("{} oversized files bypass pattern scanning", oversized_files.len()),
                details,
            });
        }

        Ok(SecurityRisk {
            level: RiskLevel::Safe,
            risk_type: RiskType::SuspiciousCodePattern,
            description: String::new(),
            details: Vec::new(),
        })
    }

    /// Get pre-compiled suspicious code patterns
    fn suspicious_patterns() -> &'static Vec<(regex::Regex, &'static str)> {
        &SUSPICIOUS_PATTERNS
    }

    /// Check if it is a code file
    fn is_code_file(path: &str) -> bool {
        if let Some(ext) = Path::new(path).extension()
            && let Some(ext_str) = ext.to_str() {
                return CODE_EXTENSIONS.iter().any(|&e| e.eq_ignore_ascii_case(ext_str));
            }
        false
    }

    /// Max file size, skip security scan if exceeded
    const MAX_FILE_SIZE: usize = crate::utils::SECURITY_MAX_FILE_SIZE;

    /// Get file content (with size limit)
    fn get_file_content(repo: &Repository, id: Oid) -> Result<String> {
        let blob = repo.find_blob(id)?;
        let content = blob.content();
        
        // Skip large files and binary files
        if content.len() > Self::MAX_FILE_SIZE {
            return Ok(String::new()); // Return empty, meaning skip
        }
        
        let content = std::str::from_utf8(content)
            .unwrap_or("")
            .to_string();
        Ok(content)
    }

    /// Check committer anomalies
    fn check_committer_anomaly(repo: &Repository, local: Oid, remote: Oid) -> Result<SecurityRisk> {
        let mut walk = repo.revwalk()?;
        walk.push(remote)?;
        walk.hide(local)?;

        let mut new_committers: HashMap<String, usize> = HashMap::new();
        let mut unknown_committers = Vec::new();

        // Get known committer list (from local history)
        let known_committers = Self::get_known_committers(repo)?;

        for oid in walk.take(100) {
            let oid = oid?;
            if let Ok(commit) = repo.find_commit(oid) {
                let committer = commit.committer();
                let name = committer.name().unwrap_or("unknown").to_string();
                let email = committer.email().unwrap_or("unknown").to_string();
                let identity = format!("{} <{}>", name, email);
                *new_committers.entry(identity.clone()).or_insert(0) += 1;
                
                // Check if it's a new committer (match by name+email combination)
                if !known_committers.contains(&identity) {
                    let commit_id = commit.id().to_string();
                    let short_id = if commit_id.len() >= 7 {
                        &commit_id[..7]
                    } else {
                        &commit_id
                    };
                    unknown_committers.push(format!(
                        "{} (commit: {})",
                        identity,
                        short_id
                    ));
                }
            }
        }

        if !unknown_committers.is_empty() {
            return Ok(SecurityRisk {
                level: RiskLevel::Medium,
                risk_type: RiskType::CommitterAnomaly,
                description: format!("Found {} new unknown committers", unknown_committers.len()),
                details: unknown_committers.into_iter().take(5).collect(),
            });
        }

        Ok(SecurityRisk {
            level: RiskLevel::Safe,
            risk_type: RiskType::CommitterAnomaly,
            description: String::new(),
            details: Vec::new(),
        })
    }

    /// Get known committer list (from local history)
    fn get_known_committers(repo: &Repository) -> Result<HashSet<String>> {
        let mut committers = HashSet::new();
        let mut walk = repo.revwalk()?;
        
        if let Ok(head) = repo.head()
            && let Some(oid) = head.target() {
                walk.push(oid)?;
            }

        for oid in walk.take(200) {
            let oid = oid?;
            if let Ok(commit) = repo.find_commit(oid) {
                let committer = commit.committer();
                let name = committer.name().unwrap_or("unknown").to_string();
                let email = committer.email().unwrap_or("unknown").to_string();
                committers.insert(format!("{} <{}>", name, email));
            }
        }

        Ok(committers)
    }
}

/// Format security scan result for display
pub fn format_security_report(result: &SecurityScanResult) -> String {
    if result.is_safe {
        return format!("{} Security scan passed", RiskLevel::Safe.emoji());
    }

    let mut report = String::new();
    report.push_str(&format!("\n{} Security warning\n", "🛡️".yellow().bold()));
    report.push_str(&format!("{}", "═".repeat(50).yellow()));
    report.push('\n');

    for risk in &result.risks {
        report.push_str(&format!(
            "\n{} {} [{}]\n",
            risk.level.emoji(),
            risk.risk_type_str().red(),
            risk.level.label().yellow()
        ));
        report.push_str(&format!("   {}\n", risk.description));
        
        if !risk.details.is_empty() {
            report.push_str("   Details:\n");
            for detail in &risk.details {
                report.push_str(&format!("     • {}\n", detail.dimmed()));
            }
        }
    }

    if result.max_level.should_block() {
        report.push('\n');
        report.push_str(&format!("{}", "⚠️ High risk detected, recommended to stop operation!\n".red().bold()));
    }

    report
}

impl SecurityRisk {
    fn risk_type_str(&self) -> String {
        match self.risk_type {
            RiskType::RemoteInaccessible => "Remote inaccessible".to_string(),
            RiskType::FileCountAnomaly => "File count anomaly".to_string(),
            RiskType::SensitiveFileModified => "Sensitive file changes".to_string(),
            RiskType::SuspiciousCodePattern => "Suspicious code pattern".to_string(),
            RiskType::CommitterAnomaly => "Committer anomaly".to_string(),
            RiskType::SignatureVerificationFailed => "Signature verification failed".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_level_ordering() {
        assert!(!RiskLevel::Safe.should_block());
        assert!(!RiskLevel::Low.should_block());
        assert!(!RiskLevel::Medium.should_block());
        assert!(RiskLevel::High.should_block());
        assert!(RiskLevel::Critical.should_block());
    }
}
