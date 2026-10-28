# 选作题目 3：更通用的内核+应用死锁检测（参考 Linux 动态死锁检测方法）

## 1. 目标与范围

题目要求“更通用的内核+应用的死锁检查（参考 Linux 的动态内核死锁检测方法）”，并鼓励“形成可脱离 OS 独立存在的库，可以裸机测试或在用户态测试（比如 easyfs 那样）”。

本选作交付两样东西：

1. **独立死锁检测库 `deadlock-rs/`**：一个 `#![no_std]`、无全局状态、可脱离 OS 独立运行的通用死锁检测库。它以经典的**线程级等待图（wait-for graph）**为基础，通过**有向环检测**判定死锁，能统一处理 `mutex`、`semaphore`、`condvar`、线程 `join`/`waitpid` 以及**任意混合**的这些同步原语——这正是题目中明确不要求 Banker 算法覆盖的场景（“无需考虑二者混合使用导致的死锁”）。
2. **本设计与功能/性能分析文档**。

关键点：Banker 算法（`os/src/sync/deadlock.rs`）是**资源分配模型**，只能对“单类资源、资源数已知、线程一次一个资源”做安全判定，无法表达“线程等待线程”（join）与跨类资源的等待环。`deadlock-rs` 改用**等待图**，把“线程 A 等待线程 B”作为一条有向边，死锁 ⇔ 图中存在有向环。这是 Linux `lockdep` / `mutex_lock` 内建检测的同类思路（lockdep 也是给锁的持有链建等待关系并检测环）。

## 2. 设计

### 2.1 数据模型

```
节点：线程节点（ThreadId = usize）
边（等待关系）：
  请求边   T -> R    线程 T 阻塞等待资源 R（R 被他人持有）
  指派边   R -> T    资源 R 当前被线程 T 持有
  线程边   T1 -> T2  线程 T1 等待线程 T2 结束（join / waitpid）
```

`WaitForGraph` 内部用三个映射维护：

- `req: BTreeMap<ThreadId, BTreeSet<ResourceId>>` —— 线程正在等待哪些资源；
- `res: BTreeMap<ResourceId, ResState>` —— 每个资源的类型（`ResKind::Mutex` 独占 1 单位 / `ResKind::Semaphore{units}` 计数）与当前持有者集合 `holders`；
- `tjoin: BTreeMap<ThreadId, BTreeSet<ThreadId>>` —— 线程正在 join 哪些线程。

**环检测**把请求边与指派边折叠成线程级的“等待谁”：

```
thread_out(T) = { h | R ∈ req[T], h ∈ holders(R), h ≠ T } ∪ tjoin[T]
```

即：线程 T 等待的资源 R，其等待对象是 R 的**除 T 以外**的所有持有者。`h ≠ T` 的排除是关键的——等待计数资源（信号量）的线程是在等**别人**手里的单位，不是在等自己手里那部分，否则 `T→R→T` 会形成“自环”而误报（已用 `semaphore_partial_hold_not_self_deadlock` 测例验证）。随后对每个线程做显式栈 DFS 判环，`O(V+E)`，无递归（内核栈友好）。

### 2.2 对外 API

| 方法 | 语义 | 返回 |
| --- | --- | --- |
| `add_thread(tid)` | 注册线程 | — |
| `remove_thread(tid)` | 线程退出，清除所有关联边 | — |
| `add_resource(id, ResKind)` | 注册 mutex / semaphore | — |
| `hold(tid, res, units)` | 授予 units 单位 | — |
| `release(tid, res, units)` | 释放 units 单位 | — |
| `request(tid, res)` | 线程申请资源 | `Decision` |
| `join(waiter, target)` | 线程等待另一线程 | `Decision` |
| `join_finished(target)` | 被等待线程结束 | — |
| `would_deadlock()` | 全图判环 | `bool` |
| `edge_waiters(tid)` | 诊断：线程正等待谁 | `BTreeSet` |

`Decision` 三态：`Ok`（立即授予/不阻塞）、`WouldBlock`（可安全阻塞）、`Deadlock`（会成环，必须拒绝——对应内核的 `-0xDEAD`）。每次 `request`/`join` 只在该次新增边时判环，增量开销可控。

### 2.3 与 Banker 算法（内核 `deadlock.rs`）的对比

