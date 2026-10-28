# Lab 5: 死锁检测实现报告

## 1. 方法功能总结

### 1.1 sys_enable_deadlock_detect() 功能总结

`sys_enable_deadlock_detect(is_enable)` 为当前进程按需开启/关闭死锁检测：参数非 0/1 时返回 -1；开启时先把已创建的互斥锁/信号量登记进检测器，置位 `PCB.deadlock.enabled` 后返回 0。开启后 `mutex_lock`/`semaphore_down` 若判定会死锁则拒绝该请求并返回 `-0xDEAD`，否则正常执行；`mutex_unlock`/`semaphore_up` 同步更新检测状态。

### 1.2 死锁检测机制（DeadlockState / ResourceSystem）功能总结

检测按互斥锁、信号量**分开**进行（题意允许）。每种资源维护银行家算法的 `Available`（空闲单位）、`Allocation`（线程已占）、`Need`（线程尚需）三结构：请求时若资源空闲则授予，否则用安全算法判断是否存在使所有线程完成的顺序；安全则允许阻塞（入等待队列），不安全则回滚 `Need` 并拒绝。释放时按内核真实唤醒顺序移交或归还空闲池；线程退出/新建同步清理其行列，自旋锁不参与等待图。

## 2. 问答题

### 问题 1：主线程退出时的资源回收

**问题**：当主线程（0 号线程）退出时，视为整个进程退出，需要结束该进程管理的所有线程并回收其资源。

**需要回收的资源有哪些？**

主线程退出路径（`os/src/task/mod.rs` 的 `exit_current_and_run_next`，`tid == 0` 分支）需要回收的资源包括：

1. **线程级资源**：每个线程的 `TaskControlBlock` 及其持有的
   - **内核栈 `kstack`**：主线程的内核栈因 `add_stopping_task` 先进入 stop-wait 状态保留，直到父进程 `sys_waittid` 回收时才真正释放；其他线程的内核栈随各自 TCB 释放。
   - **`TaskUserRes`（用户态资源）**：`tid` 号、`trap_cx` 用户页、`ustack` 用户栈页。退出时逐线程 `res.take()` 取出并 `drop`，归还 `tid` 到进程的回收分配器、释放这两组用户页。
2. **进程级资源**：
   - **地址空间**：`memory_set.recycle_data_pages()` 释放程序代码/数据/堆的用户页；页表随 `MemorySet` 释放。
   - **文件描述符表 `fd_table`**：`clear()` 释放所有打开文件/管道/设备对象。
   - **同步对象**：`mutex_list`/`semaphore_list`/`condvar_list` 随 `PCB` 释放。
   - **`pid2process` 全局映射条目**：`remove_from_pid2process(pid)` 删除。
   - **子进程列表**：退出前把 `children` 全部移交给 `INITPROC`（孤儿收养），随后 `children.clear()`。
   - **线程列表 `tasks`**：`tasks.clear()` 释放全部线程的 `Arc`。
3. **调度器状态**：每个线程在 `TaskManager` 就绪队列中的条目、以及 `TIMERS` 定时器堆中的睡眠定时器，都必须移除（`remove_inactive_task` 内部调用 `remove_timer`）。
4. **进程本身**：进程被标记为 `is_zombie = true` 并记录 `exit_code`；`PCB` 本体不会立即释放，而是等父进程 `waitpid` 回收（回收 `PCB`、主线程的内核栈与僵尸记录）。

**其他线程的 `TaskControlBlock` 可能在哪些位置被引用，分别是否需要回收，为什么？**

其他线程（`tid != 0`）的 `TaskControlBlock` 可能被以下位置以 `Arc` 强引用：

1. **`PCB.tasks`（`Vec<Option<Arc<TaskControlBlock>>>`）** —— **需要回收**。它是线程 TCB 的主要持有者，必须通过 `tasks.clear()` 释放这些 `Arc`，否则线程的 TCB、内核栈、用户资源无法释放。
2. **`TaskManager` 的就绪队列（`RUN_QUEUE`）** —— **需要回收（移除）**。若一个线程处于 Ready 状态，调度器持有它的 `Arc`；进程退出后若不移除，该线程仍会被 `fetch_task` 取到并调度，而此时它的进程已亡、地址空间已回收，将导致访问已释放内存或调度到“幽灵线程”。`remove_inactive_task` 正是为此而设计。
3. **`TIMERS` 定时器堆** —— **需要回收（移除）**。若线程在 `sys_sleep`/条件变量等待中挂起，`TIMERS` 持有其 `Arc` 和到期时间；进程退出后若不 `remove_timer`，定时器到期会 `wakeup_task` 一个已死进程的线程。`remove_inactive_task` 内调用 `remove_timer` 处理。
4. **`mutex`/`semaphore`/`condvar` 的等待队列（`VecDeque<Arc<TaskControlBlock>>`）** —— **需要回收，但采用间接方式**。等待队列中的线程因阻塞既不在就绪队列也不在定时器堆，不会被调度或唤醒；其 TCB 的 `Arc` 只存在于对应同步对象的等待队列中。由于这些同步对象是**进程私有**的（题目限定单进程内使用），当 `PCB` 释放、同步对象的 `Arc` 引用计数归零时，等待队列随之释放，从而间接释放这些 TCB。代码注释亦说明“blocked tasks are removed when the PCB is deallocated”，故退出路径不显式处理它们。若将来同步对象可跨进程共享，则需改为显式清理。
5. **`TaskControlBlock.res`（用户资源）** —— **需要回收**。退出时逐线程 `res.take()` 取出并释放。
6. **`ProcessControlBlock` 对线程的弱引用方向**（`TCB.process: Weak<PCB>`）与 `Processor.current`、`idle_task_cx` —— 不涉及其他线程 TCB，无需处理。

