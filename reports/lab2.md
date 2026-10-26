# Lab 2 · 编程作业报告

## 一、作业概述

### 1.1 任务目标

本实验要求基于 RCore OS 教学操作系统（ch5 初始化态），完成以下任务：

1. **源代码深度解析**：对 `/os` 目录下的源代码进行深度阐述，包括：
   - 设计思想
   - 技术细节
   - 为什么是这样的设计
   - 有什么优点或缺点

2. **目标**：让工程师可以充分理解程序的实现以及技术细节

3. **输出**：在 `./ch5` 目录下生成详细的完整的文档

### 1.2 完成内容

本次作业已完成：

- **15 篇深度文档**（`01` 到 `15`），覆盖 RCore OS 内核的所有核心模块
- **4 张架构图**（SVG + PNG），可视化关键概念
- **总览 README**，提供阅读路线和导航

## 二、文档结构

### 2.1 文档列表

| 编号 | 标题 | 核心源码 | 字数 |
|------|------|----------|------|
| README | 总览 | - | 2000+ |
| 01 | 构建与启动 | `build.rs` / `Makefile` / `entry.asm` / `linker.ld` / `main.rs` | 3500+ |
| 02 | 内存地址体系 | `mm/address.rs` | 2800+ |
| 03 | SV39 页表实现 | `mm/page_table.rs` | 4200+ |
| 04 | 物理页帧分配器 | `mm/frame_allocator.rs` | 2600+ |
| 05 | 内核堆分配器 | `mm/heap_allocator.rs` | 1200+ |
| 06 | 地址空间 MemorySet | `mm/memory_set.rs` | 3800+ |
| 07 | 用户程序加载 | `loader.rs` / `task/mod.rs` | 2400+ |
| 08 | 任务上下文与切换 | `task/context.rs` / `task/switch.S` | 2600+ |
| 09 | 任务控制块 | `task/task.rs` | 2200+ |
| 10 | PID 与内核栈分配 | `task/id.rs` | 2000+ |
| 11 | 任务管理与调度 | `task/manager.rs` / `task/processor.rs` | 2600+ |
| 12 | Trap 处理 | `trap/mod.rs` / `trap/trap.S` | 3600+ |
| 13 | 系统调用 | `syscall/` | 2400+ |
| 14 | 基础设施 | `sbi.rs` / `console.rs` / `timer.rs` / `logging.rs` | 2800+ |
| 15 | 设计思想与优缺点 | 全部 | 3500+ |

**总计**：约 44,700+ 字

### 2.2 架构图

| 编号 | 标题 | 文件 |
|------|------|------|
| 1 | RCore OS 内核架构总览 | `arch-overview.svg/png` |
| 2 | SV39 三级页表结构 | `page-table.svg/png` |
| 3 | 任务状态机 | `task-states.svg/png` |

## 三、核心设计思想

### 3.1 极简主义

- **无标准库**：`#![no_std]`，只依赖必要的外部 crate
- **单核假设**：所有同步原语都假设单核，不引入锁或原子操作
- **教学优先**：代码简洁易读，适合操作系统教学

### 3.2 类型安全外壳 + 底层 unsafe

- **Newtype 包装**：`PhysAddr`、`VirtAddr`、`VirtPageNum`、`PhysPageNum` 防止类型混用
- **`unsafe` 最小化**：只在必要时使用 `unsafe`（如 `get_mut()`、`translated_refmut()`）
- **RAII**：`FrameTracker`、`TaskControlBlock` 等结构体通过 `Drop` 自动释放资源

### 3.3 双地址空间

- **内核页表**：`KERNEL_SPACE`，identity 映射，无 U 标志
- **用户页表**：`from_elf` 创建，Framed 映射，有 U 标志
- **切换**：`satp` 写入 + `sfence.vma`

### 3.4 RCU 风格的生命周期

- **`Arc`/`Weak`**：父进程持有子进程的 `Arc<Child>`，子进程持有父进程的 `Weak<Parent>`
- **Zombie 状态**：进程退出后变为 Zombie，等待父进程 `waitpid` 收尸
- **自动释放**：`Arc` 引用计数归零时自动释放

