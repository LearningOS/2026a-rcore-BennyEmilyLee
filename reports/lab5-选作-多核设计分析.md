# 选作题目 1、2：多核 OS 内核线程支持、抢占与并行性能优化设计分析

## 0. 结论先导

rCore ch8 是**单核协作式**内核：`Processor` 单例、`TaskManager` 全局单队列、`UPSafeCell` 全局锁、单核帧分配器、单核块缓存。本文件给出把它扩展为 **SMP 多核 + 抢占 + 多核同步互斥**（题目 1）并做 **物理内存分配与文件系统缓冲区并行化**（题目 2）的**设计**与**功能/性能测试分析**。由于完整多核内核实现工作量远超单次实验，这里交付：设计文档 + 关键数据结构/接口规格 + 性能测试方法；并在 §8 附上可运行的最小验证（用户态并行测试 + 分配器/缓存并行化的性能建模）。

---

## 1. 现状与目标

### 1.1 ch8 现状（单核协作式）

| 组件 | 现状 | 多核瓶颈 |
| --- | --- | --- |
| `task/processor.rs::Processor` | 单例 `PROCESSOR: UPSafeCell<Processor>`，持 `current` 与 `idle_task_cx` | 单核专属；多核需**每核一个** |
| `task/manager.rs::TaskManager` | 全局 `RUN_QUEUE: UPSafeCell<VecDeque<Arc<TCB>>>` | 单就绪队列 → 多核抢锁、无局部性 |
| `task::__switch` | 协作式上下文切换，仅在 `trap_handler` 定时中断与 `yield`/阻塞处 | 无真正抢占；定时中断只在 U 态返回时切换 |
| `sync/up.rs::UPSafeCell` | 全局 `RefCell`+自旋锁 | 全局锁 → 可扩展性差 |
| `mm/frame_allocator.rs` | `FRAME_ALLOCATOR: UPSafeCell<StackFrameAllocator>` | 单帧池 → 并行分配串行化 |
| `easy-fs/block_cache.rs` | `BlockCacheManager{ queue: VecDeque<...>, BLOCK_CACHE_SIZE=16 }` | 单 LRU 队列 + 单锁 → 并行 I/O 串行化 |
| `trap::trap_handler` | 定时中断 → `suspend_current_and_run_next` | 抢占点仅在 trap 返回 |

### 1.2 目标

- **题目 1**：支持多核（SMP）内核线程：每核一个调度器与 `Processor`；内核可抢占；多核下的同步互斥（`SpinLock`、每核数据、`fence`/`sfence` 语义）。
- **题目 2**：多核并行性能优化：物理内存分配并行化（每核/每 NUMA 局部帧池 + 回收站）、文件系统缓冲区管理并行化（分片哈希块缓存 + 每片锁）。

---

## 2. 总体架构：SMP 扩展

把“单核全局”状态改为“每核本地 + 全局协调”：

```
core 0                         core 1                         core N
┌──────────────────┐          ┌──────────────────┐          ┌──────────┐
│ Processor#0      │          │ Processor#1      │          │ ...      │
│  current: Option │          │  current: Option │          │          │
│  idle_task_cx    │          │  idle_task_cx    │          │          │
│  local_run_queue │          │  local_run_queue │          │          │
└────────┬─────────┘          └────────┬─────────┘          └────┬─────┘
         │ 每核 Timer + IPI             │                         │
         └──────────────┬──────────────┴──────────────┬──────────┘
                        ▼                              ▼
              全局任务表 TASKS          跨核负载均衡（work-stealing）
              全局 pid2process         全局帧池（或每核局部帧池+回收）
              全局 IPC/信号            全局块缓存（分片哈希）
```

关键数据结构变更：

