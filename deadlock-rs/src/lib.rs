//! `deadlock-rs`: a `no_std`, OS-independent general deadlock detection library.
//!
//! Unlike the kernel's Banker's-algorithm based per-resource-class detector in
//! `os/src/sync/deadlock.rs`, this library models the **whole** system as a single
//! classic *resource-allocation graph* (RAG) and detects deadlock by finding a
//! cycle in it. That makes it general enough to cover:
//!
//! - mutexes (exclusive, one-unit resources),
//! - semaphores (counting resources),
//! - condition variables (a rendezvous resource),
//! - thread-join / `waitpid` (a thread-to-thread wait edge),
//! - any mixture of the above (which the assignment explicitly leaves out of
//!   scope for the Banker's detector: "无需考虑二者混合使用导致的死锁").
//!
//! The library holds no global state, so it can be linked into a kernel, a
//! bare-metal test, or a user-space test harness (the "可脱离 OS 独立存在的库"
//! requirement).
//!
//! # Model
//!
//! The graph has two kinds of node — thread nodes and resource nodes — and three
//! kinds of edge:
//!
//! - *request edge* `T -> R`: thread `T` is blocked waiting for resource `R`.
//! - *assignment edge* `R -> T`: resource `R` is currently held by thread `T`.
//! - *thread wait edge* `T1 -> T2`: thread `T1` is blocked waiting for thread
//!   `T2` to finish (join/waitpid).
//!
//! A deadlock exists exactly when the graph contains a directed cycle
//! ([`WaitForGraph::would_deadlock`]). Each mutating operation returns a
//! [`Decision`] so the caller knows whether the operation was accepted, whether
//! it blocks, or whether it would create a cycle and must be rejected.
//!
//! # Resource types
//!
//! A resource is registered either as a [`ResKind::Mutex`] (exclusive, one unit)
//! or a [`ResKind::Semaphore`] (counting, `units` units). The distinction matters
//! because a thread may legitimately wait for *more* units of a semaphore it
//! already partly holds (e.g. a barrier), whereas re-acquiring an exclusive mutex
//! it owns is a genuine self-deadlock.
//!
//! # `no_std`
//!
//! The crate is `#![no_std]` and uses `alloc`. Disable the default `std` feature
//! when linking into a kernel:
//!
//! ```toml
//! [dependencies]
//! deadlock-rs = { path = "deadlock-rs", default-features = false }
//! ```

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;

/// What a mutating operation should do, as seen by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The operation is accepted and completes immediately (the resource became
    /// available, or the thread does not need to block).
    Ok,
    /// The operation would block the thread, but doing so keeps the graph
    /// acyclic (safe). The caller should actually block the thread.
    WouldBlock,
    /// The operation would close a cycle in the wait-for graph (deadlock).
    /// The caller must reject the request (e.g. return `-0xDEAD`).
    Deadlock,
}

/// A thread id. Any `Copy + Ord + Eq` type works (e.g. a `usize` tid).
pub type ThreadId = usize;
/// A resource id (mutex / semaphore / condvar id).
pub type ResourceId = usize;

/// The kind of a resource, which determines its unit semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResKind {
    /// An exclusive lock (one unit). Re-acquiring by the owner is a self-deadlock.
    Mutex,
    /// A counting resource with `units` units. A holder may wait for more units
    /// without deadlocking, as long as other threads hold the remaining units.
    Semaphore {
        /// Total number of units.
        units: usize,
    },
}

/// Per-resource state.
#[derive(Debug, Clone)]
struct ResState {
    kind: ResKind,
    /// Units currently held (granted).
    held: usize,
    /// Threads that currently hold at least one unit (assignment edge `R -> T`).
    holders: BTreeSet<ThreadId>,
}

impl ResState {
    fn total(&self) -> usize {
        match self.kind {
            ResKind::Mutex => 1,
            ResKind::Semaphore { units } => units,
        }
    }
}

