//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: Vec<Arc<TaskControlBlock>>,
}

/// A stride scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: Vec::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        if self.ready_queue.is_empty() {
            return None;
        }
        // Find the task with minimum stride
        let big_stride: u32 = 2048;
        let mut min_idx = 0;
        let mut min_stride = self.ready_queue[0].inner_exclusive_access().stride;
        for i in 1..self.ready_queue.len() {
            let stride = self.ready_queue[i].inner_exclusive_access().stride;
            if stride < min_stride {
                min_stride = stride;
                min_idx = i;
            }
        }
        // Add pass to the selected task's stride
        let selected = self.ready_queue[min_idx].clone();
        let mut inner = selected.inner_exclusive_access();
        let pass = big_stride / inner.prio;
        inner.stride += pass;
        drop(inner);
        // Remove from queue
        self.ready_queue.swap_remove(min_idx);
        Some(selected)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
