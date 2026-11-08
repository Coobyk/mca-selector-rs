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
        ACTIVE.fetch_add(1, Ordering::Relaxed);
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
    save: Arc<Queue>,
    parse: Arc<Queue>,
}

static POOLS: std::sync::OnceLock<Pools> = std::sync::OnceLock::new();

pub fn pools() -> &'static Pools {
    POOLS.get_or_init(|| {
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let process_threads = (cpus.saturating_sub(2).max(1)).min(4);
        let write_threads = cpus.min(4);
        Pools {
            process: spawn_pool(process_threads, "processPool"),
            save: spawn_pool(write_threads, "savePool"),
            parse: spawn_pool(1, "parsePool"),
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
        self.process.heap.lock().push(HeapEntry { job });
        self.process.cv.notify_one();
    }
    pub fn add_process(&self, job: Job) {
        self.add_job(job);
    }
    pub fn save(&self, job: Job) {
        self.save.heap.lock().push(HeapEntry { job });
        self.save.cv.notify_one();
    }
    pub fn parse(&self, job: Job) {
        self.parse.heap.lock().push(HeapEntry { job });
        self.parse.cv.notify_one();
    }

    /// remove queued jobs matching predicate (process + parse pools).
    /// Cancelled jobs are dropped without running (ACTIVE decremented).
    pub fn validate_jobs(&self, p: impl Fn(&Job) -> bool) {
        for q in [&self.process, &self.parse] {
            let mut h = q.heap.lock();
            let mut keep = BinaryHeap::new();
            for e in h.drain() {
                if p(&e.job) {
                    ACTIVE.fetch_sub(1, Ordering::Relaxed);
                } else {
                    keep.push(e);
                }
            }
            *h = keep;
        }
    }

    pub fn clear_queues(&self) {
        for q in [&self.process, &self.save, &self.parse] {
            let mut h = q.heap.lock();
            let n = h.len();
            h.clear();
            if n > 0 {
                ACTIVE.fetch_sub(n, Ordering::Relaxed);
            }
        }
    }

    pub fn cancel_parser_queue(&self) {
        let mut h = self.parse.heap.lock();
        let n = h.len();
        h.clear();
        if n > 0 {
            ACTIVE.fetch_sub(n, Ordering::Relaxed);
        }
    }
}