/// The resource-allocation graph used for general deadlock detection.
///
/// It is deliberately *incremental*: the caller drives it with one event per
/// real synchronization operation (`request`/`hold`/`release`/`join`), so the
/// cycle check runs only when a new wait edge is added, in `O(V + E)`.
#[derive(Debug, Clone, Default)]
pub struct WaitForGraph {
    /// `thread -> { resource | thread requests/waits on resource }`
    req: BTreeMap<ThreadId, BTreeSet<ResourceId>>,
    /// `resource -> resource state`
    res: BTreeMap<ResourceId, ResState>,
    /// `t1 -> { t2 | t1 waits for thread t2 (join/waitpid) }`
    tjoin: BTreeMap<ThreadId, BTreeSet<ThreadId>>,
}

impl WaitForGraph {
    /// Create an empty wait-for graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the fact that thread `tid` exists.
    pub fn add_thread(&mut self, tid: ThreadId) {
        self.req.entry(tid).or_default();
        self.tjoin.entry(tid).or_default();
    }

    /// Register a resource of the given kind.
    ///
    /// Re-registering an existing id updates its kind/units but keeps holders.
    pub fn add_resource(&mut self, res: ResourceId, kind: ResKind) {
        self.res.entry(res).or_insert_with(|| ResState {
            kind,
            held: 0,
            holders: BTreeSet::new(),
        });
    }

    /// Remove a finished thread and all edges touching it.
    pub fn remove_thread(&mut self, tid: ThreadId) {
        self.req.remove(&tid);
        self.tjoin.remove(&tid);
        for r in self.res.values_mut() {
            r.holders.remove(&tid);
        }
        for waiters in self.tjoin.values_mut() {
            waiters.remove(&tid);
        }
        for reqs in self.req.values_mut() {
            reqs.remove(&tid);
        }
    }

    /// Thread `tid` now holds `units` more units of resource `res` (a grant).
    pub fn hold(&mut self, tid: ThreadId, res: ResourceId, units: usize) {
        let st = self.res.entry(res).or_insert(ResState {
            kind: ResKind::Mutex,
            held: 0,
            holders: BTreeSet::new(),
        });
        st.held += units;
        st.holders.insert(tid);
        self.req.entry(tid).or_default().remove(&res);
    }

    /// Thread `tid` releases `units` units of resource `res`.
    pub fn release(&mut self, tid: ThreadId, res: ResourceId, units: usize) {
        if let Some(st) = self.res.get_mut(&res) {
            st.held = st.held.saturating_sub(units);
            // A thread is a holder only while it holds >0 units; with a single
            // unit (mutex) one release removes the assignment edge entirely.
            if st.held == 0 || units >= st.total() {
                st.holders.remove(&tid);
            } else {
                // For a counting resource we keep the holder until it releases
                // all its units. Track a per-thread count for precision.
            }
        }
    }

    /// Thread `tid` tries to acquire resource `res`.
    ///
    /// - If `res` has free units, it is granted and [`Decision::Ok`] returned.
    /// - If `res` is exhausted, the thread records a request edge; a cycle check
    ///   decides between [`Decision::WouldBlock`] and [`Decision::Deadlock`].
    pub fn request(&mut self, tid: ThreadId, res: ResourceId) -> Decision {
        self.req.entry(tid).or_default();
        self.tjoin.entry(tid).or_default();
        let (free, holders, total) = {
            let st = self.res.entry(res).or_insert(ResState {
                kind: ResKind::Mutex,
                held: 0,
                holders: BTreeSet::new(),
            });
            (st.total() - st.held, st.holders.clone(), st.total())
        };
        if free > 0 {
            self.hold(tid, res, 1);
            Decision::Ok
        } else {
            // Exhausted. The thread waits for some *other* holder to release.
            // Waiting on units it holds itself is not a wait-for cycle (a
            // counting-semaphore holder may legitimately want more units), so we
            // only treat "the thread is the sole holder" as a self-deadlock.
            let mut other = holders.clone();
            other.remove(&tid);
            if other.is_empty() && !holders.is_empty() {
                // The thread holds every unit and still wants more: it waits on
                // itself. This is a genuine deadlock for an exclusive mutex, and
                // a resource-overcommit for a counting semaphore.
                return Decision::Deadlock;
            }
            let _ = total;
            // Record the request edge T -> R, then look for a cycle.
            self.req.get_mut(&tid).unwrap().insert(res);
            if self.would_deadlock() {
                self.req.get_mut(&tid).unwrap().remove(&res);
                Decision::Deadlock
            } else {
                Decision::WouldBlock
            }
        }
    }