### 问题 2：两种 Mutex 实现的区别

**问题**：对比以下两种 `Mutex` 实现，二者有什么区别？这些区别可能会导致什么问题？

**区别**（对应 `os/src/sync/mutex.rs` 的 `MutexBlocking`，属于 `Mutex2` 形态）：

| 维度 | `Mutex1`（含 `loop` 版本） | `Mutex2`（当前 `MutexBlocking`，无 `loop` 版本） |
| --- | --- | --- |
| `lock()` 被唤醒后的行为 | 重新进入 `loop`，**重新检查** `locked`；若仍被占则再次入队、再次阻塞 | 唤醒后**不再检查**，直接认为已获得锁并进入临界区 |
| `unlock()` 对 `locked` 的处理 | 无条件置 `locked = false`，再唤醒一个等待者 | 有等待者时**只唤醒不移除 `locked`**（直接移交）；无等待者才置 `locked = false` |
| 锁的移交方式 | 释放后锁回到“空闲池”，由等待者**重新竞争**获得 | 释放时**直接移交给**队首等待者（handoff），`locked` 保持为真 |

**可能导致的区别与问题：**

1. **`Mutex1` 的问题——“车队/惊群”效应（性能问题，非正确性问题）**：`unlock` 置 `locked=false` 并唤醒一个等待者；该等待者被唤醒后重新检查，若此刻锁已被**另一个**等待者或新来的线程抢走，它会再次入队、再次阻塞。在高竞争场景下，一个锁的释放可能引发大量线程反复“唤醒→检查→再阻塞”，造成上下文切换开销陡增（convoy effect），但**不会破坏临界区互斥**，正确性有保证。

2. **`Mutex2` 的问题——移交不一致导致的正确性缺陷（更严重）**：
   - **锁状态与真实所有权脱钩**：当 `unlock` 发现有等待者时，只 `add_task` 唤醒它而不置 `locked=false`，即把锁“移交”给被唤醒线程。若该被唤醒线程在被调度前被抢占、或该线程随后异常/被终止而未真正运行，`locked` 将**永远保持为真**，此后所有 `lock()` 都会阻塞，系统进入**永久死锁**——尽管锁实际上空闲。
   - **唤醒线程未复核**：被唤醒线程直接进入临界区而不检查自己是否真的持锁。在单核协作式调度下移交通常成立，但一旦调度顺序或抢占改变（如多核、中断抢占），被唤醒线程可能与“新抢到锁”的线程同时进入临界区，造成**数据竞争**。
   - **丢失唤醒**：`unlock` 只在 `wait_queue` 非空时唤醒；若某个线程在 `add_task` 之后、真正运行之前被移除/终止，该唤醒“丢失”，其后等待者可能永久阻塞。

3. **共同点**：两者都是**非重入**锁；同一线程对已持有的锁再次 `lock()` 会把自己加入等待队列并阻塞，形成**自我死锁**。

**结论**：`Mutex1` 以“唤醒后复核 + 无条件释放 `locked`”换得正确性，代价是可能的车队效应；`Mutex2`（当前实现）以“移交 + 不置 `locked=false`”省去复核，换得更少上下文切换，但存在锁状态与所有权不一致、丢失唤醒、永久死锁的风险。对 `Mutex2` 的改进方向是：唤醒后仍复核 `locked`（即回到 `Mutex1` 形式），或保证移交的原子性与“被唤醒者必然运行”的调度保证。

## 3. 选作题目

选作题目（更通用的死锁检测、多核内核同步/性能）的实现代码、设计与功能/性能测试分析文档，见：

- 独立死锁检测库 `deadlock-rs/`（`no_std`、可脱离 OS 独立存在的通用等待图死锁检测，覆盖 mutex/semaphore/condvar/线程等待混合场景）及测试。
- 多核扩展设计与分析文档：`reports/lab5-选作-多核设计分析.md`。
- 通用死锁检测设计与分析文档：`reports/lab5-选作-通用死锁检测.md`。

详见上述文件。
