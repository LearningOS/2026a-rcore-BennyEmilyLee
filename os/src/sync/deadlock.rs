//! Deadlock detection for mutexes and semaphores
//!
//! Implements a Banker's-algorithm style safety check ([`ResourceSystem::is_safe`])
//! that is maintained incrementally as threads acquire/release the process's
//! synchronization resources. When deadlock detection is enabled for a process,
//! [`sys_enable_deadlock_detect`](crate::syscall::sync::sys_enable_deadlock_detect)
//! makes `mutex_lock` / `semaphore_down` reject an acquisition that would lead
//! the system into an unsafe (deadlocked) state with `-0xDEAD`.
//!
//! Mutexes and semaphores are detected independently, as the assignment permits:
//! the two resource classes never share a wait-for graph in this implementation.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Result of a resource acquisition request under deadlock detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireResult {
    /// The resource is currently free and is granted immediately.
    Ok,
    /// The resource is held by another thread; it is safe to block on it.
    WouldBlock,
    /// Granting/blocking on the resource would deadlock; the request must be rejected.
    Deadlock,
}

/// Banker's-algorithm state for one resource class (either mutexes or semaphores).
///
/// The classic Banker's data structures are kept here:
/// - `available[j]`: free units of resource `j`.
/// - `allocation[i][j]`: units of resource `j` currently held by thread `i`.
/// - `need[i][j]`: units of resource `j` that thread `i` will still request.
///
/// `wait_queue` mirrors the kernel's real FIFO waiter queue for each resource so
/// that a release can hand the resource to the exact thread the kernel wakes.
pub struct ResourceSystem {
    /// Free units per resource id.
    available: Vec<usize>,
    /// allocation[tid][res] = units held by thread `tid` of resource `res`.
    allocation: Vec<Vec<usize>>,
    /// need[tid][res] = remaining units thread `tid` will request of `res`.
    need: Vec<Vec<usize>>,
    /// Mirror of each resource's FIFO waiter queue (tids), in kernel wake order.
    wait_queue: Vec<VecDeque<usize>>,
}

impl ResourceSystem {
    /// Create an empty resource system.
    pub fn new() -> Self {
        Self {
            available: Vec::new(),
            allocation: Vec::new(),
            need: Vec::new(),
            wait_queue: Vec::new(),
        }
    }

    /// Ensure the columns for resource `res` exist in every row.
    fn ensure_res(&mut self, res: usize) {
        while self.available.len() <= res {
            self.available.push(0);
            self.wait_queue.push(VecDeque::new());
            for row in self.allocation.iter_mut() {
                row.push(0);
            }
            for row in self.need.iter_mut() {
                row.push(0);
            }
        }
    }

    /// Ensure the row for thread `tid` exists.
    fn ensure_thread(&mut self, tid: usize) {
        while self.allocation.len() <= tid {
            self.allocation.push(alloc::vec![0; self.available.len()]);
            self.need.push(alloc::vec![0; self.available.len()]);
        }
    }

    /// Register a freshly created resource with `units` free units.
    pub fn add_resource(&mut self, res: usize, units: usize) {
        self.ensure_res(res);
        self.available[res] = units;
    }

    /// Declare that thread `tid` requests one unit of resource `res`.
    ///
    /// Returns [`AcquireResult`]. On [`AcquireResult::WouldBlock`] the thread is
    /// recorded in the resource's waiter queue; on [`AcquireResult::Deadlock`]
    /// the declaration is rolled back so the caller can reject the request.
    pub fn acquire(&mut self, tid: usize, res: usize) -> AcquireResult {
        self.ensure_thread(tid);
        self.ensure_res(res);
        // The thread declares it still needs (at least) one more unit of `res`.
        self.need[tid][res] = core::cmp::max(self.need[tid][res], 1);
        if self.available[res] >= 1 {
            // Grant immediately; the caller's real lock/down will also succeed.
            self.available[res] -= 1;
            self.allocation[tid][res] += 1;
            self.need[tid][res] -= 1;
            AcquireResult::Ok
        } else {
            // The resource is exhausted; the thread would block. Accept the
            // request only if the resulting state is still safe.
            if self.is_safe() {
                self.wait_queue[res].push_back(tid);
                AcquireResult::WouldBlock
            } else {
                // Unsafe: roll back the need declaration.
                self.need[tid][res] = self.need[tid][res].saturating_sub(1);
                AcquireResult::Deadlock
            }
        }
    }

    /// Release one unit of resource `res` held by thread `tid`.
    ///
    /// If a thread is waiting on the resource, the unit is handed directly to the
    /// first waiter (matching the kernel's wake order); otherwise it returns to
    /// the free pool.
    pub fn release(&mut self, tid: usize, res: usize) {
        self.ensure_thread(tid);
        self.ensure_res(res);
        if let Some(waiter) = self.wait_queue[res].pop_front() {
            // Hand off to the woken waiter: the releaser loses a unit, the waiter gains one.
            self.allocation[waiter][res] += 1;
            self.need[waiter][res] = self.need[waiter][res].saturating_sub(1);
            self.allocation[tid][res] = self.allocation[tid][res].saturating_sub(1);
        } else {
            self.available[res] += 1;
            self.allocation[tid][res] = self.allocation[tid][res].saturating_sub(1);
        }
    }