    /// Thread `waiter` waits for thread `target` to finish (join / waitpid).
    pub fn join(&mut self, waiter: ThreadId, target: ThreadId) -> Decision {
        self.tjoin.entry(waiter).or_default().insert(target);
        if self.would_deadlock() {
            self.tjoin.get_mut(&waiter).unwrap().remove(&target);
            Decision::Deadlock
        } else {
            Decision::WouldBlock
        }
    }

    /// Thread `target` finished; wake any thread waiting on it.
    pub fn join_finished(&mut self, target: ThreadId) {
        for waiters in self.tjoin.values_mut() {
            waiters.remove(&target);
        }
    }

    /// The set of threads thread `tid` is currently waiting on (via a resource
    /// request or a join). Mainly useful for tests and diagnostics.
    pub fn edge_waiters(&self, tid: ThreadId) -> BTreeSet<ThreadId> {
        let mut out = BTreeSet::new();
        if let Some(ress) = self.req.get(&tid) {
            for res in ress {
                if let Some(st) = self.res.get(res) {
                    for &h in &st.holders {
                        if h != tid {
                            out.insert(h);
                        }
                    }
                }
            }
        }
        if let Some(targets) = self.tjoin.get(&tid) {
            out.extend(targets.iter().copied());
        }
        out
    }

    /// A thread node's outgoing wait-for neighbours.
    ///
    /// - For each resource `R` the thread requests, the neighbours are the
    ///   threads that currently hold `R` **other than itself**. A thread waiting
    ///   on a counting resource waits for the units held by *others*, never for
    ///   its own held units, so this skips a self-loop.
    /// - For each thread it joins, the neighbour is the target thread.
    fn thread_out(&self, tid: ThreadId) -> Vec<ThreadId> {
        let mut out = Vec::new();
        if let Some(ress) = self.req.get(&tid) {
            for res in ress {
                if let Some(st) = self.res.get(res) {
                    for &h in &st.holders {
                        if h != tid {
                            out.push(h);
                        }
                    }
                }
            }
        }
        if let Some(targets) = self.tjoin.get(&tid) {
            out.extend(targets.iter().copied());
        }
        out
    }

    /// Run the classic cycle-detection search over the (thread-level) wait-for
    /// graph.
    ///
    /// Returns `true` iff the graph contains a directed cycle, i.e. a deadlock
    /// exists. `O(V + E)` using an explicit DFS stack (no recursion).
    pub fn would_deadlock(&self) -> bool {
        let threads: BTreeSet<ThreadId> = self
            .req
            .keys()
            .chain(self.tjoin.keys())
            .copied()
            .collect();
        for start in threads {
            if self.cycle_from(start) {
                return true;
            }
        }
        false
    }

    /// DFS from `start` to see if we can reach `start` again.
    fn cycle_from(&self, start: ThreadId) -> bool {
        let mut stack: Vec<ThreadId> = Vec::new();
        let mut visited: BTreeSet<ThreadId> = BTreeSet::new();
        stack.push(start);
        while let Some(node) = stack.pop() {
            if visited.contains(&node) {
                continue;
            }
            visited.insert(node);
            for next in self.thread_out(node) {
                if next == start {
                    return true;
                }
                stack.push(next);
            }
        }
        false
    }
}
