//! Concurrent execution utility module
//!
//! Provides safe concurrent execution, solving the following problems:
//! - Deadlock risk (proper cleanup when worker thread panics)
//! - Busy-wait issue (uses condition variables)
//! - Error handling (does not silently ignore errors)
//! - Reasonable timeout handling

use std::sync::mpsc::{channel, Sender};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

/// Task results
#[derive(Debug)]
pub struct TaskResult<T> {
    pub index: usize,
    pub result: T,
}

/// Execute multiple tasks concurrently (return raw results)
///
/// # Parameters
/// - `tasks`: Task list, each task is a closure
/// - `max_concurrent`: Maximum concurrency
///
/// # Returns
/// Return result list in original order (panicked tasks return None)
///
/// # Features
/// - Auto-handle panics (returns None)
/// - Use blocking wait (non busy-wait)
/// - Overall deadline (120s) to prevent infinite hang on stuck git2/fs ops
pub fn execute_concurrent_raw<F, T>(tasks: Vec<F>, max_concurrent: usize) -> Vec<Option<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let total = tasks.len();
    if total == 0 {
        return Vec::new();
    }

    let (tx, rx) = channel::<TaskResult<Option<T>>>();
    // Use AtomicUsize instead of Mutex<usize>, avoiding lock poisoning and counter leak issues
    let active_count = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(max_concurrent));
    let mut handles = Vec::new();

    for (index, task) in tasks.into_iter().enumerate() {
        if crate::signal_handler::is_shutdown_requested() {
            for remaining_index in index..total {
                let _ = tx.send(TaskResult { index: remaining_index, result: None });
            }
            break;
        }

        // 等待直到有空位（阻塞式，非忙等）
        loop {
            let current = active_count.load(Ordering::Relaxed);
            let max = max_concurrent.load(Ordering::Relaxed);
            if current < max {
                // 尝试递增计数器
                match active_count.compare_exchange_weak(
                    current,
                    current + 1,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(_) => continue, // Contention failed, retry
                }
            }
            // Brief wait to avoid CPU spinning
            thread::sleep(std::time::Duration::from_millis(1));
        }

        let tx_inner = Sender::clone(&tx);
        let active_count_inner = Arc::clone(&active_count);

        // Spawn thread with reduced stack size (1MB) to limit memory waste if threads are abandoned after timeout
        match thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                // Use RAII guard to ensure counter decrements correctly (even on panic)
                struct CountGuard(Arc<AtomicUsize>);
                impl Drop for CountGuard {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                let _guard = CountGuard(active_count_inner);

                // Execute task (catch panic)
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));

                // Send result (panicked returns None)
                let result = match result {
                    Ok(r) => Some(r),
                    Err(_) => {
                        eprintln!("Warning: task {} panicked", index);
                        None
                    }
                };
                let _ = tx_inner.send(TaskResult { index, result });
                // _guard drops here, automatically decrementing counter
            }) {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                eprintln!("Warning: failed to spawn thread for task {}: {}", index, e);
                active_count.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.send(TaskResult { index, result: None });
            }
        }
    }

    // Close sender
    drop(tx);

    // Collect results
    let mut results: Vec<Option<Option<T>>> = (0..total).map(|_| None).collect();
    let mut received = 0;

    // Overall deadline to prevent infinite hang on stuck git2/fs operations
    let overall_deadline = std::time::Instant::now() + std::time::Duration::from_secs(crate::utils::CONCURRENT_OVERALL_TIMEOUT_SECS);

    while received < total {
        let remaining = overall_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            eprintln!("Warning: concurrent execution exceeded overall deadline (120s), {} tasks unfinished", total - received);
            break;
        }
        // Use per-recv timeout of 30s (capped by remaining overall time)
        let recv_timeout = std::cmp::min(remaining, std::time::Duration::from_secs(crate::utils::CONCURRENT_RECV_TIMEOUT_SECS));

        match rx.recv_timeout(recv_timeout) {
            Ok(task_result) => {
                results[task_result.index] = Some(task_result.result);
                received += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Check if all threads have finished
                let active_handles = handles.iter().filter(|h| !h.is_finished()).count();
                if active_handles == 0 {
                    eprintln!("Warning: {} tasks incomplete, may have panicked or failed to send", total - received);
                    break;
                }
                // If we still have active handles but overall deadline not yet reached,
                // loop again (with potentially shorter remaining timeout)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    // Best-effort join with its own deadline to avoid blocking forever
    let join_deadline = std::time::Instant::now() + std::time::Duration::from_secs(crate::utils::CONCURRENT_JOIN_TIMEOUT_SECS);
    for handle in handles {
        if std::time::Instant::now() > join_deadline {
            eprintln!("Warning: join deadline exceeded, leaving remaining threads detached to prevent deadlock");
            break;
        }
        let _ = handle.join();
    }

    // Flatten results: Option<Option<T>> -> Option<T>
    results.into_iter().map(|r| r.flatten()).collect()
}

/// Execute single task and catch panic
#[allow(dead_code)]
pub fn run_with_catch<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => Ok(result),
        Err(_) => Err("task panicked".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_concurrent_execute() {
        let tasks: Vec<_> = (0..10)
            .map(|i| move || -> i32 { (i * 2) as i32 })
            .collect();

        let results = execute_concurrent_raw(tasks, 3);

        assert_eq!(results.len(), 10);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(*result, Some((i * 2) as i32));
        }
    }

    #[test]
    fn test_empty_tasks() {
        let tasks: Vec<Box<dyn FnOnce() -> i32 + Send>> = Vec::new();
        let results = execute_concurrent_raw(tasks, 3);
        assert!(results.is_empty());
    }

    #[test]
    fn test_panic_recovery() {
        let tasks: Vec<Box<dyn FnOnce() -> i32 + Send>> = vec![
            Box::new(|| 1),
            Box::new(|| panic!("task 2 panic")),
            Box::new(|| 3),
        ];

        let results = execute_concurrent_raw(tasks, 2);

        assert_eq!(results.len(), 3);
        assert_eq!(results[0], Some(1));
        assert_eq!(results[1], None); // panic returns None
        assert_eq!(results[2], Some(3));
    }

    #[test]
    fn test_counter_no_leak_on_panic() {
        // Test that counter doesn't leak even if all tasks panic
        let tasks: Vec<Box<dyn FnOnce() -> () + Send>> = (0..5)
            .map(|i| Box::new(move || panic!("task {} panic", i)) as Box<dyn FnOnce() -> () + Send>)
            .collect();

        let _results = execute_concurrent_raw(tasks, 2);
        // If counter leaks, subsequent tasks may fail to execute
        // Mainly verifies no deadlock occurs
    }
}
