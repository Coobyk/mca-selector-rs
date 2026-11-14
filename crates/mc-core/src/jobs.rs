//! Port of io.JobHandler: priority process pool + FIFO save pool + parse pool.

use parking_lot::{Condvar, Mutex};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

pub const PRIORITY_LOW: i64 = 20_000_000;
pub const PRIORITY_MEDIUM: i64 = 10_000_000;

static JOB_ID: AtomicI64 = AtomicI64::new(0);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn active_jobs() -> usize {
    ACTIVE.load(Ordering::Relaxed)
}

/// A job carries its work plus an optional priority offset. Tile-map jobs
/// pass an offset computed from queue pressure at enqueue (bounded by
/// [`MAX_DYNAMIC_PRIO`]); everything else passes `None`.
pub struct Job {
    id: i64,
    base: i64,
    prio: i64,
    work: Box<dyn FnOnce() + Send>,
}

impl Job {
    pub fn new(base: i64, prio: Option<i64>, work: impl FnOnce() + Send + 'static) -> Self {
        Job {
            id: JOB_ID.fetch_add(1, Ordering::Relaxed),
            base,
            prio: prio.unwrap_or(0),
            work: Box::new(work),
        }
    }
    /// effective priority: base + enqueue-time offset (see HeapEntry::prio).
    fn priority(&self) -> i64 {
        self.base + self.prio
    }
    fn run(self) {
        // a panicking job must not take its worker thread down — upstream
        // runs on an ExecutorService which replaces dead workers. Without
        // this the pool silently shrank (progressive lag) and ACTIVE leaked,
        // leaving the UI in permanent "jobs in queue" repaint mode.
        let Job { work, .. } = self;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
        if let Err(e) = result {
            let msg = e
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("unknown panic");
            eprintln!("process pool job panicked: {msg}");
        }
    }
}

pub struct Queue {
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// Two priority lanes behind one lock + condvar.
///
/// `fast` holds jobs whose *base* priority is better than PRIORITY_LOW
/// (MEDIUM/HIGH: overlay parses, png loads); `slow` holds the bulk
/// render/batch-op backlog. Callers never let the dynamic prio offset cross
/// the class boundary (offsets are in 1..=MAX_DYNAMIC_PRIO, classes are 10M apart),
/// so classifying by `base` is equivalent to classifying by effective
/// priority — and it lets one reserved worker serve `fast` exclusively
/// without a slow job ever masking a fast one underneath it in the heap.
#[derive(Default)]
struct Inner {
    fast: BinaryHeap<HeapEntry>,
    slow: BinaryHeap<HeapEntry>,
}

struct HeapEntry {
    job: Job,
    /// effective priority, snapshotted at enqueue. Freezing it here keeps
    /// the thunk out of the queue lock (it ran O(log n) times per push/pop
    /// during heap sift) — with the side benefit that a future *dynamic*
    /// thunk can't make an entry's ordering inconsistent mid-sift.
    prio: i64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.job.id == other.job.id
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // lower priority value = run first (Java PriorityBlockingQueue);
        // both keys must be reversed for the max-heap so that the smallest
        // (priority, id) tuple pops first — the id tie-break was forward,
        // making equal-priority jobs run newest-first (LIFO).
        o.prio
            .cmp(&self.prio)
            .then_with(|| o.job.id.cmp(&self.job.id))
    }
}

/// dynamic prio offsets must stay below this to keep the class boundary:
/// PRIORITY classes are 10M apart and lanes classify on the base value.
pub const MAX_DYNAMIC_PRIO: i64 = 9_999_999;

static QUEUE: std::sync::OnceLock<Queue> = std::sync::OnceLock::new();
static PROCESS_THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static SPAWNED: std::sync::Once = std::sync::Once::new();

/// app-configured process pool size (upstream Config.processThreads); must be
/// called before first pools() use.
pub fn set_process_threads(n: usize) {
    let _ = PROCESS_THREADS.set(n.max(1));
}

pub fn pools() -> &'static Queue {
    let q = QUEUE.get_or_init(|| Queue {
        inner: Mutex::new(Inner::default()),
        cv: Condvar::new(),
    });
    SPAWNED.call_once(|| {
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        // default: all cores minus two (one for the UI thread, one of OS
        // headroom), at least 4, and no fixed ceiling — render jobs scale
        // linearly with cores on big machines. `process_threads` config
        // overrides this via set_process_threads().
        let default_threads = cpus.saturating_sub(2).max(4);
        let n = PROCESS_THREADS
            .get()
            .copied()
            .unwrap_or(default_threads)
            .max(1);
        for _ in 0..n {
            spawn_worker(q, "processPool", Lane::Any);
        }
        // the express worker is extra on top of the configured/general
        // count, so `process_threads` still means "bulk worker count" and
        // the reservation costs nothing when the queue is calm.
        spawn_worker(q, "processPool-fast", Lane::FastOnly);
    });
    q
}

