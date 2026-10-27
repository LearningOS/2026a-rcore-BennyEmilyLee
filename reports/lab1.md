# Lab 1 报告

## 0. sys_trace() 实现功能总结

`sys_trace(410)` 用于追踪当前任务的系统调用历史。系统调用编号 `trace_request` 决定三种功能：`0` 时把 `id` 视作 `*const u8` 读当前任务内存中一字节并返回该值（忽略 `data`）；`1` 时把 `id` 视作 `*mut u8`，将 `data` 的最低字节写入该地址并返回 `0`；`2` 时查询编号为 `id` 的系统调用被调用的次数并返回，本次调用也计入统计。内核为每个任务维护一张 `syscall_count` 计数表，每次进入系统调用时先自增再分派，故查询自身也计数；读写前用 `is_current_task_addr` 白名单校验地址，越权返回 `-1`，其余情况返回 `-1`。

---

## 1. 简答作业 

### 问答题 1：正确进入 U 态后，程序的特征（三个 bad 测例的行为）

**使用的 SBI**：RustSBI **0.4.1**。该版本通过 `mideleg`/`medeleg` 把 U 态异常委派给 S 态内核处理，应用可正常以非法指令/非法访问被内核杀退出（源文件注释中提到的 `rustsbi 0.2.0-alpha.1` 之前的退出问题已修复）。

三个 bad 测例在 U 态下各自触发一个异常，内核 `trap_handler` 捕获后打印并杀掉当前任务：

| 测例 | 用户程序动作 | 触发的异常（scause） | 内核处理 |
| --- | --- | --- | --- |
| `ch2b_bad_address.rs` | 向地址 `0x0` `write_volatile(0)` | `StoreFault` / `StorePageFault`（内存访问异常） | 打印 `PageFault in application, bad addr = 0x0, bad instruction = …`，杀任务 |
| `ch2b_bad_instructions.rs` | 在 U 态执行 `sret` 指令 | `IllegalInstruction`（特权指令异常） | 打印 `IllegalInstruction in application, kernel killed it.`，杀任务 |
| `ch2b_bad_register.rs` | 在 U 态 `csrr sstatus`（读 S 态寄存器） | `IllegalInstruction`（CSR 权限异常） | 打印 `IllegalInstruction in application, kernel killed it.`，杀任务 |

**结论**：程序在 U 态下执行 **S 态特权指令**（如 `sret`）或**访问 S 态寄存器**（如 `sstatus`）会被硬件判为 `IllegalInstruction` 异常，这正是「U 态 ≠ S 态」的直接证据——U 态代码没有特权，企图越权必然被中断拦截并交给内核处置（此处即杀掉任务）。

### 问答题 2：深入理解 __alltraps 与 __restore

`trap.S` 中 `__alltraps`（L12-38）负责**从 U 态陷入时保存现场**并调 `trap_handler`；`__restore`（L40-61）负责**从内核返回 U 态时恢复现场**。两者共用同一份栈上 `TrapContext` 内存布局（34 个 `usize` 槽位）。

#### 2.1 L40：刚进入 __restore 时，sp 代表什么？两种使用情景

进入 `__restore` 时，`sp` 指向**当前任务内核栈上已分配好的 TrapContext 起始地址**（即 `__alltraps` 里 `addi sp, sp, -34*8` 之后那个位置）。

`__restore` 有两种使用情景：

1. **正常返回**：一次陷入（ecall / 中断 / 异常）由 `trap_handler` 处理后，`call trap_handler`（L38）`ret` 回来，落到 L40 的 `__restore`。此时 `sp` 正指向内核栈上的 TrapContext。
2. **首次进入任务（任务尚未运行过）**：`TaskContext::goto_restore` 把 `ra = __restore`、`sp = 该任务内核栈顶` 预置好；`__switch` 切到该任务时 `ret` 直接跳到 `__restore`，此时 `sp` 指向内核栈顶——而 `init_app_cx` 已把该任务的 TrapContext 预先写在这个位置，所以 `sp` 同样指向 TrapContext。

两种情形下，`sp` 都指向「当前要恢复的用户现场（TrapContext）」，代码路径完全一致，这正是复用同一函数的巧妙之处。

#### 2.2 L43-48：特殊处理了哪些寄存器？对进入用户态的意义

```asm
ld t0, 32*8(sp)    ; 读出保存的 sstatus
ld t1, 33*8(sp)    ; 读出保存的 sepc
ld t2, 2*8(sp)     ; 读出保存的用户栈指针 sp
csrw sstatus, t0
csrw sepc, t1
csrw sscratch, t2
```

这三条 `csrw` 分别把三个 CSR 写回：

- **`sstatus`**：其中 **SPP 位**记录「陷入前的特权级」。恢复为 U 态值（SPP=0），`sret` 才能据此返回 U 态。同时恢复全局中断使能等状态位。
- **`sepc`**：陷入前用户程序的 PC。`sret` 会把 PC 设成 `sepc`，从而从用户程序被打断处继续执行。
- **`sscratch`**：陷入前交换进来的用户栈指针。`sret` 后用户态栈要从 `sscratch` 取回（见 L60 的交换）。

