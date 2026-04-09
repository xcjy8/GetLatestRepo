//! General utility functions

use std::path::Path;

/// Sanitize URL, remove credential info
///
/// Convert `https://token@github.com/user/repo.git` to `https://github.com/user/repo.git`
pub fn sanitize_url(url: &str) -> String {
    // Parse URL, remove user info part
    if let Ok(parsed) = url::Url::parse(url) {
        if parsed.username() != "" || parsed.password().is_some() {
            // Rebuild URL without credentials
            let mut cleaned = parsed.clone();
            cleaned.set_username("").ok();
            cleaned.set_password(None).ok();
            return cleaned.to_string();
        }
    }
    // If parsing fails, return original URL (may be local path or other format)
    url.to_string()
}

/// Sanitize path, only show last two directory levels
/// 
/// Examples:
/// - `/home/user/projects/myrepo` -> `.../projects/myrepo`
/// - `/Users/sy/spgit/myrepo` -> `.../spgit/myrepo`
/// - `myrepo` -> `myrepo`
pub fn sanitize_path(path: &str) -> String {
    let path = Path::new(path);
    let components: Vec<_> = path.components().collect();
    
    if components.len() <= 2 {
        // Path is short, return directly
        path.to_string_lossy().to_string()
    } else {
        // Only show last two levels
        let last_two: Vec<_> = components.iter().rev().take(2).rev().collect();
        // Safety check: ensure there are two elements (avoid panic)
        if last_two.len() < 2 {
            path.to_string_lossy().to_string()
        } else {
            format!(".../{}/{}", 
                last_two[0].as_os_str().to_string_lossy(),
                last_two[1].as_os_str().to_string_lossy()
            )
        }
    }
}

/// Sanitize path (Path version)
#[allow(dead_code)]
pub fn sanitize_path_buf(path: &Path) -> String {
    sanitize_path(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_short_path() {
        assert_eq!(sanitize_path("myrepo"), "myrepo");
        assert_eq!(sanitize_path("/myrepo"), "/myrepo");
    }

    #[test]
    fn test_sanitize_long_path() {
        assert_eq!(
            sanitize_path("/home/user/projects/myrepo"),
            ".../projects/myrepo"
        );
        assert_eq!(
            sanitize_path("/Users/sy/spgit/myrepo"),
            ".../spgit/myrepo"
        );
    }

    #[test]
    fn test_sanitize_url_with_credentials() {
        assert_eq!(
            sanitize_url("https://token@github.com/user/repo.git"),
            "https://github.com/user/repo.git"
        );
        assert_eq!(
            sanitize_url("https://user:pass@github.com/user/repo.git"),
            "https://github.com/user/repo.git"
        );
    }

    #[test]
    fn test_sanitize_url_without_credentials() {
        assert_eq!(
            sanitize_url("https://github.com/user/repo.git"),
            "https://github.com/user/repo.git"
        );
    }

    #[test]
    fn test_sanitize_url_invalid() {
        // Invalid URL should be returned as-is
        assert_eq!(sanitize_url("not-a-url"), "not-a-url");
    }
}