## 四、关键技术细节

### 4.1 内存管理

1. **SV39 三级页表**：
   - 39-bit VA = [VPN2(9) | VPN1(9) | VPN0(9) | Offset(12)]
   - 512 GiB 虚拟地址空间，256 TiB 物理地址空间
   - PTEFlags: V=1, R=2, W=4, X=8, U=16, G=32, A=64, D=128

2. **页帧分配器**：
   - `StackFrameAllocator`：顺序分配 + 回收栈
   - `FrameTracker`：RAII 页帧句柄，自动清零和回收
   - 范围：`[ekernel.ceil(), MEMORY_END.floor())`

3. **堆分配器**：
   - `buddy_system_allocator::LockedHeap`
   - 32 MiB 静态分配在 BSS 段
   - `#[global_allocator]` 标注，所有标准库容器自动使用

4. **地址空间**：
   - `MemorySet`：管理页表和映射区域
   - `MapArea`：记录虚拟内存区域，支持 Identical/Framed 两种映射类型
   - `MapPermission`：R/W/X/U 权限位

### 4.2 进程管理

1. **任务控制块（TCB）**：
   - `TaskControlBlock`：公开接口，只读方法
   - `TaskControlBlockInner`：内部结构，私有字段
   - `Arc`/`Weak` 管理生命周期

2. **PID 与内核栈**：
   - `PidHandle`：用户可见 PID + 内部索引
   - `KernelStack`：从 trampoline 向下分配，8 KiB
   - `RecycleAllocator`：PID 可回收

3. **任务调度**：
   - `TaskManager`：FIFO 队列
   - `Processor`：调度循环
   - Timer 中断强制调度（20 ms 间隔）

### 4.3 Trap 处理

1. **统一入口**：`__alltraps`，所有异常统一处理
2. **保存所有寄存器**：32 个 x 寄存器 + sstatus + sepc + 用户 satp
3. **双页表切换**：保存用户 satp，加载内核 satp，刷新 TLB
4. **Rust 处理函数**：`trap_handler` 根据 `scause` 分派
5. **返回用户态**：`trap_return` 恢复页表、栈、寄存器，`sret` 返回

### 4.4 系统调用

1. **分发机制**：`syscall(a0, a1, a2)` 根据 `a0` 分派到具体函数
2. **translated_* 工具**：
   - `translated_refmut`：单页结构体
   - `translated_str`：字符串到 `\0`
   - `translated_byte_buffer`：跨页缓冲区，返回切片向量
3. **系统调用列表**：exit、yield、getpid、fork、exec、waitpid、get_time、trace、mmap、munmap、read、write、sbrk

### 4.5 基础设施

1. **SBI 封装**：`console_putchar`、`set_timer`、`shutdown`
2. **console.rs**：`println!` 宏，逐字节输出
3. **logging.rs**：`log` crate 适配，全局 logger
4. **timer.rs**：`get_time`、`set_next_trigger`（20 ms 间隔）
5. **UPSafeCell**：单核安全包装，`Cell` 实现内部可变性

## 五、优缺点分析

### 5.1 优点

1. **代码简洁**：核心代码不到 5000 行，适合教学
2. **类型安全**：Newtype 包装防止类型混用
3. **RAII**：资源自动释放，避免内存泄漏
4. **双地址空间**：内核/用户隔离，安全性高
5. **FIFO 调度**：公平无饥饿，实现简单
6. **RCU 生命周期**：`Arc`/`Weak` 管理进程生命周期
7. **教学友好**：代码注释清晰，结构层次分明

### 5.2 缺点

