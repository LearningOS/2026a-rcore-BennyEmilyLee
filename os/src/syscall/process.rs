//! Process management syscalls
use crate::{
    loader::{app_range, user_stack_range},
    task::{
        current_task_id, exit_current_and_run_next, get_current_syscall_count,
        suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

// TODO: implement the syscall
///调用 ``sys_trace``（ID 为 410）用来追踪当前任务系统调用的历史信息，并做对应的修改。
/// 调用规范：
/// 这个系统调用有三种功能，根据 trace_request 的值不同，执行不同的操作：
///  - 如果 trace_request 为 0，则 id 应被视作 *const u8 ，表示读取当前任务 id 地址处一个字节的无符号整数值。此时应忽略 data 参数。返回值为 id 地址处的值。
///  - 如果 trace_request 为 1，则 id 应被视作 *mut u8 ，表示写入 data （作为 u8，即只考虑最低位的一个字节）到该用户程序 id 地址处。返回值应为0。
///  - 如果 trace_request 为 2，表示查询当前任务调用编号为 id 的系统调用的次数，返回值为这个调用次数。本次调用也计入统计 。
///  - 否则，忽略其他参数，返回值为 -1。
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        // read one byte from the user address `id`
        0 => {
            if is_current_task_addr(id) {
                unsafe { (id as *const u8).read_volatile() as isize }
            } else {
                -1
            }
        }
        // write the lowest byte of `data` to the user address `id`
        1 => {
            if is_current_task_addr(id) {
                unsafe { (id as *mut u8).write_volatile(data as u8) };
                0
            } else {
                -1
            }
        }
        // query how many times this task has invoked syscall `id`
        2 => get_current_syscall_count(id) as isize,
        _ => -1,
    }
}

/// Check whether `addr` lies inside the memory the current task owns.
///
/// Chapter 3 has no address-space isolation: the kernel runs with the same
/// page table (actually, with paging disabled) as the user task, so the only
/// way to keep a malicious or buggy pointer from making the kernel fault is to
/// whitelist the task's own app image and user stack.
fn is_current_task_addr(addr: usize) -> bool {
    let task_id = current_task_id();
    let (app_lo, app_hi) = app_range(task_id);
    let (stack_lo, stack_hi) = user_stack_range(task_id);
    (app_lo..app_hi).contains(&addr) || (stack_lo..stack_hi).contains(&addr)
}