| 维度 | Banker（内核现实现） | 等待图（`deadlock-rs`） |
| --- | --- | --- |
| 模型 | 资源分配（Available/Allocation/Need） | 等待图（线程间有向边） |
| 适用原语 | 仅互斥锁 / 仅信号量，**分开**检测 | mutex+semaphore+condvar+join **混合**统一检测 |
| 可表达“线程等线程” | 否 | 是（join/waitpid） |
| 判据 | 安全序列存在性 | 有向环存在性 |
| 计算 | 每请求一次全图迭代 | 每请求一次 DFS，`O(V+E)` |
| 语义 | **预防**：不安全即拒绝（保守） | **检测**：成环即拒绝 |
| 依赖 | 需线程预先声明 `need`（近似） | 只需真实等待边（精确） |
| 独立库 | 否（内嵌 PCB） | 是（`no_std` 独立 crate） |

两者的共同点：都在“申请可能造成死锁”时**拒绝**（返回 `-0xDEAD`），属于死锁**预防/避免**而非事后检测；不同点在于 Banker 需要预知每线程的资源需求（保守、对信号量“每线程每类最多一个单位”的近似），等待图只依据**当前真实等待关系**（精确、可覆盖跨类与 join）。

## 3. 功能测试用例

测试位于 `deadlock-rs/tests/graph.rs`（`cargo test`），13 个用例：

| 用例 | 覆盖 |
| --- | --- |
| `mutex_abba_deadlock` | 经典 ABBA 互斥死锁 → `Deadlock` |
| `mutex_recursive_is_deadlock` | 重入互斥锁（自环）→ `Deadlock` |
| `mutex_abba_safe_when_one_releases` | 一方先释放则不误报 → `WouldBlock` |
| `semaphore_partial_hold_not_self_deadlock` | 持有一半单位再申请 → 不误报自环 |
| `semaphore_deadlock_with_second_resource` | 信号量+互斥锁组合成环 |
| `join_mutual_deadlock` / `join_self_deadlock` | 互 join / 自 join |
| `join_chain_safe` | join 链不闭环，结束解除 |
| `mixed_mutex_and_semaphore_deadlock` | **跨类混合成环**（Banker 不覆盖）|
| `safe_interleaving_no_false_positive` | 安全交错全程不误报 |
| `remove_thread_breaks_cycle` | 线程退出清边破环 |
| `three_thread_cycle` | 三线程成环 |
| `condvar_cycle_deadlock` | 条件变量成环 |

全部通过。其中 `mixed_mutex_and_semaphore_deadlock` 与 `join_mutual_deadlock` 正是题目允许 Banker 忽略、而本库必须覆盖的“混合使用”场景。

## 4. 性能分析

复杂度：新增一条等待边触发一次 `would_deadlock()`，最坏 `O(V+E)`，V=线程数、E=等待边数；单进程内线程数与同步对象数都很小（rCore 单核协作式、每进程几十线程以内），判环开销可忽略。相比 Banker 的“每请求全图迭代”，等待图 DFS 只在**新增边**时执行，且用 `visited` 剪枝，实际常数更小。

内存：每线程、每资源各 O(1) 条目，总 O(V+E)。无全局静态，可放入 PCB（每进程一个 `WaitForGraph`，与 `DeadlockState` 平级），也可嵌入用户态。

## 5. 可脱离 OS 独立存在

`deadlock-rs` 为 `#![no_std]` + `alloc`，无任何内核依赖（不引用 task/timer/页表），可：
- 编译进内核（`default-features = false`）；
- 在**用户态**直接 `cargo test`（本文档即如此运行）；
- 在**裸机**（无 std）作为纯逻辑库使用。

这就是题目“鼓励形成可脱离 OS 独立存在的库”的落地。后续可把它接入 `PCB.deadlock`，取代/补充 Banker 检测，实现对 mutex+semaphore+join 的统一死锁避免。

## 6. 结论

用**等待图 + 有向环检测**实现了比 Banker 更通用的死锁检测，独立成 `no_std` 库并有 13 个功能测试。它覆盖 Banker 无法表达的跨类混合与线程 join，且语义更精确（只依据真实等待关系）。代价是它属于“检测并拒绝”而非“预防”，且对计数资源的持有者建模需要“排除自身”以避免自环误报——这一点已用测例固定下来。
