//! Running a subsystem on a runtime and a thread of its own, and starting it
//! again if it dies.
//!
//! The deploy and analytics runtimes each get one, so nothing either does —
//! a brotli pass, a month-long report — runs on the threads that serve sites.
//! Each is cheap to restart because each reconciles everything it is
//! responsible for as it starts, so a restart needs nothing replayed.

use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;

use crate::fields;
use crate::log::Logger;

/// First and longest wait before restarting a runtime that died.
const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(60);

/// A runtime that ran this long before dying is restarted without the backoff
/// it had built up.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// Run `body` on a fresh runtime of `workers` threads, on a thread named
/// `npanel-{name}`, until it returns `Ok`. An `Err` or a panic that reaches
/// `body` gets a new runtime after a second, then after twice as long each
/// time, up to a minute. A panic inside a task the body spawns is the task's
/// to catch; it never reaches here.
pub fn supervise(
    name: &'static str,
    workers: usize,
    log: Logger,
    mut body: impl FnMut(&Runtime) -> Result<(), String> + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let thread = format!("npanel-{name}");
    std::thread::Builder::new()
        .name(thread.clone())
        .spawn(move || {
            let mut delay = RESTART_MIN;
            loop {
                let started = Instant::now();
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(workers)
                    .thread_name(&thread)
                    .enable_all()
                    .build()
                    .expect("a runtime");

                let result = std::panic::catch_unwind(AssertUnwindSafe(|| body(&runtime)));
                runtime.shutdown_timeout(Duration::from_secs(5));

                let reason = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(reason)) => reason,
                    Err(_) => "panicked".into(),
                };
                if started.elapsed() > HEALTHY_AFTER {
                    delay = RESTART_MIN;
                }
                log.error(&format!("{name}_thread_exited"), fields! { "err_msg" => reason, "restart_in_ms" => delay.as_millis() as u64 });
                std::thread::sleep(delay);
                delay = (delay * 2).min(RESTART_MAX);
            }
        })
        .expect("a thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn restarts_until_done() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let thread = supervise("test", 1, Logger::new(crate::log::LogLevel::Error), move |runtime| {
            let run = counted.fetch_add(1, Ordering::SeqCst);
            runtime.block_on(async {
                if run == 0 {
                    panic!("the first run dies");
                }
            });
            Ok(())
        });
        thread.join().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