这三个 CSR 是 `sret` 能否正确「回到用户态 + 回到用户栈 + 回到用户 PC」的关键，且它们**不能**用通用 `x` 寄存器直接恢复（CSR 只能经通用寄存器中转），所以先用 `t0/t1/t2` 读出再 `csrw`。选 `t0~t2` 是因为它们已保存进栈、可自由使用。

#### 2.3 L50-56：为何跳过 x2 和 x4？

```asm
ld x1, 1*8(sp)     ; ra
ld x3, 3*8(sp)     ; gp
.set n, 5
.rept 27
   LOAD_GP %n      ; x5 ~ x31
   .set n, n+1
.endr
```

- **x2 = sp**：用户栈指针**不能**在这里直接 `ld` 恢复——因为当前 `sp` 还是内核栈地址（指向 TrapContext）。必须先完成全部 `ld`（此时要用内核 `sp` 作为基址），再在 L58 `addi sp, sp, 34*8` 释放 TrapContext，最后 L60 用 `csrrw` 把用户 `sp`（暂存在 `sscratch`）换回来。若在这里就改 `sp`，后续 `ld` 的基址会错乱。
- **x4 = tp**：RISC-V ABI 中 `tp` 是线程指针（thread pointer），本教学系统**未使用**、应用代码不会碰它（也不属于 callee-saved 集合），故保存（`__alltraps` 也跳过）与恢复都跳过，保持 0 即可。

> 这也与 `__alltraps` 的对称保存一致：那里同样跳过了 `sp`（留到 L34 单独存用户栈）和 `tp`。

#### 2.4 L60：该指令之后，sp 与 sscratch 的意义

```asm
csrrw sp, sscratch, sp
```

这是一个**原子交换**：把当前 `sp` 的值写入 `sscratch`，同时把 `sscratch` 的旧值装入 `sp`。

执行后：

- **`sp` = 用户栈指针**（原本在 `sscratch` 里，现在换到 `sp`）—— 为 `sret` 后的用户态提供正确的栈。
- **`sscratch` = 内核栈地址**（原本在 `sp` 里，现在存回 `sscratch`）—— 供下一次陷入时 `__alltraps` 的 L13 `csrrw sp, sscratch, sp` 再次交换回来，找到内核栈。

一句话：**sp 拿回用户栈，sscratch 暂存内核栈**，完成「出内核 / 入用户」的栈指针交接。

#### 2.5 __restore 中状态切换发生在哪条指令？为何之后进入用户态？

发生在 **L61 `sret`**。

`sret` 从 `sstatus` 读取 **SPP 位**（L46 已恢复为 U 态，SPP=0），据此把特权级切回 **U 态**；同时把 PC 设为 `sepc`（L47 已恢复的用户 PC），并把 `SPIE` 复制回 `SIE`（恢复中断使能）。因为 SPP=0（U 态）且 `sepc` 指向用户程序，所以 `sret` 执行后 CPU 就以 U 态、从用户程序被打断处继续执行。

> `sret` 是唯一能改变特权级的「返回」指令——它不是普通的寄存器恢复，而是硬件层面的**模式切换**。

#### 2.6 L13：该指令之后，sp 与 sscratch 的意义（__alltraps 中）

```asm
csrrw sp, sscratch, sp
```

这是 `__alltraps` 的开场交换。执行后：

- **`sp` = 内核栈顶**（原本在 `sscratch` 里，换到 `sp`）—— 陷入后要用内核栈来分配 TrapContext 并保存现场。
- **`sscratch` = 用户栈指针**（原本在 `sp` 里，存回 `sscratch`）—— 用户栈先暂存起来，等 L34 再把它写进 TrapContext 槽位，供返回时恢复。

与 L60 完全对称：**进内核时 sp 换到内核栈、sscratch 暂存用户栈**；返回时再换回来。

#### 2.7 从 U 态进入 S 态是哪一条指令发生的？

是用户程序里执行的 **`ecall`** 指令（在 `user/src/syscall.rs` 的 `syscall` 汇编封装中）。

当 CPU 处于 U 态执行 `ecall` 时：

1. 硬件把当前 PC 写入 `sepc`、记录 `sstatus.SPP = 0`（U 态）；
2. 按 `scause` 记录为 `UserEnvCall` 异常；
3. 特权级从 U 态切换到 **S 态**，PC 跳转到 `stvec`（即 `__alltraps`）。

> 严格地说，本仓库的 U 态 `ecall` 会被 **M 态 RustSBI** 先经 `medeleg` 委派回 S 态，最终落到内核 `stvec`；但**触发 U→（经 M 委派→）S 进入的动作本身是 U 态的 `ecall` 指令**。S 态内部分支见 `trap_handler` 的 `Exception::UserEnvCall`（`trap/mod.rs:55`）。