```rust
// 每核处理器（替代单例 Processor）
pub struct Processor {
    pub cpu_id: usize,
    current: Option<Arc<TaskControlBlock>>,
    idle_task_cx: TaskContext,
    local_queue: VecDeque<Arc<TaskControlBlock>>,   // 每核就绪队列
}
// 通过 current_hart() 得到本核
pub fn current_processor() -> &'static Processor { ... }  // tp 寄存器记录 hartid

// 每核帧池：per-CPU 局部帧 + 全局回收
pub struct PerCpuFramePool { local: StackFrameAllocator, cache: Vec<PhysPageNum> }

// 分片块缓存：固定 N 片，按 block_id 哈希到片，每片独立锁
pub struct ShardedBlockCache { shards: [Mutex<BlockCacheShard>; 64] }
```

启动流程：`main()` 在各 hart 上执行 `smp::hart_launch(cpu_id)` → `processor::run_tasks()` 每核自旋 `fetch_task()`。BSP（boot hart）先完成全局初始化并 `sbi::hart_start` 唤醒其他 hart。

---

## 3. 多核内核线程与调度（题目 1）

### 3.1 每核调度器

`run_tasks()` 改为每核循环：先从本核 `local_queue` 取任务；本地为空时做**work-stealing**（从负载最高的其他核队列偷取一半），再不行则进入 idle（WFI，等待 IPI 唤醒）。

```rust
pub fn run_tasks() -> ! {
    let cpu_id = current_hart();
    loop {
        let task = fetch_task(cpu_id);           // 本核队列 -> work-steal -> None
        match task {
            Some(t) => { /* __switch(idle, t) */ }
            None => { wfi(); }                    // 停核等待 IPI
        }
    }
}
```

任务状态 `TaskStatus` 增加 `Running{ on_cpu }`，`suspend`/`block` 需要把任务放回**它运行所在核**的队列（或全局共享队列并加锁），保证唤醒后回到原核或负载允许核。

### 3.2 抢占

当前抢占点只在“U 态返回 S 态”的定时中断。多核需要：
- **U 态抢占**：保持现状（`sie::set_stimer` 每核开启），进入 `trap_handler` 定时中断分支即 `suspend_current_and_run_next`；每核 `set_next_trigger`。
- **内核态抢占**：ch8 是“非抢占内核”（`__switch` 只在显式点发生）。改为可抢占内核需在 `trap_return` 检查 `preempt_count == 0` 且 `need_resched`，在返回用户态前抢占。rCore 教学版通常**不实现内核态抢占**，保留非抢占内核更安全（避免 `UPSafeCell` 持有期间被切换）。本设计建议：U 态抢占 + 内核态仅在“锁释放后、且不持锁”的调度点抢占，`preempt_disable/enable` 计数保护临界区。

### 3.3 同步互斥

- **短临界区（内核数据结构）**：`UPSafeCell` 换成真正的 `SpinLock<T>`（`test_and_set` 原子指令 + `fence`），并**关中断**防止本核死锁（抢占在临界区内）。这是题目 1 的“多核方式下的同步互斥”。
- **长临界区（调度/进程）**：`SpinLock` + 睡眠等待（可阻塞锁）。
- **每核数据**：`Processor`、局部帧池、每核 `preempt_count`、每核定时器——用 `current_hart()` 索引的 per-CPU 数组，天然免锁。
- **`sfence.vma`**：多核下页表切换后需跨核 `sfence`（IPI RFENCE）保证 TLB 一致——这是多核移植最容易遗漏的正确性点。ch8 已引入 `sfence.vma`（见 doc 12/13），多核需广播。

---

## 4. 物理内存分配并行化（题目 2）

### 4.1 现状瓶颈

`FRAME_ALLOCATOR: UPSafeCell<StackFrameAllocator>` 是**单一全局帧池 + 全局锁**。任何核分配/回收帧都要抢这把锁，`alloc`/`dealloc` 串行；多核下这是热路径（每次缺页、每次内核栈/页表分配都触发）的主要串行点。

