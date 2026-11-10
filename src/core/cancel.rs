//! Stopping long work from inside: a job run with [`run`] is given a flag, and the loops of the
//! work it calls ([`check`] in reading a file, numbering lines, building rows) leave as soon as
//! the flag is set, by unwinding out of the job. Anywhere else [`check`] does nothing, so the
//! same code runs unchanged when nothing can stop it.
//!
//! Unwinding drops everything the job built, so a stopped job leaves nothing half made behind,
//! and [`std::panic::resume_unwind`] does it without the panic message a real panic prints.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

thread_local! {
    static FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// What a stopped job unwinds with.
struct Stopped;

/// When a check last saw the flag set, for the benchmarks: the work has stopped from then on,
/// and what follows is freeing what it built.
#[cfg(test)]
pub static NOTICED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Leaves the job this thread runs, if it was told to stop.
#[inline]
pub fn check() {
    let stop = FLAG.with(|f| {
        f.borrow()
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
    });
    if stop {
        #[cfg(test)]
        if let Ok(mut at) = NOTICED.lock() {
            *at = Some(std::time::Instant::now());
        }
        std::panic::resume_unwind(Box::new(Stopped));
    }
}

/// Like [`check`], every `every` steps of a loop (a power of two), so that a loop of millions
/// of short steps does not look at the flag on each.
#[inline]
pub fn check_every(step: usize, every: usize) {
    if step & (every - 1) == 0 {
        check();
    }
}

/// Runs `job` on this thread until it ends or `stop` is set: `None` when it was stopped. A
/// real panic in the job goes on unwinding.
pub fn run<T>(stop: Arc<AtomicBool>, job: impl FnOnce() -> T) -> Option<T> {
    let previous = FLAG.with(|f| f.replace(Some(stop)));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    FLAG.with(|f| *f.borrow_mut() = previous);
    match result {
        Ok(v) => Some(v),
        Err(payload) if payload.is::<Stopped>() => None,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_stops_at_its_next_check_and_only_there() {
        let stop = Arc::new(AtomicBool::new(false));
        assert_eq!(run(stop.clone(), || 7), Some(7));
        let mut reached = 0;
        let flag = stop.clone();
        let out = run(stop, || {
            for i in 0..100 {
                if i == 10 {
                    flag.store(true, Ordering::Relaxed);
                }
                check();
                reached = i;
            }
            "finished"
        });
        assert_eq!(out, None);
        assert_eq!(reached, 9);
        // Outside a job the flag is gone, and a check does nothing.
        check();
    }

    /// Reading and comparing large files stop soon after they are told to, wherever they are.
    #[test]
    fn reading_and_comparing_stop_soon_after_cancelling() {
        use crate::core::diff::{compute, DiffOptions};
        let dir = tempfile::tempdir().unwrap();
        let text: String = (0..2_000_000)
            .map(|i| format!("line {}\n", i % 900_000))
            .collect();
        for n in ["a.txt", "b.txt"] {
            std::fs::write(dir.path().join(n), &text).unwrap();
        }
        for after_ms in [0, 50, 400, 1200] {
            let stop = Arc::new(AtomicBool::new(false));
            let (job_stop, d) = (stop.clone(), dir.path().to_path_buf());
            let job = std::thread::spawn(move || {
                run(job_stop, || {
                    let docs: Vec<_> = ["a.txt", "b.txt"]
                        .iter()
                        .map(|n| crate::core::text::load_doc(&d.join(n), None).unwrap())
                        .collect();
                    let panes: Vec<&[String]> = docs.iter().map(|d| d.lines.as_slice()).collect();
                    compute(&panes, &DiffOptions::default()).rows.len()
                })
            });
            std::thread::sleep(std::time::Duration::from_millis(after_ms));
            let asked = std::time::Instant::now();
            stop.store(true, Ordering::Relaxed);
            let out = job.join().unwrap();
            let took = asked.elapsed();
            // A job that finished before the flag was set is fine too.
            assert!(
                out.is_none() || after_ms > 0,
                "stopped at {after_ms} ms: {out:?}"
            );
            assert!(
                took < std::time::Duration::from_millis(500),
                "stopped {took:?} after cancelling at {after_ms} ms"
            );
        }
    }

    #[test]
    fn a_real_panic_is_not_taken_for_a_stop() {
        let stop = Arc::new(AtomicBool::new(false));
        let r = std::panic::catch_unwind(|| run(stop, || panic!("broken")));
        assert!(r.is_err());
        check();
    }
}
