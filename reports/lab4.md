# Lab 4: 硬链接实现报告

## 1. 3个函数功能总结

### 1.1 sys_fstat() 功能总结

`sys_fstat()` 系统调用用于获取已打开文件的状态信息。该函数接收文件描述符 `fd` 和用户空间指针 `st`，通过 `fd` 从当前任务的 `fd_table` 中查找对应的文件对象。由于 `fd_table` 存储的是 `Arc<dyn File>` 类型的 trait object，无法直接向下转型，因此在 `File` trait 中添加了 `stat()` 方法。对于 `OSInode` 类型，`stat()` 方法会访问底层的 `Inode` 对象，获取其 `inode_id`（inode 编号）、`nlink`（硬链接计数）和文件类型（目录或普通文件），填充 `Stat` 结构体并写回用户空间。对于 `Stdin` 和 `Stdout` 等特殊文件，`stat()` 返回 `None`，系统调用返回 -1。该实现遵循 POSIX 标准，确保用户程序可以正确查询文件元数据。

### 1.2 sys_linkat() 功能总结

`sys_linkat()` 系统调用用于为已有文件创建硬链接。该函数接收旧路径 `old_name` 和新路径 `new_name` 的用户空间指针。首先，使用 `translated_str` 将用户空间字符串转换为内核空间字符串，并检查新旧路径是否相同（相同时返回 -1）。然后，通过 `ROOT_INODE.find()` 查找旧文件对应的 `Inode` 对象。如果找到，调用 `ROOT_INODE.link()` 方法在根目录下创建新的目录项，指向相同的 inode，并增加 inode 的 `nlink` 计数。`link()` 方法会检查新名称是否已存在，如果不存在则分配新的目录项空间，写入指向目标 inode 的 `DirEntry`，最后调用 `inode.inc_nlink()` 增加硬链接计数。整个操作是原子的，确保文件系统一致性。

### 1.3 sys_unlinkat() 功能总结

`sys_unlinkat()` 系统调用用于删除文件的硬链接。该函数接收文件路径 `name` 的用户空间指针。通过 `translated_str` 转换路径后，调用 `ROOT_INODE.unlink()` 方法从目录中移除对应的目录项。`unlink()` 方法首先查找目标文件的 inode_id，然后创建 `Inode` 对象并调用 `dec_nlink()` 减少硬链接计数。同时，该方法会移动目录中的后续条目来填补被删除条目的空隙，并更新目录大小。如果 `dec_nlink()` 返回 `true`（表示 nlink 降为 0），说明这是最后一个硬链接，系统会调用 `ROOT_INODE.delete_inode()` 释放 inode 及其所有数据块。释放数据块时，会遍历 inode 的直接块、间接一级块和间接二级块，逐个调用 `fs.dealloc_data()` 释放，最后调用 `fs.dealloc_inode()` 释放 inode 本身。

## 2. 问答题

**问题：在我们的 easy-fs 中，root inode 起着什么作用？如果 root inode 中的内容损坏了，会发生什么？**

**回答：**

在 easy-fs 文件系统中，root inode（根 inode，inode_id 为 0）扮演着文件系统的入口点角色。它的具体作用包括：

1. **文件系统根目录**：root inode 对应根目录 `/`，所有文件和子目录都直接或间接地挂载在 root inode 下。文件系统初始化时，会首先分配 inode_id 为 0 的 inode 并标记为目录类型。

2. **路径解析起点**：当用户通过 `open()`、`find()` 等系统调用访问文件时，文件系统总是从 root inode 开始解析路径。例如，访问文件 `foo.txt` 时，内核会调用 `ROOT_INODE.find("foo.txt")` 来定位目标文件的 inode。

3. **文件创建入口**：创建新文件时，通过 `ROOT_INODE.create()` 在根目录下添加新的目录项，并分配新的 inode。

4. **全局静态引用**：在操作系统内核中，`ROOT_INODE` 通过 `lazy_static!` 宏初始化为全局静态变量，在文件系统首次访问时被加载到内存中。

**如果 root inode 内容损坏，会导致以下严重后果：**

1. **文件系统不可用**：由于所有路径解析都从 root inode 开始，root inode 损坏意味着无法定位任何文件和目录，整个文件系统将变得不可访问。

2. **系统启动失败**：内核启动时会加载文件系统并初始化 `ROOT_INODE`。如果 root inode 损坏（例如 magic number 错误、目录结构混乱），会导致断言失败或 panic，系统无法启动。

3. **数据丢失风险**：尝试修复损坏的 root inode 可能导致数据进一步丢失。因为目录项（Dirent）包含文件名和 inode_id 的映射关系，如果这些映射损坏，即使数据块完好也无法访问。

4. **无法恢复**：与数据块不同，root inode 没有备份。如果 inode 区域的数据损坏（如被覆写、位翻转），只能通过重建文件系统来恢复，这将导致所有数据丢失。

**保护措施建议**：
- 定期备份超级块和 inode 区域
- 使用文件系统检查工具（如 fsck）定期验证 root inode 完整性
- 实现日志文件系统（journaling），在崩溃后恢复 root inode 状态
- 使用只读挂载模式保护关键系统文件


