# Lab 2 · 编程作业报告

## 一、四个系统调用的功能总结

### `sys_get_time` (SYSCALL 169)

读取当前时间并写回用户态指针。从 `timer::get_time()` 取全局时钟周期数，除以/对 `1_000_000` 取余拆出秒/微秒组装成内核侧 `TimeVal`。`TimeVal` 是 16 字节结构，指针地址可能与页边界不对齐，直接 deref 会触发 page fault。正确做法是用 `translated_byte_buffer(current_user_token(), ts, len)` 逐页取物理内存切片，再把序列化的 `TimeVal` 分片 `copy_from_slice` 写回，天然处理跨界。传空指针返回 -1。

### `sys_trace` (SYSCALL 410)

用户态调试三态工具。`trace_request=0` 读一字节：用 `PageTable::from_token(current_user_token()).translate(vpn.floor())` 查用户页表，若 PTE 缺失或 `is_valid`/`readable`/`U` 任一为假则返回 -1；否则用 `PhysAddr::from(ppn) + page_offset` 计算物理地址后 `read_volatile`。`trace_request=1` 写一字节逻辑对称但检查 `writable` 而非 `readable`，成功后返回 0。`trace_request=2` 直接透传 `get_current_syscall_count(id)`，用于统计当前任务某 syscall 的调用次数。其余请求返回 -1。相比 ch3 的地址白名单，ch4 靠 MMU + U 标志位做隔离。

### `sys_mmap` (SYSCALL 222)

把 `start` 起 `len` 字节映射为匿名物理页。先校验 `start % PAGE_SIZE == 0`，再经 `prot_to_permission` 把 Linux 风格 prot（bit0 R/bit1 W/bit2 X）转成 `MapPermission`：非法位（`prot & !0x7`）或全 0 直接 -1；合法时附加 `U` 标志保证用户态可访问。最后调 `mmap_current` 委托 MemorySet，它先扫描 `[start_vpn, end_vpn)` 确认无已映射页，再用 `insert_framed_area` 分配 FrameTracker 并写页表；OOM 允许 panic，不做回收。全成功返回 0，任一条件不满足返回 -1。

### `sys_munmap` (SYSCALL 215)

解除 `[start, start+len)` 的映射。对称校验 `start` 和 `end = start+len` 页对齐（`end` 溢出或 `len==0` 时退化用 start）。调 `munmap_current` 遍历 VPN，若发现任何一页未映射立即返回 -1 且不改动任何 PTE（原子性语义）。全部映射才逐页 `page_table.unmap(vpn)` 清空 PTE，返回 0。物理 frame 不主动回收（题面允许）。

## 二、问答题

### 2.1 SV39 页表项的组成与标志位

SV39 页表项 PTE 是 64 位，从低到高：

| 位 | 名称 | 作用 |
| --- | --- | --- |
| [10:0] | Offset | 页内偏移，硬件忽略 |
| [53:10] | PPN | 物理页号，指向下一级页表或物理页帧 |
| 63 | N | No Translation，保留 |
| 62 | Reserved | 保留 |
| 61:60 | GDIR | Global Directory Index，保留 |
| 59 | AVL | Advisory，保留 |
| 58 | **D** | Dirty，写访问硬件自动置位 |
| 57 | **A** | Accessed，读写访问硬件自动置位 |
| 56 | **G** | Global，Svade/Global；置 1 跳过地址空间切换刷新 |
| 55 | **U** | User，置 1 允许 U 态访问；为 0 时 U 态访问触发 LoadPageFault/StorePageFault |
| 54 | **X** | eXecutable，置 1 允许取指 |
| 53- | Reserved | — |
| 2 | **W** | Writable，置 1 允许写；为 0 时写触发 StorePageFault |
| 1 | **R** | Readable，置 1 允许读；为 0 时读触发 LoadPageFault |
| 0 | **V** | Valid，置 1 该 PTE 有效；为 0 时任何访问都触发 page fault |

关键约束：**W=1 必须 R=1**（RISC-V 不允许写而不读），因为写操作隐含读页表并需 A/D 位访问权。U 与内核访问独立，内核无论 U 值都能访问。

### 2.2 缺页

#### 哪些异常可能是缺页导致的？

在 RISC-V 上缺页表现为 `scause` 的以下 exception code：

- **`Exception::LoadPageFault`（15）**：U/S 态读地址翻译失败
- **`Exception::StorePageFault`（15）**：U/S 态写地址翻译失败
- **`Exception::FetchPageFault`（13）**：取指翻译失败
- **`Exception::LoadAccessFault`（5）**：地址在 PMP 之外或物理访问拒绝
- **`Exception::Store/AMOAccessFault`（7）**：写侧对应情况
- **`Exception::FetchAccessFault`（1）**：取指侧对应情况

