//! Parallel per-file extraction with a deterministic, in-order merge (#236).
//!
//! Per-file extraction is independent, so workers parse files concurrently,
//! but every result is handed back on the calling thread **in walk order**.
//! The merged output is therefore byte-identical to a serial run for any
//! worker count (US-0014): parallelism changes only wall-clock time.
//!
//! Memory: workers never run more than a small window ahead of the in-order
//! merge, so the results held at once stay proportional to the worker count,
//! not the repository. The accumulated graph is built once, on the calling
//! thread, exactly as in a serial run.

use std::any::Any;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

/// Largest worker count accepted from a setting.
pub const MAX_WORKERS: usize = 64;

/// How many extraction workers to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parallelism {
    /// [`auto_workers`]: non-efficiency cores − 1, capped by physical memory.
    Auto,
    /// Exactly this many workers (clamped to `1..=MAX_WORKERS`); `1` is serial.
    Fixed(usize),
}

impl Parallelism {
    /// The worker count this setting resolves to on this machine.
    #[must_use]
    pub fn workers(self) -> usize {
        match self {
            Self::Auto => auto_workers(),
            Self::Fixed(workers) => workers.clamp(1, MAX_WORKERS),
        }
    }
}

/// Process-wide setting: 0 = Auto, otherwise a fixed worker count.
static CONFIGURED: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static OVERRIDE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Apply the user's "Ingest parallelism" setting to every later extraction.
pub fn set_parallelism(parallelism: Parallelism) {
    let raw = match parallelism {
        Parallelism::Auto => 0,
        Parallelism::Fixed(workers) => workers.clamp(1, MAX_WORKERS),
    };
    CONFIGURED.store(raw, Ordering::Relaxed);
}

/// The process-wide setting last applied with [`set_parallelism`].
#[must_use]
pub fn parallelism() -> Parallelism {
    match CONFIGURED.load(Ordering::Relaxed) {
        0 => Parallelism::Auto,
        workers => Parallelism::Fixed(workers),
    }
}

/// Run `body` with extractions on this thread using exactly `workers`
/// workers, regardless of the process-wide setting — for callers (and
/// determinism tests) that must pin the worker count.
pub fn with_workers<R>(workers: usize, body: impl FnOnce() -> R) -> R {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|cell| cell.replace(Some(workers.clamp(1, MAX_WORKERS)))));
    body()
}

/// The worker count extractions on this thread use now.
#[must_use]
pub fn workers() -> usize {
    OVERRIDE
        .with(Cell::get)
        .unwrap_or_else(|| parallelism().workers())
}

/// `Auto`: one worker per performance (non-efficiency) core, leaving one for
/// the UI and the merge (`max(1, P-cores − 1)`), and at most one worker per 2 GiB of
/// physical memory so a large ingest degrades to fewer workers rather than
/// swapping. Computed once per process.
#[must_use]
pub fn auto_workers() -> usize {
    static AUTO: OnceLock<usize> = OnceLock::new();
    *AUTO.get_or_init(|| {
        let cores = performance_cores().saturating_sub(1).max(1);
        let memory_cap = physical_memory_bytes()
            .map(|bytes| usize::try_from(bytes / (2 << 30)).unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX)
            .max(1);
        cores.min(memory_cap).min(MAX_WORKERS)
    })
}

/// Cores outside the efficiency cluster where the platform reports clusters
/// (Apple silicon: every `hw.perflevelN` not named "Efficiency", so both the
/// "Super" and "Performance" clusters of newer chips count), otherwise the
/// available parallelism.
fn performance_cores() -> usize {
    #[cfg(target_os = "macos")]
    if let Some(levels) = sysctl("hw.nperflevels").and_then(|v| v.parse::<usize>().ok()) {
        let cores: usize = (0..levels)
            .filter(|level| {
                sysctl(&format!("hw.perflevel{level}.name")).is_some_and(|n| n != "Efficiency")
            })
            .filter_map(|level| {
                sysctl(&format!("hw.perflevel{level}.physicalcpu"))?
                    .parse::<usize>()
                    .ok()
            })
            .sum();
        if cores > 0 {
            return cores;
        }
    }
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

fn physical_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        sysctl("hw.memsize").and_then(|v| v.parse().ok())
    }
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib: u64 = meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(kib * 1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn sysctl(name: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

type Outcome<T, E> = Result<Result<T, E>, Box<dyn Any + Send>>;

struct State<T, E> {
    next: usize,
    merged: usize,
    stop: bool,
    ready: BTreeMap<usize, Outcome<T, E>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `work` over every item on up to [`workers`] threads and hand each
/// result to `merge` on the calling thread, strictly in item order.
///
/// The observable behaviour equals the serial loop
/// `for item { merge(item, work(item)?)? }`: the first error in item order is
/// returned, nothing after it is merged, and a panic in `work` resumes on the
/// calling thread. Only the timing differs.
pub fn map_ordered<T: Send, E: Send>(
    items: &[String],
    work: impl Fn(&str) -> Result<T, E> + Sync,
    mut merge: impl FnMut(&str, T) -> Result<(), E>,
) -> Result<(), E> {
    let workers = workers().min(items.len());
    if workers <= 1 {
        for item in items {
            merge(item, work(item)?)?;
        }
        return Ok(());
    }
    let window = workers * 4;
    let state = Mutex::new(State {
        next: 0,
        merged: 0,
        stop: false,
        ready: BTreeMap::new(),
    });
    let claimable = Condvar::new();
    let finished = Condvar::new();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut guard = lock(&state);
                        loop {
                            if guard.stop || guard.next >= items.len() {
                                return;
                            }
                            if guard.next < guard.merged + window {
                                break;
                            }
                            guard = claimable
                                .wait(guard)
                                .unwrap_or_else(PoisonError::into_inner);
                        }
                        guard.next += 1;
                        guard.next - 1
                    };
                    let outcome = catch_unwind(AssertUnwindSafe(|| work(&items[index])));
                    lock(&state).ready.insert(index, outcome);
                    finished.notify_all();
                }
            });
        }
        let result = (|| {
            for (index, item) in items.iter().enumerate() {
                let outcome = {
                    let mut guard = lock(&state);
                    loop {
                        if let Some(outcome) = guard.ready.remove(&index) {
                            guard.merged = index + 1;
                            break outcome;
                        }
                        guard = finished.wait(guard).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                claimable.notify_all();
                match outcome {
                    Ok(value) => merge(item, value?)?,
                    Err(panic) => {
                        stop(&state, &claimable);
                        resume_unwind(panic);
                    }
                }
            }
            Ok(())
        })();
        stop(&state, &claimable);
        result
    })
}

fn stop<T, E>(state: &Mutex<State<T, E>>, claimable: &Condvar) {
    lock(state).stop = true;
    claimable.notify_all();
}
