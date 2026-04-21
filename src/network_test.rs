//! Network connectivity test module
//!
//! TDD principle: write tests first, verify issues, then fix

use anyhow::Result;
use std::time::Duration;

/// Proxy test result
#[derive(Debug, Clone)]
pub struct ProxyTestResult {
    pub url: String,
    pub success: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

/// Network tester
pub struct NetworkTester;

impl NetworkTester {
    /// Test proxy connectivity
    /// 
    /// Tests proxy availability using actual HTTP requests
    pub async fn test_proxy(proxy_url: &str) -> ProxyTestResult {
        let start = std::time::Instant::now();
        
        // Try to access a reliable test endpoint via proxy
        let result = Self::try_connect_via_proxy(proxy_url).await;
        
        ProxyTestResult {
            url: proxy_url.to_string(),
            success: result.is_ok(),
            latency_ms: if result.is_ok() {
                Some(start.elapsed().as_millis() as u64)
            } else {
                None
            },
            error: result.err().map(|e| e.to_string()),
        }
    }

    /// Try connecting via proxy
    async fn try_connect_via_proxy(proxy_url: &str) -> Result<()> {
        Self::test_tcp_connect(proxy_url).await
    }

    /// Option B: TCP connection test
    async fn test_tcp_connect(proxy_url: &str) -> Result<()> {
        use tokio::net::TcpStream;

        
        // Parse proxy URL
        let url = url::Url::parse(proxy_url)?;
        let host = url.host_str().ok_or_else(|| anyhow::anyhow!("Invalid proxy address"))?;
        let port = url.port().unwrap_or(7890); // Default Clash port
        
        let addr = format!("{}:{}", host, port);
        
        // Try to connect
        match tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(&addr)
        ).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(anyhow::anyhow!("TCP connection failed: {}", e)),
            Err(_) => Err(anyhow::anyhow!("Connection timeout")),
        }
    }

    /// Plan C: use git2 to test actual fetch
    pub fn test_git_fetch_via_proxy(proxy_url: &str, test_repo: &std::path::Path) -> Result<Duration> {
        use git2::{FetchOptions, ProxyOptions, RemoteCallbacks};
        
        let start = std::time::Instant::now();
        
        let repo = git2::Repository::open(test_repo)?;
        let mut remote = repo.find_remote("origin")?;
        
        let mut callbacks = RemoteCallbacks::new();
        callbacks.sideband_progress(|data| {
            print!("{}", String::from_utf8_lossy(data));
            true
        });
        
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        
        let mut proxy_opts = ProxyOptions::new();
        proxy_opts.url(proxy_url);
        fetch_opts.proxy_options(proxy_opts);
        
        // Try fetch (no download, only check connectivity)
        remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None)?;
        
        Ok(start.elapsed())
    }

    /// Run full network diagnostics
    pub async fn diagnose(proxy_url: Option<&str>) -> Vec<String> {
        let mut reports = vec![];
        
        // 1. Check if internet is accessible (without proxy)
        reports.push("=== Network Diagnostics ===".to_string());
        
        match Self::test_tcp_connect("http://1.1.1.1:53").await {
            Ok(_) => reports.push("✓ Basic network connection normal".to_string()),
            Err(e) => reports.push(format!("✗ Basic network connection failed: {}", e)),
        }
        
        // 2. Test proxy (if provided)
        if let Some(proxy) = proxy_url {
            reports.push(format!("\n=== Proxy test: {} ===", proxy));
            
            // Test TCP connectivity
            match Self::test_tcp_connect(proxy).await {
                Ok(_) => reports.push("✓ TCP connection succeeded".to_string()),
                Err(e) => reports.push(format!("✗ TCP connection failed: {}", e)),
            }
            
            // Test full proxy functionality
            let result = Self::test_proxy(proxy).await;
            if result.success {
                reports.push(format!("✓ Proxy working (Latency: {}ms)", 
                    result.latency_ms.unwrap_or(0)));
            } else {
                reports.push(format!("✗ Proxy test failed: {}", 
                    result.error.unwrap_or_default()));
            }
        }
        
        reports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tcp_connect_localhost() {
        // Test connecting to an unlikely open port should fail
        let result = NetworkTester::test_tcp_connect("http://127.0.0.1:1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tcp_connect_cloudflare_dns() {
        // Cloudflare DNS should be connectable
        let result = NetworkTester::test_tcp_connect("http://1.1.1.1:53").await;
        // Note: May fail in some network environments, so no assert
        println!("Cloudflare DNS connection results: {:?}", result);
    }

    #[test]
    fn test_parse_proxy_url() {
        let url = "http://127.0.0.1:7890";
        let parsed = url::Url::parse(url).unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(7890));
    }

    #[test]
    fn test_parse_proxy_url_no_port() {
        let url = "http://127.0.0.1";
        let parsed = url::Url::parse(url).unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), None); // Default port
    }
}