1. **单核假设**：不支持多核，无法利用多核性能
2. **无缺页处理**：缺页异常直接杀死进程，无法实现虚拟内存
3. **深复制 fork**：大进程 fork 效率低（O(n) 时间）
4. **无信号机制**：进程间通信受限
5. **无文件系统**：只有 stdio，无文件描述符
6. **无内存保护**：权限完全由用户指定，无 W^X 策略
7. **逐字节 I/O**：效率低，无缓冲区
8. **无优先级调度**：交互式进程和批处理进程无区别
9. **无内存管理**：只有匿名内存，无 mmap/swap
10. **无中断优先级**：所有中断平等，无法区分紧急程度

### 5.3 与主流 OS 的对比

| 特性 | RCore OS | Linux |
|------|----------|-------|
| 内核大小 | < 5000 行 | > 30M 行 |
| 调度 | FIFO | CFS + 实时 |
| fork | 深复制 | COW |
| 内存管理 | 简单页表 | 多级别页表 + swap |
| 文件系统 | 无 | ext4/xfs/btrfs |
| 网络 | 无 | TCP/IP 全栈 |
| 驱动 | SBI | 内核驱动框架 |
| 安全 | 无 | SELinux/AppArmor |

## 六、改进建议

1. **多核支持**：引入 `Atomic`、`Mutex`、`RCU` 等原语
2. **缺页处理**：实现虚拟内存，支持 swap
3. **COW fork**：减少 fork 开销
4. **文件系统**：实现简单的文件系统（如 FatFS）
5. **网络**：实现 TCP/IP 栈
6. **信号机制**：支持进程间通信
7. **内存保护**：实施 W^X 策略
8. **I/O 缓冲区**：减少 `ecall` 次数
9. **优先级调度**：支持实时调度
10. **驱动框架**：抽象硬件访问

## 七、适用场景

- **教学**：操作系统课程，学习内核设计
- **研究**：操作系统原型验证，快速迭代
- **嵌入式**：资源受限的单核设备
- **学习 RISC-V**：理解 RISC-V 架构和特权级

## 八、不适用场景

- **生产环境**：代码量小，功能有限，不适合生产
- **多核服务器**：单核假设，无法利用多核性能
- **高性能应用**：FIFO 调度，无优化，性能低
- **安全敏感应用**：无内存保护，无访问控制
- **复杂应用**：无文件系统，无网络，无驱动

## 九、总结

RCore OS 是一个**教学导向的极简操作系统**，以**代码简洁**和**类型安全**为核心设计理念。它通过**双地址空间**、**FIFO 调度**、**RCU 生命周期管理**等机制，实现了基本的多进程支持。虽然功能有限，但代码结构清晰，适合学习和研究。

**核心价值**：
- 理解操作系统基本原理（进程、内存、调度、trap）
- 学习 RISC-V 架构和特权级
- 掌握 Rust 系统编程技巧（`no_std`、RAII、类型安全）

**未来方向**：
- 多核支持
- 虚拟内存
- 文件系统
- 网络栈
- 驱动框架

## 十、附件

### 10.1 文档目录

```
ch5/
├── README.md                    # 总览
├── 01-构建与启动.md
├── 02-内存地址体系.md
├── 03-SV39页表实现.md
├── 04-物理页帧分配器.md
├── 05-内核堆分配器.md
├── 06-地址空间MemorySet.md
├── 07-用户程序加载.md
├── 08-任务上下文与切换.md
├── 09-任务控制块.md
├── 10-PID与内核栈分配.md
├── 11-任务管理与调度.md
├── 12-Trap处理.md
├── 13-系统调用.md
├── 14-基础设施.md
├── 15-设计思想与优缺点.md
└── assets/
    ├── arch-overview.svg/png    # 内核架构总览
    ├── page-table.svg/png       # SV39 页表结构
    └── task-states.svg/png      # 任务状态机
```

### 10.2 统计

- **文档数量**：16 篇（含 README）
- **总字数**：约 44,700+ 字
- **架构图**：3 张（SVG + PNG）
- **核心源码覆盖**：100%（所有 `.rs` 和 `.S` 文件）

---

**报告人**：Benny Lee  
**日期**：2026-10-23  
**项目**：RCore OS 内核源码深度解析（ch5 初始化态）