/// Which lanes a worker may drain.
#[derive(Clone, Copy)]
enum Lane {
    /// general worker: fast lane first, then slow.
    Any,
    /// reserved express worker: fast lane only. Guarantees a MEDIUM/HIGH job
    /// (overlay, png load) never waits behind an in-flight LOW render — the
    /// priority heap already orders queued jobs, but cannot preempt a running
    /// one. Costs one mostly-idle thread while the fast lane is empty.
    FastOnly,
}

fn spawn_worker(q: &'static Queue, name: &str, lane: Lane) {
    let name = name.to_string();
    thread::Builder::new()
        .name(name)
        .spawn(move || loop {
            let job = {
                let mut g = q.inner.lock();
                loop {
                    let entry = match lane {
                        Lane::Any => g.fast.pop().or_else(|| g.slow.pop()),
                        Lane::FastOnly => match g.fast.pop() {
                            some @ Some(_) => some,
                            None => {
                                // the notify that woke us may have been for a
                                // slow-lane push: relay it to a general worker
                                // so the wakeup isn't swallowed. No-op when no
                                // general is waiting (they'll see the job when
                                // they finish their current one).
                                if !g.slow.is_empty() {
                                    q.cv.notify_one();
                                }
                                None
                            }
                        },
                    };
                    if let Some(e) = entry {
                        break e.job;
                    }
                    q.cv.wait(&mut g);
                }
            };
            job.run();
        })
        .expect("spawn pool thread");
}

impl Queue {
    /// Enqueue a job. Upstream had two pools (ProcessDataWriterPool for batch
    /// ops, a process pool for the rest); here both collapse onto the one
    /// two-lane queue described at [`Inner`].
    pub fn add_process(&self, job: Job) {
        let entry = HeapEntry {
            prio: job.priority(),
            job,
        };
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        {
            let mut g = self.inner.lock();
            if entry.job.base < PRIORITY_LOW {
                g.fast.push(entry);
            } else {
                g.slow.push(entry);
            }
        }
        // notify after releasing the lock so the woken worker doesn't bounce
        // off a still-held mutex. notify_one is enough: every waiter pops at
        // most one job, and the FastOnly worker relays a slow-lane wakeup.
        self.cv.notify_one();
    }

    pub fn clear_queues(&self) {
        let n = {
            let mut g = self.inner.lock();
            let n = g.fast.len() + g.slow.len();
            g.fast.clear();
            g.slow.clear();
            n
        };
        if n > 0 {
            ACTIVE.fetch_sub(n, Ordering::Relaxed);
        }
    }
}

// -- progress tracking --------------------------------------------------------

/// Shared progress state for a batch operation (port of upstream `Progress`).
pub struct Progress {
    pub title: String,
    pub total: AtomicI64,
    pub done: AtomicI64,
    pub current: Mutex<String>,
    pub cancelled: std::sync::atomic::AtomicBool,
}

impl Progress {
    pub fn new(title: impl Into<String>, total: i64) -> Arc<Self> {
        let p = Arc::new(Self {
            title: title.into(),
            total: AtomicI64::new(total.max(0)),
            done: AtomicI64::new(0),
            current: Mutex::new(String::new()),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        });
        *CURRENT.lock() = Arc::downgrade(&p);
        p
    }

    pub fn set_current(&self, s: impl Into<String>) {
        *self.current.lock() = s.into();
    }

    pub fn inc(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// fraction done in [0,1]; total==0 => indeterminate (returns -1).
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t <= 0 { return -1.0; }
        (self.done.load(Ordering::Relaxed) as f32 / t as f32).clamp(0.0, 1.0)
    }

    /// guard that increments `done` when the iteration ends (drop, continue or break).
    pub fn inc_guard(&self) -> IncGuard<'_> {
        IncGuard(self)
    }
}

pub struct IncGuard<'a>(&'a Progress);
impl Drop for IncGuard<'_> {
    fn drop(&mut self) {
        self.0.inc();
    }
}

static CURRENT: Mutex<std::sync::Weak<Progress>> = Mutex::new(std::sync::Weak::new());

/// the in-flight batch progress, if the owning job is still alive.
pub fn current_progress() -> Option<Arc<Progress>> {
    CURRENT.lock().upgrade()
}

pub fn cancel_progress() {
    if let Some(p) = CURRENT.lock().upgrade() {
        p.cancel();
    }
}