    /// Mark thread `tid` as finished (exited).
    ///
    /// Its pending needs are cleared so the safety check treats it as a
    /// completed thread that can release whatever it holds; it is also removed
    /// from any waiter queue in case it exited while blocked.
    pub fn thread_finished(&mut self, tid: usize) {
        self.ensure_thread(tid);
        for res in 0..self.available.len() {
            self.need[tid][res] = 0;
            self.wait_queue[res].retain(|&t| t != tid);
        }
    }

    /// Reset thread `tid`'s row (used when a tid is recycled by a new thread).
    pub fn thread_reset(&mut self, tid: usize) {
        self.ensure_thread(tid);
        for res in 0..self.available.len() {
            self.allocation[tid][res] = 0;
            self.need[tid][res] = 0;
            self.wait_queue[res].retain(|&t| t != tid);
        }
    }

    /// Banker's safety algorithm.
    ///
    /// Returns `true` iff there exists an order in which every thread can obtain
    /// its remaining needs and run to completion. A `false` answer means the
    /// system is unsafe (a deadlock is possible / already present).
    fn is_safe(&self) -> bool {
        let n = self.allocation.len();
        let m = self.available.len();
        let mut work = self.available.clone();
        let mut finish = alloc::vec![false; n];
        loop {
            let mut progress = false;
            for i in 0..n {
                if finish[i] {
                    continue;
                }
                // Thread i can proceed if every remaining need is satisfiable.
                let can_finish = (0..m).all(|j| self.need[i][j] <= work[j]);
                if can_finish {
                    for j in 0..m {
                        work[j] += self.allocation[i][j];
                    }
                    finish[i] = true;
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        finish.iter().all(|&f| f)
    }
}

/// Per-process deadlock detection state.
///
/// Holds independent [`ResourceSystem`]s for the process's mutexes and
/// semaphores. `enabled` gates whether `mutex_lock` / `semaphore_down` consult
/// the detectors.
pub struct DeadlockState {
    /// Whether detection is currently enabled for this process.
    pub enabled: bool,
    /// Detector for the process's mutex resources.
    mutexes: ResourceSystem,
    /// Whether each mutex id is a blocking mutex (spin mutexes are not tracked).
    mutex_blocking: Vec<bool>,
    /// Detector for the process's semaphore resources.
    semaphores: ResourceSystem,
}

impl DeadlockState {
    /// Create a disabled deadlock-detection state.
    pub fn new() -> Self {
        Self {
            enabled: false,
            mutexes: ResourceSystem::new(),
            mutex_blocking: Vec::new(),
            semaphores: ResourceSystem::new(),
        }
    }

    /// Enable or disable detection.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Register a mutex resource. Only blocking mutexes participate in the
    /// wait-for graph; spin mutexes are recorded as inert.
    pub fn add_mutex(&mut self, res: usize, units: usize, blocking: bool) {
        self.mutexes.add_resource(res, units);
        while self.mutex_blocking.len() <= res {
            self.mutex_blocking.push(false);
        }
        self.mutex_blocking[res] = blocking;
    }

    /// Register a semaphore resource.
    pub fn add_semaphore(&mut self, res: usize, units: usize) {
        self.semaphores.add_resource(res, units);
    }

    /// Check a mutex acquisition request for thread `tid`.
    pub fn mutex_acquire(&mut self, tid: usize, res: usize) -> AcquireResult {
        if res >= self.mutex_blocking.len() || !self.mutex_blocking[res] {
            // Spin mutex: not part of the wait-for graph.
            return AcquireResult::Ok;
        }
        self.mutexes.acquire(tid, res)
    }

    /// Record a mutex release by thread `tid`.
    pub fn mutex_release(&mut self, tid: usize, res: usize) {
        if res >= self.mutex_blocking.len() || !self.mutex_blocking[res] {
            return;
        }
        self.mutexes.release(tid, res);
    }

    /// Check a semaphore acquisition request for thread `tid`.
    pub fn semaphore_acquire(&mut self, tid: usize, res: usize) -> AcquireResult {
        self.semaphores.acquire(tid, res)
    }

    /// Record a semaphore release by thread `tid`.
    pub fn semaphore_release(&mut self, tid: usize, res: usize) {
        self.semaphores.release(tid, res);
    }

    /// Mark thread `tid` as finished in both detectors.
    pub fn thread_finished(&mut self, tid: usize) {
        self.mutexes.thread_finished(tid);
        self.semaphores.thread_finished(tid);
    }

    /// Reset thread `tid`'s rows (tid recycled by a new thread).
    pub fn thread_reset(&mut self, tid: usize) {
        self.mutexes.thread_reset(tid);
        self.semaphores.thread_reset(tid);
    }
}