前三个是 MMU 翻译失败（PTE 无效、权限不足、U 与 MPRV 组合等），后三个是翻译成功但 PMP 拒绝。

#### 发生缺页时相关重要寄存器的值

| 寄存器 | 值 |
| --- | --- |
| `scause` | 15 = `Exception::LoadPageFault` 或 13/15/7 之一 |
| `stval` | **触发缺页的用户虚拟地址**（不是物理地址） |
| `sepc` | 触发缺页的指令地址 |
| `sstatus.SPIE` | 陷入前 SIE 的状态，硬件自动保存 |
| `sstatus.SPP` | 陷入前特权级（缺页来自 U 态则为 0） |
| `satp` | 当前活动页表根，指向 SV39 页表 |
| `x10` (a0) | 用户态原寄存器的值不变，直到内核改写 |
| `x11` (a1)、`x12` (a2) | 同上 |

处理流程：内核读 `stval` 得缺页地址，`sepc` 得指令；决定重填页表后用 `sret` 返回，`sepc` 保持不变让 CPU 重跑那条指令；决定杀进程则走 `exit_current_and_run_next()`。

#### Lazy 策略的好处

1. **fork 变快**：`fork` 只需复制页表结构（COW），子进程的读写触发页表时才拷贝物理页，避免全量复制几十 MB 地址空间。
2. **exec 变快**：进程加载时只登记 `.text`/`.data` 在磁盘上的偏移，实际读/执行时才 lazy load，节省启动时间和未使用段的物理内存。
3. **BRK/mmap 变快**：`sbrk`/`mmap` 只更新页表结构，物理页在真正访问时才分配，避免大块预留导致的内存浪费。
4. **零填充天然高效**：新分配页首次写入前是零，硬件 A/D 位配合零页映射可省去显式清零。
5. **降低整体常驻内存**：未访问的页始终不占物理内存，多进程共存时受益明显。

#### 10G 连续内存页面，SV39 页表大致占用多少内存

SV39 页表 4 级：39 位 VA = 3（页内偏移 12 位）+ 3×9（VPN0/1/2）。每级页表 512 项 × 8 字节 = **4 KiB**。

- **叶子级（PTL3，第 3 级页表）**：每 4 KiB 物理页对应一个 PTE。10 GB / 4 KiB = 2 560 000 项，需要 2 560 000 / 512 ≈ **5 000 张叶子页表**，共约 **20 MB**。
- **中间三级页表**：叶子页表按 1 GiB 大页切块。10 GiB 跨 11 个 1 GiB 段，每段 1 张 PTL2，PTL2 挂在 512 段共享的 PTL1 上，PTL1 挂在 512 段共享的 PTL0 上，再加 1 张根表。中间三级合计仅 **11 + 1 + 1 + 1 = 14 张页表 ≈ 56 KiB**。
- **总计**：**约 20 MiB 物理内存**（叶子页表主导），中间三级可忽略。

如果 10 GiB 恰好跨越更多 1 GiB 边界（例如从 0x8000_0000 开始），仍受同样上界约束，因为 10 < 512×512×512 = 134 217 728 页 = 512 TiB。数量级就是 **20 MB**。

#### Lazy mmap 的实现思路

1. **mmap 时不分配物理页**：为整个 `[start, start+len)` 建立中间级页表，但叶子 PTE 全部置空（V=0）。页表结构本身仍占内存（如 10G 需约 20 MiB），但物理页帧数为 0。
2. **可选：把叶子 PTE 指向一个只读的零物理页**（zero page），配合 R 位开启、W 位关闭，让读操作直接命中零页，写操作触发 StorePageFault。
3. **缺页处理**：内核读 `stval` 得到缺页 VA，`vpn = va.floor()`，查页表：
   - 若 PTE 无效（V=0）：分配一个新物理页（从 `frame_alloc`），根据缺页原因决定内容——
     - Lazy 加载磁盘文件：从 `mmap` 时记录的 inode/offset 通过块设备读入新页；
     - Lazy 匿名映射：分配新页并清零；
     - swap in：从 swap 分区按记录读入。
   - 若 PTE 只映射零页且触发写：分配真实物理页，把零页当前内容（通常就是全 0）复制过去，替换 PTE 并开启 W/D 位。
