//! Port of io.JobHandler: priority process pool + FIFO save pool + parse pool.

use parking_lot::{Condvar, Mutex};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

pub const PRIORITY_LOW: i64 = 20_000_000;
pub const PRIORITY_MEDIUM: i64 = 10_000_000;
pub const PRIORITY_HIGH: i64 = 0;

static JOB_ID: AtomicI64 = AtomicI64::new(0);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn active_jobs() -> usize {
    ACTIVE.load(Ordering::Relaxed)
}

/// A job carries its work plus a dynamic priority supplier.
pub struct Job {
    id: i64,
    base: i64,
    prio: Option<Box<dyn Fn() -> i64 + Send>>,
    work: Box<dyn FnOnce() + Send>,
}

impl Job {
    pub fn new(base: i64, prio: Option<Box<dyn Fn() -> i64 + Send>>, work: impl FnOnce() + Send + 'static) -> Self {
        Job {
            id: JOB_ID.fetch_add(1, Ordering::Relaxed),
            base,
            prio,
            work: Box::new(work),
        }
    }
    fn priority(&self) -> i64 {
        self.base + self.prio.as_ref().map(|p| p()).unwrap_or(0)
    }
    fn run(self) {
        (self.work)();
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Queue {
    heap: Mutex<BinaryHeap<HeapEntry>>,
    cv: Condvar,
}

struct HeapEntry {
    job: Job,
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
        // lower priority value = run first (Java PriorityBlockingQueue)
        o.job
            .priority()
            .cmp(&self.job.priority())
            .then_with(|| self.job.id.cmp(&o.job.id))
    }
}

pub struct Pools {
    process: Arc<Queue>,
}

static POOLS: std::sync::OnceLock<Pools> = std::sync::OnceLock::new();
static PROCESS_THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// app-configured process pool size (upstream Config.processThreads); must be
/// called before first pools() use.
pub fn set_process_threads(n: usize) {
    let _ = PROCESS_THREADS.set(n.max(1));
}

pub fn pools() -> &'static Pools {
    POOLS.get_or_init(|| {
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let process_threads = PROCESS_THREADS
            .get()
            .copied()
            .unwrap_or((cpus.saturating_sub(2).max(1)).min(4));
        Pools {
            process: spawn_pool(process_threads, "processPool"),
        }
    })
}

fn spawn_pool(n: usize, name: &str) -> Arc<Queue> {
    let q = Arc::new(Queue {
        heap: Mutex::new(BinaryHeap::new()),
        cv: Condvar::new(),
    });
    for _ in 0..n {
        let q = q.clone();
        let name = name.to_string();
        thread::Builder::new()
            .name(name)
            .spawn(move || loop {
                let job = {
                    let mut h = q.heap.lock();
                    loop {
                        if let Some(e) = h.pop() {
                            break e.job;
                        }
                        q.cv.wait(&mut h);
                    }
                };
                job.run();
            })
            .expect("spawn pool thread");
    }
    q
}

impl Pools {
    pub fn add_job(&self, job: Job) {
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        self.process.heap.lock().push(HeapEntry { job });
        self.process.cv.notify_one();
    }
    /// upstream ProcessDataWriterPool.
    pub fn add_process(&self, job: Job) {
        self.add_job(job);
    }

    

    pub fn clear_queues(&self) {
        let mut h = self.process.heap.lock();
        let n = h.len();
        h.clear();
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
