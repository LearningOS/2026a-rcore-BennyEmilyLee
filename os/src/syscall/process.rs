//! Process management syscalls

use crate::config::PAGE_SIZE;
use crate::mm::{
    translated_byte_buffer, MapPermission, PageTable, PageTableEntry, PTEFlags, PhysAddr,
    VirtAddr,
};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next,
    get_current_syscall_count, mmap_current, munmap_current, suspend_current_and_run_next,
};
use crate::timer::get_time;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// Write the current wall-clock time through the user pointer.
///
/// The `TimeVal` may straddle two virtual pages, so we serialize it on the
/// kernel side and copy the bytes back through [`translated_byte_buffer`],
/// which already walks page by page.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    if ts.is_null() {
        return -1;
    }
    let now = get_time();
    let tv = TimeVal {
        sec: now / 1_000_000,
        usec: now % 1_000_000,
    };
    let len = core::mem::size_of::<TimeVal>();
    let src: &[u8] =
        unsafe { core::slice::from_raw_parts(&tv as *const TimeVal as *const u8, len) };
    let user_bufs = translated_byte_buffer(current_user_token(), ts as *const u8, len);
    let mut off = 0usize;
    for buf in user_bufs {
        let n = buf.len();
        buf.copy_from_slice(&src[off..off + n]);
        off += n;
    }
    0
}

fn is_user_readable(pte: &PageTableEntry) -> bool {
    pte.is_valid()
        && pte.readable()
        && (pte.flags() & PTEFlags::U) != PTEFlags::empty()
}

fn is_user_writable(pte: &PageTableEntry) -> bool {
    pte.is_valid()
        && pte.writable()
        && (pte.flags() & PTEFlags::U) != PTEFlags::empty()
}

/// Three modes:
/// - `trace_request == 0`: read one byte from the user address `id`, return the value.
/// - `trace_request == 1`: write `data & 0xFF` to the user address `id`, return 0.
/// - `trace_request == 2`: return how many times the current task called syscall `id`.
///
/// For modes 0/1 we refuse addresses whose PTE is either missing, not accessible
/// from U-mode, or lacking the required R/W permission — returning -1 in each case.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        0 => {
            let va = VirtAddr::from(id);
            let pte = match PageTable::from_token(current_user_token()).translate(va.floor()) {
                Some(p) => p,
                None => return -1,
            };
            if !is_user_readable(&pte) {
                return -1;
            }
            let pa = PhysAddr::from(pte.ppn());
            unsafe { ((pa.0 + va.page_offset()) as *const u8).read_volatile() as isize }
        }
        1 => {
            let va = VirtAddr::from(id);
            let pte = match PageTable::from_token(current_user_token()).translate(va.floor()) {
                Some(p) => p,
                None => return -1,
            };
            if !is_user_writable(&pte) {
                return -1;
            }
            let pa = PhysAddr::from(pte.ppn());
            unsafe { ((pa.0 + va.page_offset()) as *mut u8).write_volatile(data as u8) };
            0
        }
        2 => get_current_syscall_count(id) as isize,
        _ => -1,
    }
}

fn prot_to_permission(prot: usize) -> Option<MapPermission> {
    // prot: bit0 = R, bit1 = W, bit2 = X; other bits must be zero.
    if prot & !0x7 != 0 || prot & 0x7 == 0 {
        return None;
    }
    let mut perm = MapPermission::U;
    if prot & 0x1 != 0 {
        perm |= MapPermission::R;
    }
    if prot & 0x2 != 0 {
        perm |= MapPermission::W;
    }
    if prot & 0x4 != 0 {
        perm |= MapPermission::X;
    }
    Some(perm)
}

/// Map `len` bytes starting at the page-aligned `start` to fresh physical frames.
///
/// Error cases all return -1:
/// - `start` not page aligned.
/// - `prot & !0x7 != 0` (unknown flags) or `prot == 0` (meaningless).
/// - Any page in `[start, start + len)` already mapped.
/// - Not enough physical frames.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!(
        "kernel: sys_mmap start={:#x} len={} prot={:#x}",
        start,
        len,
        prot
    );
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    let perm = match prot_to_permission(prot) {
        Some(p) => p,
        None => return -1,
    };
    if !mmap_current(start, len, perm) {
        return -1;
    }
    0
}

/// Unmap the pages covering `[start, start + len)`.
///
/// Refuses if either boundary is not page aligned, or if any page in the
/// range is currently unmapped.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap start={:#x} len={}", start, len);
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    let end = start.checked_add(len).filter(|_| len > 0).unwrap_or(start);
    if end % PAGE_SIZE != 0 {
        return -1;
    }
    if !munmap_current(start, len) {
        return -1;
    }
    0
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