4. **更新页表**：写回新 PTE（置 V/R/W/U/D/A），`sfence.vma` 刷新 TLB，`sret` 让 CPU 重跑原指令。
5. **淘汰**：物理内存紧张时，把 A=1 但 D=0 的干净页直接回收，把 D=1 的脏页写回 swap 或磁盘再回收，同时清 V 位。下次访问再 lazy 换入。
6. **辅助元数据**：为每个映射区间维护 inode/offset/size 表，供缺页处理时决定来源；页表结构本身按需递归创建，不预先展开所有叶子。

### 2.3 双页表与单页表

**背景**：本 OS 用「用户页表 + 内核页表」两张独立页表（双页表），用户态跑时 `satp` 指向用户页表，进入内核后 `satp` 指向内核页表；内核线程和对应用户线程是不同任务，通过内核态 trap context 里的 `user_satp` 切换。

#### 单页表情况下如何更换页表？

单页表设计里，用户态和内核态共用一张页表。所谓"更换页表"实际是**切换 satp 根指针**：`satp::write(new_satp_token)` + `sfence.vma`。切换时机有：

- 进程切换时，把 `satp` 改成新进程的页表 token；
- 单页表没有用户/内核态切换 satp 的动作，只依赖特权级 CSR（`mstatus.MPP` / `sstatus.SPP`）和 U 标志位区分访问权。

#### 单页表情况下如何控制用户态无法访问内核页面？

内核页面对应 PTE 的 **U 位保持为 0**。用户态访问（sstatus 里 Privilege=U）时，MMU 检查 PTE.U=0 立刻触发 LoadPageFault/StorePageFault，内核捕获后杀进程。这就是「用 U 位划分地址空间」的单页表做法。缺点：U 态可看到整张页表结构的地址位（虽然访问会 fault），且 satp 中的根页表物理地址对 U 态可见（可通过时序侧信道推断），这就是 KPTI 要解决的问题。

#### 单页表有何优势？

1. **切换开销低**：进程切换只需改 satp 一个 CSR，TLB 可大量复用（`G` 位使内核页在进程间共享 TLB 条目）；双页表要切换两次 satp（用户↔内核）。
2. **实现简单**：不需要为每个内核线程单独建立页表，不需要管理"用户页表副本"和"内核页表"的同步。
3. **无 KPTI 侧信道开销**：不需要系统调用和中断入口都强制刷 TLB，避免每次陷入的 `sfence.vma` 代价。
4. **代码段共享**：内核代码页 PTE 天然被所有进程的 TLB 缓存，命中率更高。

代价：面对 Spectre v2 / Meltdown 类侧信道攻击，单页表需要 KPTI 补丁（在陷入时切换一个"最小页表"再切回完整页表），否则用户态可通过时序窥探内核内存布局。

#### 双页表实现下，何时需要更换页表？

本 OS 的双页表方案里，每次**用户态进入内核**（系统调用、定时器中断、异常）都要切换 satp 到内核页表；**内核返回用户态**（`sret` 前）要切回用户页表。具体触发点：

1. `trap_handler` 入口：`satp::write(kernel_satp_token)` + `sfence.vma`。
2. `sret` 之前（`trap_return`）：`satp::write(user_satp_token)` + `sfence.vma`，然后用 `sret` 返回 U 态。
3. 任务切换（`__switch`）时若下一个任务仍是用户态，切换前先激活下一个任务的用户页表。

#### 假设写一个单页表操作系统，你会选择何时更换页表？

在单页表方案下，"更换页表"意味着替换整张页表根：

1. **进程切换时**（`__switch` 上下文切换前后）：把 satp 从旧进程页表改为新进程页表，`sfence.vma` 刷 TLB。这是必须的。
2. **fork**：COW 场景下让父子进程共享同一 satp 根（页表结构共享，仅叶子 PTE 按需分裂），子进程第一次写触发 StorePageFault 时复制叶子 PTE 到新物理页。这样 fork 几乎零拷贝。
3. **exec**：抛弃旧页表，为新 ELF 建一张全新页表，写入 satp。
4. **系统调用和中断入口**：**不切换页表**，只靠特权级变化（S 态进入）和 U 位保护——这是单页表相对双页表的最大节省。
5. **若引入 KPTI**：陷入时切到一个"用户态最小页表"（仅映射 trampoline），出陷入前再切回完整页表；这是单页表防侧信道的补丁式做法，代价是每次陷入两次 satp 写和两次 sfence。

结论：单页表把"页表切换"和"特权级切换"解耦——只在真正换进程时改 satp，陷入/退出仅切换特权级，性能最好，但需配合 KPTI 才能安全对抗侧信道。双页表用额外的 satp 切换换来了天然隔离，代价是每次陷入的 TLB 刷新开销。


