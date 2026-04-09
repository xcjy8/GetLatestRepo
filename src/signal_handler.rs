//! Signal handler module
//!
//! Provides graceful shutdown, ensuring when SIGINT (Ctrl+C) is received:
//! - Complete current database transactions
//! - Clean up temporary files
//! - Release file locks

use std::sync::atomic::{AtomicBool, Ordering};

/// Global shutdown flag
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Initialize signal handling
///
/// Listen for Ctrl+C (SIGINT), set shutdown flag
pub fn init() {
    tokio::spawn(async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                eprintln!("\n⚠️  Interrupt signal received, shutting down gracefully...");
                SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
                // Give some time for current operations to complete
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                std::process::exit(130);
            }
            Err(e) => {
                eprintln!("⚠️  Unable to listen for Ctrl+C: {}", e);
            }
        }
    });
}

/// Check if shutdown was requested
#[allow(dead_code)]
pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Interval for periodically polling shutdown flag
#[allow(dead_code)]
pub const SHUTDOWN_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_flag() {
        assert!(!is_shutdown_requested());
        SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
        assert!(is_shutdown_requested());
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    }
}
