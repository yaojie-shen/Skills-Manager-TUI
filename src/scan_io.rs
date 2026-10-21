//! Short-lived, bounded parallel reads for one reconciliation pass.
//!
//! Tasks borrow the input, claim small batches from one shared counter, and
//! process each batch serially. Results retain input order, including errors.
//! At most 32 threads are created per call. Callers may select a lower limit
//! for their read phase. Empty, small, single-worker, or explicitly serial
//! calls run on the caller. Nothing is cached across calls, and this module
//! performs no filesystem writes. Content
//! hashing keeps its own bounded pool.

use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_WORKERS: usize = 32;
const BATCH_SIZE: usize = 4;

pub(crate) fn map_ordered<T: Sync, R: Send>(
    items: &[T],
    parallel: bool,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    map_ordered_with_limit(items, parallel, MAX_WORKERS, f)
}

pub(crate) fn map_ordered_with_limit<T: Sync, R: Send>(
    items: &[T],
    parallel: bool,
    worker_limit: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let worker_limit = worker_limit.clamp(1, MAX_WORKERS);
    let workers = items.len().div_ceil(BATCH_SIZE).min(worker_limit);
    if !parallel || workers <= 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..workers)
            .map(|_| {
                let next = &next;
                let f = &f;
                scope.spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let start = next.fetch_add(BATCH_SIZE, Ordering::Relaxed);
                        if start >= items.len() {
                            break;
                        }
                        let end = start.saturating_add(BATCH_SIZE).min(items.len());
                        for (offset, item) in items[start..end].iter().enumerate() {
                            results.push((start + offset, f(item)));
                        }
                    }
                    results
                })
            })
            .collect();
        let mut results: Vec<Option<R>> =
            std::iter::repeat_with(|| None).take(items.len()).collect();
        for job in jobs {
            for (index, value) in job.join().expect("scan I/O worker panicked") {
                results[index] = Some(value);
            }
        }
        results
            .into_iter()
            .map(|value| value.expect("scan I/O task was not processed"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn empty_small_and_serial_work_stays_on_the_caller() {
        let thread = std::thread::current().id();
        for (items, parallel) in [
            (Vec::new(), true),
            (vec![1, 2, 3, 4], true),
            ((0..100).collect(), false),
        ] {
            let results = map_ordered(&items, parallel, |value| {
                assert_eq!(std::thread::current().id(), thread);
                value * 2
            });
            assert_eq!(
                results,
                items.iter().map(|value| value * 2).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn bounded_workers_reuse_threads_and_keep_input_order_and_errors() {
        let threads = Mutex::new(HashSet::new());
        let calls = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let input: Vec<_> = (0..1025).rev().collect();
        let results = map_ordered(&input, true, |value| {
            threads.lock().unwrap().insert(std::thread::current().id());
            calls.fetch_add(1, Ordering::Relaxed);
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::Relaxed);
            std::thread::yield_now();
            active.fetch_sub(1, Ordering::SeqCst);
            if value % 11 == 0 {
                Err(*value)
            } else {
                Ok(value * 2)
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), input.len());
        assert!(threads.lock().unwrap().len() <= MAX_WORKERS);
        assert!(peak.load(Ordering::Relaxed) <= MAX_WORKERS);
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert_eq!(
            results,
            input
                .iter()
                .map(|value| if value % 11 == 0 {
                    Err(*value)
                } else {
                    Ok(value * 2)
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn custom_worker_limit_reuses_two_threads_and_keeps_input_order() {
        let threads = Mutex::new(HashSet::new());
        let calls = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let first_batches = std::sync::Barrier::new(2);
        let input: Vec<_> = (0..BATCH_SIZE * 4).rev().collect();
        let results = map_ordered_with_limit(&input, true, 2, |value| {
            threads.lock().unwrap().insert(std::thread::current().id());
            calls.fetch_add(1, Ordering::Relaxed);
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::Relaxed);
            if *value == BATCH_SIZE * 4 - 1 || *value == BATCH_SIZE * 3 - 1 {
                first_batches.wait();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            value * 2
        });

        assert_eq!(calls.load(Ordering::Relaxed), input.len());
        assert_eq!(threads.into_inner().unwrap().len(), 2);
        assert_eq!(peak.load(Ordering::Relaxed), 2);
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert_eq!(
            results,
            input.iter().map(|value| value * 2).collect::<Vec<_>>()
        );
    }

    #[test]
    fn completion_can_be_out_of_order_while_results_stay_ordered() {
        let input: Vec<_> = (0..BATCH_SIZE * 4).collect();
        let barrier = std::sync::Barrier::new(4);
        let (tx, rx) = std::sync::mpsc::channel();
        let rx = Mutex::new(rx);
        let completed = Mutex::new(Vec::new());
        let threads = Mutex::new(HashSet::new());
        let results = map_ordered(&input, true, |value| {
            threads.lock().unwrap().insert(std::thread::current().id());
            if value % BATCH_SIZE == 0 {
                barrier.wait();
            }
            if *value == 0 {
                rx.lock().unwrap().recv().unwrap();
            }
            completed.lock().unwrap().push(*value);
            if *value == BATCH_SIZE {
                tx.send(()).unwrap();
            }
            value * 2
        });
        let completed = completed.into_inner().unwrap();
        assert!(
            completed.iter().position(|value| *value == BATCH_SIZE)
                < completed.iter().position(|value| *value == 0)
        );
        assert_eq!(threads.into_inner().unwrap().len(), 4);
        assert_eq!(
            results,
            input.iter().map(|value| value * 2).collect::<Vec<_>>()
        );
    }

    #[test]
    #[should_panic(expected = "scan I/O worker panicked")]
    fn a_panicking_worker_does_not_leave_the_call_waiting() {
        let input: Vec<_> = (0..100).collect();
        let _ = map_ordered(&input, true, |value| {
            assert_ne!(*value, 13, "broken read task");
        });
    }
}
