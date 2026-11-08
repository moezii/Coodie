//! Order-preserving parallel map over a slice, on scoped std threads.
//!
//! Results come back in input order whatever order the workers finish in, so
//! anything built from them is identical to a sequential run. Work is handed
//! out one item at a time from a shared counter, which keeps a few very large
//! files from stranding one worker with a long tail.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Worker count: `CODIE_THREADS` if set to a positive integer, else the
/// machine's available parallelism.
pub(crate) fn thread_count() -> usize {
    std::env::var("CODIE_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        })
}

/// `items.iter().map(f).collect()`, spread over up to `threads` threads.
pub(crate) fn map<T, R, F>(items: &[T], threads: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = threads.min(items.len());
    if workers <= 1 {
        return items.iter().map(f).collect();
    }

    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else { break };
                        done.push((index, f(item)));
                    }
                    done
                })
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(done) => {
                    for (index, result) in done {
                        slots[index] = Some(result);
                    }
                }
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    slots
        .into_iter()
        .map(|slot| slot.expect("every index is claimed by exactly one worker"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_input_order_at_any_thread_count() {
        let items: Vec<usize> = (0..1000).collect();
        let expected: Vec<usize> = items.iter().map(|value| value * 3).collect();
        for threads in [1, 2, 3, 8, 64, 5000] {
            assert_eq!(map(&items, threads, |value| value * 3), expected);
        }
    }

    #[test]
    fn handles_empty_input() {
        let items: Vec<u8> = Vec::new();
        assert!(map(&items, 8, |value| *value).is_empty());
    }
}
