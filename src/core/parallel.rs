//! Work shared out over several threads. Inside a job (see [`super::cancel`]), the threads
//! check its flag as the thread that started them would, and stopping the job stops them all.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How many threads to share one piece of work among: one per processor thread, up to eight,
/// as a folder compare does by default. Beyond that, memory rather than the processor limits
/// the work this is used for. Asked of the system once: on Linux that reads cgroup files.
pub fn threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(8)
    })
}

/// Threads taken beside their callers by all the work running at once, out of
/// [`threads`]` - 1`, so that the comparisons of a folder compare's workers don't each start
/// [`threads`] more.
static TAKEN: AtomicUsize = AtomicUsize::new(0);

/// Threads for one piece of work: the calling thread and those taken from what [`threads`]
/// allows all work at once, given back when this is dropped.
pub struct Threads {
    taken: usize,
    from: &'static AtomicUsize,
}

impl Threads {
    pub fn count(&self) -> usize {
        self.taken + 1
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        self.from.fetch_sub(self.taken, Ordering::Relaxed);
    }
}

/// Up to `want` threads, a power of two (1 when none are free): work split into a power of two
/// of parts gets no faster from the threads between two powers.
pub fn take_power_of_two(want: usize) -> Threads {
    take_from(&TAKEN, threads() - 1, want)
}

fn take_from(from: &'static AtomicUsize, spare: usize, want: usize) -> Threads {
    let mut used = from.load(Ordering::Relaxed);
    loop {
        let count = want.clamp(1, spare.saturating_sub(used) + 1);
        let taken = (1 << count.ilog2()) - 1;
        match from.compare_exchange_weak(used, used + taken, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Threads { taken, from },
            Err(now) => used = now,
        }
    }
}

/// `f` of each of `items`, in order, on up to `threads` threads taking the next item as they
/// are done. A stopped job unwinds out of this once every thread has left its item.
pub fn map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if threads <= 1 || items.len() <= 1 {
        return items.iter().map(f).collect();
    }
    let job = super::cancel::current();
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads.min(items.len()))
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    let done = super::cancel::share(&job, || loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(at) else {
                            break;
                        };
                        out.push((at, f(item)));
                    });
                    if done.is_none() {
                        stopped.store(true, Ordering::Relaxed);
                        // The others stop at their next check; this one takes no more.
                        next.store(items.len(), Ordering::Relaxed);
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    if stopped.load(Ordering::Relaxed) {
        super::cancel::check();
    }
    out.sort_unstable_by_key(|&(at, _)| at);
    out.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn results_keep_the_order_of_the_items() {
        let items: Vec<usize> = (0..1000).collect();
        assert_eq!(
            map(&items, 4, |i| i * 2),
            items.iter().map(|i| i * 2).collect::<Vec<_>>()
        );
        assert_eq!(map(&items, 1, |i| i + 1)[999], 1000);
        assert!(map(&[] as &[usize], 4, |i| *i).is_empty());
    }

    /// Work running at once shares the threads, each taking a power of two of them, and gives
    /// them back when done.
    #[test]
    fn work_at_once_shares_the_threads() {
        static FROM: AtomicUsize = AtomicUsize::new(0);
        let a = take_from(&FROM, 7, 8);
        assert_eq!(a.count(), 8);
        let b = take_from(&FROM, 7, 8);
        assert_eq!(b.count(), 1, "none left");
        drop(a);
        let c = take_from(&FROM, 7, 6);
        assert_eq!(c.count(), 4, "6 rounds down");
        let d = take_from(&FROM, 7, 8);
        assert_eq!(d.count(), 4, "4 of the 8 are free");
        drop((b, c, d));
        assert_eq!(FROM.load(Ordering::Relaxed), 0);
        assert_eq!(take_from(&FROM, 0, 8).count(), 1);
        assert_eq!(take_from(&FROM, 7, 0).count(), 1);
    }

    #[test]
    fn stopping_the_job_stops_every_thread() {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let seen = AtomicUsize::new(0);
        let items: Vec<usize> = (0..10_000).collect();
        let out = super::super::cancel::run(stop, || {
            map(&items, 4, |&i| {
                if i == 100 {
                    flag.store(true, Ordering::Relaxed);
                }
                super::super::cancel::check();
                seen.fetch_add(1, Ordering::Relaxed);
            })
        });
        assert!(out.is_none());
        assert!(seen.load(Ordering::Relaxed) < items.len());
        // Outside a job every item is done.
        assert_eq!(map(&items, 4, |_| ()).len(), items.len());
    }
}
