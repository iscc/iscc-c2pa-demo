//! Model runs spread over the cores, results in input order: one thread takes jobs from an
//! iterator (which may do work of its own, such as cutting text or rendering pages) into a short
//! queue, workers each run one job at a time on an rten thread pool of their own, and the calling
//! thread gathers the results, reporting each as it arrives. rten spreads one small input poorly
//! over many cores, so several inputs at once on a thread or a few each go faster. The results
//! do not depend on the number of workers or threads: rten computes the same on any thread pool.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rten::ThreadPool;

use crate::tools::Cancelled;

/// The results of `work` on the jobs of `jobs`, in their order. A thread of its own iterates
/// `jobs`, until they end or the run does. `workers` threads take the jobs, each with a pool of `threads` rten threads. `done` hears every
/// result as it arrives, in any order, and stops the run with [`Cancelled`] by returning false; a
/// job that fails stops it with its error.
pub fn ordered<J: Send, R: Send>(
    workers: usize,
    threads: usize,
    jobs: impl Iterator<Item = J> + Send,
    work: impl Fn(J, &Arc<ThreadPool>) -> Result<R> + Sync,
    done: &mut dyn FnMut(&R) -> bool,
) -> Result<Vec<R>> {
    let workers = workers.max(1);
    let stop = AtomicBool::new(false);
    // A short queue: the feeder stays a few jobs ahead of the workers.
    let (job_tx, job_rx) = mpsc::sync_channel::<(usize, J)>(workers);
    let job_rx = Mutex::new(job_rx);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            for job in jobs.enumerate() {
                if stop.load(Ordering::Relaxed) || job_tx.send(job).is_err() {
                    break;
                }
            }
        });
        for _ in 0..workers {
            let done_tx = done_tx.clone();
            let (work, job_rx) = (&work, &job_rx);
            scope.spawn(move || run_jobs(work, threads, job_rx, stop, done_tx));
        }
        drop(done_tx);
        gather(done_rx, stop, done)
    })
}

/// A worker: runs queued jobs on a thread pool of its own until the queue closes. After a stop it
/// only empties the queue, so the thread that fills it never waits on a full one.
fn run_jobs<J, R>(
    work: &impl Fn(J, &Arc<ThreadPool>) -> Result<R>,
    threads: usize,
    jobs: &Mutex<Receiver<(usize, J)>>,
    stop: &AtomicBool,
    done: Sender<(usize, Result<R>)>,
) {
    let pool = Arc::new(ThreadPool::with_num_threads(threads.max(1)));
    loop {
        let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).recv();
        let Ok((index, job)) = job else {
            return;
        };
        if !stop.load(Ordering::Relaxed) {
            // The receiver is gone only after a stop.
            let _ = done.send((index, work(job, &pool)));
        }
    }
}

/// The results from the workers in job order. Sets `stop` when a job fails or `done` returns
/// false.
fn gather<R>(
    results: Receiver<(usize, Result<R>)>,
    stop: &AtomicBool,
    done: &mut dyn FnMut(&R) -> bool,
) -> Result<Vec<R>> {
    let mut ordered: Vec<Option<R>> = Vec::new();
    for (index, result) in results {
        let result = result.inspect_err(|_| stop.store(true, Ordering::Relaxed))?;
        if !done(&result) {
            stop.store(true, Ordering::Relaxed);
            return Err(Cancelled.into());
        }
        if ordered.len() <= index {
            ordered.resize_with(index + 1, || None);
        }
        ordered[index] = Some(result);
    }
    Ok(ordered.into_iter().flatten().collect())
}

/// The instruction set rten picks on this CPU, for the drift reports of the tests that compare
/// model results with a reference.
#[cfg(test)]
pub(crate) fn instruction_set() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f") {
            return "x86_64 AVX-512";
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return "x86_64 AVX2";
        }
        "x86_64 generic"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64 NEON"
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "generic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    /// Squares a number after a pause that makes later jobs finish first.
    fn slow_square(i: usize, _: &Arc<ThreadPool>) -> Result<usize> {
        std::thread::sleep(std::time::Duration::from_millis((20 - i as u64 % 20) * 2));
        Ok(i * i)
    }

    #[test]
    fn results_keep_the_order_of_the_jobs() {
        let mut heard = 0;
        let squares = ordered(4, 1, 0..30, slow_square, &mut |_| {
            heard += 1;
            true
        })
        .unwrap();
        assert_eq!(squares, (0..30).map(|i| i * i).collect::<Vec<_>>());
        assert_eq!(heard, 30);
        let none = ordered(3, 2, 0..0, slow_square, &mut |_| true).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn a_stop_or_a_failure_ends_the_run() {
        let fed = std::sync::atomic::AtomicUsize::new(0);
        let counting = (0..1000).inspect(|_| {
            fed.fetch_add(1, Ordering::Relaxed);
        });
        let stopped = ordered(2, 1, counting, slow_square, &mut |_| false).unwrap_err();
        assert!(stopped.downcast_ref::<Cancelled>().is_some(), "{stopped}");
        assert!(fed.load(Ordering::Relaxed) < 1000, "the jobs stopped early");
        let failing = |i: usize, _: &Arc<ThreadPool>| -> Result<usize> {
            if i == 5 {
                bail!("job 5 failed");
            }
            Ok(i)
        };
        let failed = ordered(2, 1, 0..100, failing, &mut |_| true).unwrap_err();
        assert_eq!(failed.to_string(), "job 5 failed");
    }
}