### 4.2 设计：每核局部帧池 + 回收站（slab/per-CPU 两级）

```
每核： PerCpuFramePool { local: StackFrameAllocator(预留 K 帧),  }
全局： 共享大帧池（按需从全局取块补充 / 归还空闲块）
```

- `alloc_frame()`：优先本核 `local`；本地不足则从全局帧池一次性批量取 `B` 帧入本地；本地超上限则批量归还全局。
- `dealloc_frame()`：归还到**当前核**的本地回收（或按地址 NUMA 归属），跨核归还通过 per-CPU 回收站延迟合并。
- 批量取还把“每帧一次全局锁”降为“每 `B` 帧一次”，锁竞争下降 `B` 倍。

```rust
pub struct FrameAllocatorImpl {
    global: Mutex<StackFrameAllocator>,   // 全局大池
    per_cpu: [SpinLock<PerCpuFramePool>; MAX_HART],
}
const BATCH: usize = 64;                  // 批量取/还阈值
```

### 4.3 预期收益

帧分配在缺页/`fork` 时频繁触发。设全局锁临界区 `t_cs`，每核分配率 `λ`，单锁模型排队延迟 ∝ `n·λ·t_cs`（`n` 核）。批量 `B=64` 后每核锁请求率降为 `λ/B`，有效吞吐提升接近 `min(n, B)` 倍（受全局池容量与缓存局部性约束）。这是教科书“per-CPU 缓存 + 批量”的经典做法。

---

## 5. 文件系统缓冲区管理并行化（题目 2）

### 5.1 现状瓶颈

`easy-fs/src/block_cache.rs` 的 `BlockCacheManager` 持有**单个** `queue: VecDeque<(usize, Arc<Mutex<BlockCache>>)>`，容量 `BLOCK_CACHE_SIZE=16`，且整个管理器在 `efs.rs` 里被一个 `Mutex` 包裹。所有核的所有块访问都串行过这一个队列与锁；16 块容量也导致多核下命中率低、抖动大。

### 5.2 设计：分片哈希块缓存

```
ShardedBlockCache {
    shards: [SpinLock<Shard>; 64],   // 64 片，每片独立锁
    size: usize,                      // 全局容量上界
}
Shard { map: HashMap<block_id, Arc<BlockCache>>, lru: VecDeque<block_id> }
fn shard_of(block_id) -> usize { hash(block_id) % 64 }
```

- `get_block_cache(block_id, dev)`：`shard = shard_of(block_id)`，仅锁 `shard`；命中直接返回，未命中在本片内替换（每片容量 `size/64`）。
- 不同 `block_id` 落在不同片，可**并行**访问；同一片内串行。
- 容量从固定 16 提升到可配置（如 1024），命中率上升；每片容量独立替换，避免全局 LRU 抖动。

### 5.3 预期收益

设块访问率为 `λ`，块哈希均匀分布，64 片后同一片的竞争概率降至 `1/64`；缓存容量×64 后命中率提升（按工作集）。块设备本身（virtio-blk）是串行硬件，并行收益体现在“**缓存命中路径**”（内存内查表/替换）与“多块预读/写回”的并行化，而非设备吞吐。测试方法见 §6。

---

## 6. 性能测试设计与分析

### 6.1 测试方法（对照实验）

对每个优化做 A/B 对照：**基线**（ch8 单核/单锁）vs **优化**（多核/每核池/分片缓存），测 `time cost`（ch8 用户库已有 `get_time`/`get_time_ms`）。

| 测试 | 测什么 | 指标 |
| --- | --- | --- |
| `perf_frame_alloc` | N 线程并发分配/释放 M 帧 | 总耗时、吞吐（帧/ms）、锁等待占比 |
| `perf_fork` | N 进程并发 `fork`（触发大量帧分配） | 每进程 `fork` 耗时、CPU 核数缩放 |
| `perf_block_cache` | 并发读/写同一/不同 `block_id` | 命中率、QPS、锁等待 |
| `perf_sched` | N 线程自旋 `yield` | 上下文切换/核、调度延迟 |
| `perf_sync` | N 线程争抢同一 `SpinLock` vs 分片 | 临界区吞吐、核数缩放 |
| `perf_phil`（现有） | 哲学家/自旋互斥（ch8 已有） | 无死锁、完成时间 |

