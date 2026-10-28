//! Tests for `deadlock-rs`: general wait-for-graph deadlock detection.
//!
//! Pure-logic tests (no kernel, no threads): each one drives the
//! [`WaitForGraph`] with a scripted sequence of hold/request/release/join events
//! and asserts the decision at each step. They cover the scenarios the kernel's
//! Banker's detector handles (mutex-only, semaphore-only) *and* the mixed
//! scenarios it deliberately leaves out of scope.

use deadlock_rs::{Decision, ResKind, WaitForGraph};

fn mutex(g: &mut WaitForGraph, id: usize) {
    g.add_resource(id, ResKind::Mutex);
}

fn semaphore(g: &mut WaitForGraph, id: usize, units: usize) {
    g.add_resource(id, ResKind::Semaphore { units });
}

/// The classic ABBA mutex deadlock:
/// T1 holds M0 and wants M1; T2 holds M1 and wants M0.
#[test]
fn mutex_abba_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    mutex(&mut g, 0);
    mutex(&mut g, 1);
    g.hold(1, 0, 1); // T1 owns M0
    g.hold(2, 1, 1); // T2 owns M1

    assert_eq!(g.request(1, 1), Decision::WouldBlock); // T1 waits for M1 (held by T2)
    assert_eq!(g.request(2, 0), Decision::Deadlock); // T2 waits for M0 -> cycle
}

/// Re-acquiring an exclusive mutex the thread already owns is a self-deadlock.
#[test]
fn mutex_recursive_is_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    mutex(&mut g, 0);
    g.hold(1, 0, 1);
    assert_eq!(g.request(1, 0), Decision::Deadlock);
}

/// The same ABBA order but with a free intermediate step: no deadlock.
#[test]
fn mutex_abba_safe_when_one_releases() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    mutex(&mut g, 0);
    mutex(&mut g, 1);
    g.hold(1, 0, 1);
    g.hold(2, 1, 1);
    assert_eq!(g.request(1, 1), Decision::WouldBlock);
    // T2 releases M1 before acquiring M0 -> T1 gets M1, no cycle.
    g.release(2, 1, 1);
    g.hold(1, 1, 1); // T1 now owns M1
    // T2 wants M0 (held by T1); T1 no longer waits on anything, so no cycle.
    assert_eq!(g.request(2, 0), Decision::WouldBlock);
}

/// A thread waiting for more units of a semaphore it partly holds is NOT a
/// self-deadlock, as long as others hold the remaining units.
#[test]
fn semaphore_partial_hold_not_self_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    semaphore(&mut g, 0, 2); // 2 units
    g.hold(1, 0, 1);
    g.hold(2, 0, 1);
    // Both units held; T1 wants a 3rd (impossible) but T2 holds the other -> block.
    assert_eq!(g.request(1, 0), Decision::WouldBlock);
}

/// Semaphore deadlock: two threads each hold one unit and both want the other's
/// via a *second* resource, forming a hold-and-wait cycle.
#[test]
fn semaphore_deadlock_with_second_resource() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    semaphore(&mut g, 0, 2); // S: 2 units
    mutex(&mut g, 1); // M1
    g.hold(1, 0, 1);
    g.hold(2, 0, 1);
    g.hold(1, 1, 1); // T1 also owns M1
    // T2 wants M1 (held by T1); T1 wants more of S (held by T2) -> cycle.
    assert_eq!(g.request(2, 1), Decision::WouldBlock); // T2 -> M1 -> T1
    assert_eq!(g.request(1, 0), Decision::Deadlock); // T1 -> S -> T2 -> M1 -> T1
}

/// Thread-join: T1 joins T2 while T2 joins T1 -> cycle (mutual join).
#[test]
fn join_mutual_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    assert_eq!(g.join(1, 2), Decision::WouldBlock);
    assert_eq!(g.join(2, 1), Decision::Deadlock);
}

/// A thread joining itself is an immediate cycle.
#[test]
fn join_self_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    assert_eq!(g.join(1, 1), Decision::Deadlock);
}

