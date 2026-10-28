# Lab 3 · 编程作业报告

## 一、方法功能总结

### 1.1 sys_spawn() 功能总结

`sys_spawn` 系统调用用于创建新进程并执行目标程序。该函数接收一个用户态路径指针，通过 `translated_str` 翻译为内核态字符串，然后调用 `get_app_data_by_name` 获取程序数据。如果程序存在，则调用当前进程的 `fork()` 方法创建子进程，再通过 `exec()` 方法在子进程中加载新程序，最后将子进程加入调度器并返回子进程 PID。与 `fork()` + `exec()` 组合不同，`spawn` 是一个原子操作，避免了中间状态。如果程序不存在，返回 -1。该实现确保了进程创建的原子性和安全性。

### 1.2 sys_set_priority() 功能总结

`sys_set_priority` 系统调用用于设置当前进程的优先级。该函数接收一个优先级参数，首先检查优先级是否合法（必须 >= 2），如果非法则返回 -1。如果合法，则获取当前进程的独占访问权限，将优先级值存储到 TCB 的 `prio` 字段中，并返回该优先级值。优先级值用于 stride 调度算法计算进程的步长（pass），优先级越高（数值越小），进程的 pass 越大，调度频率越高。该函数确保进程优先级在合理范围内，防止优先级反转问题。

## 二、问答作业：stride 算法深入

### 2.1 问题 1：8bit 无符号整数溢出问题

**问题**：两个 pass = 10 的进程，使用 8bit 无符号整数存储 stride，p1.stride = 255, p2.stride = 250，在 p2 执行一个时间片后，理论上下一次应该 p1 执行。实际情况是轮到 p1 执行吗？为什么？

**回答**：

**不会轮到 p1 执行**。原因如下：

1. p2 执行一个时间片后，p2.stride 变为 250 + 10 = 260。
2. 260 超过了 8bit 无符号整数的最大值（255），发生溢出，实际存储值为 260 % 256 = 4。
3. 此时比较 p1.stride = 255 和 p2.stride = 4，显然 p2.stride 更小。
4. 因此，下一次调度会选择 p2 而不是 p1。

**这就是溢出反转问题**：理论上 stride 较小的进程应该优先调度，但溢出后比较结果完全反转。

### 2.2 问题 2：为什么要求进程优先级 >= 2

**问题**：我们之前要求进程优先级 >= 2 其实就是为了解决这个问题。可以证明，在不考虑溢出的情况下，在进程优先级全部 >= 2 的情况下，如果严格按照算法执行，那么 STRIDE_MAX - STRIDE_MIN <= BigStride / 2。为什么？

**回答**：

当所有进程优先级 >= 2 时，每个进程的 pass = BigStride / priority <= BigStride / 2。

假设当前时刻所有进程的 stride 值分别为 s1, s2, ..., sn。设 STRIDE_MIN = min(si)，STRIDE_MAX = max(si)。

1. 调度器选择 stride 最小的进程 P_min 执行。
2. P_min 执行后，P_min.stride += pass_min，其中 pass_min <= BigStride / 2。
3. 因此，新的 STRIDE_MAX <= 原来的 STRIDE_MAX + pass_min <= 原来的 STRIDE_MAX + BigStride / 2。
4. 新的 STRIDE_MIN >= 原来的 STRIDE_MIN（因为 P_min 被移除，其他进程的 stride 不变）。

**关键性质**：每次调度后，stride 的增量 <= BigStride / 2。这意味着所有进程的 stride 值不会相差太远，最大差距不会超过 BigStride / 2。

**直觉解释**：如果所有进程的 pass 都 <= BigStride / 2，那么每次调度后，进程的 stride 最多增加 BigStride / 2。因此，所有进程的 stride 值会保持在一个相对紧凑的范围内，不会发散到很大的差距。

### 2.3 问题 3：补全 Stride 的 partial_cmp 函数

**问题**：已知以上结论，考虑溢出的情况下，可以为 Stride 设计特别的比较器，让 BinaryHeap<Stride> 的 pop 方法能返回真正最小的 Stride。补全下列代码中的 partial_cmp 函数，假设两个 Stride 永远不会相等。

```rust
use core::cmp::Ordering;

struct Stride(u64);

impl PartialOrd for Stride {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // ...
    }
}

impl PartialEq for Stride {
    fn eq(&self, other: &Self) -> bool {
        false
    }
}
```

**回答**：

```rust
impl PartialOrd for Stride {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let diff = self.0.wrapping_sub(other.0);
        // 如果差值在 [0, STRIDE_MAX/2] 范围内，self 较小
        // 如果差值在 (STRIDE_MAX/2, STRIDE_MAX] 范围内，other 较小
        // STRIDE_MAX 是 u64::MAX
        if diff < (u64::MAX / 2 + 1) {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        }
    }
}
```

**解释**：

1. 使用 `wrapping_sub` 计算差值，避免溢出 panic。
2. 根据差值判断大小关系：
   - 如果差值在 [0, STRIDE_MAX/2] 范围内，说明 self 在 other 之后（环形距离较近），self 较小。
   - 如果差值在 (STRIDE_MAX/2, STRIDE_MAX] 范围内，说明 other 在 self 之后（环形距离较近），other 较小。
3. 由于假设两个 Stride 永远不会相等，所以不需要处理 `Ordering::Equal`。

**原理**：

将 stride 值看作环形缓冲区上的位置。两个位置的"环形距离"可以通过差值计算。如果差值小于环形大小的一半，说明第一个位置在第二个位置之后；否则，第二个位置在第一个位置之后。这样就能正确处理溢出情况，返回真正最小的 Stride。