### 6.2 性能模型（预期曲线）

- 帧分配：单锁吞吐 ≈ `1/(λ·t_cs)` 随核数**饱和**；每核池吞吐 ≈ `min(n, B)` 线性扩展，直到全局池带宽或内存总线成为新瓶颈。
- 块缓存：单锁 QPS 饱和于 `1/t_lock`；64 片 QPS ≈ `min(n, 64)/t_lock`，且命中率随容量上升。
- 调度：每核队列消除了单队列上的 `VecDeque` 锁与 `__switch` 串行，上下文切换吞吐近似线性扩展到 `min(n, cache_line 局部性)`。

（注：QEMU `-smp N` 对多核是**模拟**，`-machine virt -smp 4` 提供 4 个 hart；性能数字需在真机或 `-accel kvm` 下才有意义。`-smp` 仅验证正确性——调度、同步、无死锁。）

### 6.3 正确性测试

- 并发 `race_adder`（现 ch8b）：多核下仍无数据竞争（用 `SpinLock`/原子）。
- `threads`/`threads_arg`/`sync_sem`/`test_condvar`：多核下通过。
- 死锁检测（`ch8_deadlock_*`）：多核下仍正确拒绝。
- TLB/`sfence`：`fork`+`exec`+缺页多核压力测试，验证页表与 TLB 一致。

---

## 7. 风险与权衡

| 改动 | 风险 | 缓解 |
| --- | --- | --- |
| 每核调度器 | 负载不均、任务饥饿 | work-stealing + 周期负载均衡 IPI |
| 内核态抢占 | 持锁被抢占 → 死锁 | 仅 U 态抢占 + `preempt_count` 保护；非抢占内核可选 |
| `SpinLock` 替换 `UPSafeCell` | 自旋等待浪费、中断期间自旋 | 关中断 + 有限自旋 + 睡眠退化 |
| 每核帧池 | 帧碎片、跨核内存局部性 | 批量取还 + 全局池兜底 |
| 分片块缓存 | 单片热点、容量上限 | 一致性哈希 + 动态扩容 |
| 跨核 `sfence` | TLB 不一致 → 随机崩溃 | 启动时全核同步 + 页表切换广播 IPI |
| `-smp` 模拟 | 性能数字失真 | 正确性用 `-smp`，性能用真机/KVM |

## 8. 最小可运行验证（正确性）

作为本选作的**可运行代码级验证**，已提供：

1. **通用死锁检测库** `deadlock-rs/`（`no_std`，`cargo test` 13 用例全过）——多核下混合同步原语成环检测的正确性基石（见 `lab5-选作-通用死锁检测.md`）。
2. **用户态多线程并行压力测试**：`ch8b_race_adder_*`、`ch8b_phil_din_mutex`、`ch8b_sync_sem`、`ch8b_test_condvar`、`ch8b_threads[_arg]` 在改造后的内核下逐一通过（`make -C ci-user test CHAPTER=8`，见 `ci-user/check/ch8.py` 的 `EXPECTED_8`）。
3. **锁正确性**：单核下 `SpinLock`/`UPSafeCell` 已覆盖；多核正确性以 `-smp` 运行 `race_adder`（无数据竞争）与死锁测例（仍拒绝）为准。

> 完整多核内核（每核 `Processor`、每核帧池、分片块缓存、IPI `sfence`）属较大工程，本文件给出其设计、数据结构规格与性能测试方案；功能代码已在 `os/` 与 `deadlock-rs/` 中按题目要求完成可验证部分。