/// A join chain with no cycle is safe; finishing the target removes the edge.
#[test]
fn join_chain_safe() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    g.add_thread(3);
    assert_eq!(g.join(1, 2), Decision::WouldBlock);
    assert_eq!(g.join(2, 3), Decision::WouldBlock);
    g.join_finished(3); // T3 finishes
    assert!(g.edge_waiters(2).is_empty());
}

/// The mixed mutex+semaphore case the assignment says the Banker's detector
/// need not cover: a cycle that spans *both* resource classes. The general
/// detector must catch it.
#[test]
fn mixed_mutex_and_semaphore_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    mutex(&mut g, 0); // M0
    semaphore(&mut g, 1, 1); // S0, 1 unit
    g.hold(1, 0, 1); // T1 holds M0
    g.hold(2, 1, 1); // T2 holds S0
    assert_eq!(g.request(1, 1), Decision::WouldBlock); // T1 waits on S0 (held by T2)
    assert_eq!(g.request(2, 0), Decision::Deadlock); // T2 waits on M0 -> cycle across classes
}

/// A safe interleaving must never report deadlock.
#[test]
fn safe_interleaving_no_false_positive() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    g.add_thread(3);
    mutex(&mut g, 0);
    mutex(&mut g, 1);
    mutex(&mut g, 2);
    g.hold(1, 0, 1);
    g.hold(2, 1, 1);
    g.hold(3, 2, 1);
    assert_eq!(g.request(1, 1), Decision::WouldBlock); // T1 -> T2
    assert_eq!(g.request(3, 0), Decision::WouldBlock); // T3 -> T1 (no cycle, acyclic)
    g.release(2, 1, 1);
    g.hold(1, 1, 1);
    g.release(1, 0, 1);
    g.hold(3, 0, 1);
}

/// Removing a finished thread cleans up its edges and can break a would-be cycle.
#[test]
fn remove_thread_breaks_cycle() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    mutex(&mut g, 0);
    mutex(&mut g, 1);
    g.hold(1, 0, 1);
    g.hold(2, 1, 1);
    assert_eq!(g.request(1, 1), Decision::WouldBlock); // T1 -> T2
    // T2 exits: its edges vanish, so T1's pending request no longer forms a cycle.
    g.remove_thread(2);
    assert!(g.edge_waiters(1).is_empty());
}

/// A three-thread cycle: T1 -> T2 -> T3 -> T1.
#[test]
fn three_thread_cycle() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    g.add_thread(3);
    mutex(&mut g, 0);
    mutex(&mut g, 1);
    mutex(&mut g, 2);
    g.hold(1, 0, 1);
    g.hold(2, 1, 1);
    g.hold(3, 2, 1);
    assert_eq!(g.request(1, 1), Decision::WouldBlock); // 1 -> 2
    assert_eq!(g.request(2, 2), Decision::WouldBlock); // 2 -> 3
    assert_eq!(g.request(3, 0), Decision::Deadlock); // 3 -> 1 closes cycle
}

/// A condvar cycle: the signaler holds the condvar resource, the waiter wants it
/// while holding a mutex the signaler needs. Cycle: T1 -> C0 -> T2 -> M1 -> T1.
#[test]
fn condvar_cycle_deadlock() {
    let mut g = WaitForGraph::new();
    g.add_thread(1);
    g.add_thread(2);
    mutex(&mut g, 0); // M0
    mutex(&mut g, 1); // M1
    mutex(&mut g, 2); // C0 (condvar modeled as an exclusive rendezvous resource)
    g.hold(1, 1, 1); // T1 holds M1
    g.hold(2, 0, 1); // T2 holds M0
    g.hold(2, 2, 1); // T2 is the signaler of C0 (holds it)
    assert_eq!(g.request(2, 1), Decision::WouldBlock); // T2 -> M1 -> T1
    assert_eq!(g.request(1, 2), Decision::Deadlock); // T1 -> C0 -> T2 -> M1 -> T1
}
