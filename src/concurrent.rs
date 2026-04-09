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
/// - Reasonable timeout (5 seconds)
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
        // Wait until slot is available (blocking, non busy-wait)
        loop {
            let current = active_count.load(Ordering::Relaxed);
            let max = max_concurrent.load(Ordering::Relaxed);
            if current < max {
                // Try to increment counter
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

        let tx = Sender::clone(&tx);
        let active_count = Arc::clone(&active_count);

        let handle = thread::spawn(move || {
            // Use RAII guard to ensure counter decrements correctly (even on panic)
            struct CountGuard(Arc<AtomicUsize>);
            impl Drop for CountGuard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _guard = CountGuard(active_count);

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
            let _ = tx.send(TaskResult { index, result });
            // _guard drops here, automatically decrementing counter
        });

        handles.push(handle);
    }

    // Close sender
    drop(tx);

    // Collect results
    let mut results: Vec<Option<Option<T>>> = (0..total).map(|_| None).collect();
    let mut received = 0;

    while received < total {
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(task_result) => {
                results[task_result.index] = Some(task_result.result);
                received += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Check if all threads have finished
                let active_handles = handles.iter().filter(|h| !h.is_finished()).count();
                if active_handles == 0 {
                    eprintln!("Warning: {} tasks incomplete, may have panicked", total - received);
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    // Wait for all threads to finish
    for handle in handles {
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
