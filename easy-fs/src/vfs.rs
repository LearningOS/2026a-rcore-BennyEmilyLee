use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    inode_id: u32,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        inode_id: u32,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            inode_id,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let inode_id = {
            let fs = self.fs.lock();
            self.read_disk_inode(|disk_inode| {
                self.find_inode_id(name, disk_inode).map(|inode_id| {
                    let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                    (block_id, block_offset, inode_id)
                })
            })
        };
        inode_id.map(|(block_id, block_offset, inode_id)| {
            Arc::new(Self::new(
                block_id,
                block_offset,
                inode_id,
                self.fs.clone(),
                self.block_device.clone(),
            ))
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        debug_assert!(new_inode_id < fs.inode_num() as u32);
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            new_inode_id,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        // Leave the dirty blocks in the write-back cache: they are flushed when
        // the cache evicts them (`BlockCache::drop`). Flushing here would write
        // the inode/indirect/bitmap blocks again on every single write syscall,
        // which is orders of magnitude slower on a host that syncs on each
        // guest block write.
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
    /// Get the inode id of this inode
    pub fn inode_id(&self) -> u32 {
        self.inode_id
    }
    /// Get the nlink count of this inode
    pub fn nlink(&self) -> u32 {
        self.read_disk_inode(|disk_inode| disk_inode.nlink)
    }
    /// Get the size of this inode
    pub fn size(&self) -> u32 {
        self.read_disk_inode(|disk_inode| disk_inode.size)
    }
    /// Check if this inode is a directory
    pub fn is_dir(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }
    /// Check if this inode is a file
    pub fn is_file(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_file())
    }
    /// Increment nlink count
    pub fn inc_nlink(&self) {
        self.modify_disk_inode(|disk_inode| {
            disk_inode.nlink += 1;
        });
        block_cache_sync_all();
    }
    /// Decrement nlink count, return whether nlink reached 0
    pub fn dec_nlink(&self) -> bool {
        let nlink = self.modify_disk_inode(|disk_inode| {
            disk_inode.nlink -= 1;
            disk_inode.nlink
        });
        block_cache_sync_all();
        nlink == 0
    }
    /// Create a hard link under current directory to the given inode
    pub fn link(&self, name: &str, inode: &Arc<Inode>) -> Result<(), String> {
        if self.find(name).is_some() {
            return Err("File already exists".to_string());
        }
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            self.increase_size(new_size as u32, disk_inode, &mut fs);
            let dirent = DirEntry::new(name, inode.inode_id);
            disk_inode.write_at(file_count * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
        });
        inode.inc_nlink();
        block_cache_sync_all();
        Ok(())
    }
    /// Remove a hard link (directory entry) under current directory
    /// Returns the inode if it was removed, along with whether it was the last link
    pub fn unlink(&self, name: &str) -> Option<(Arc<Inode>, bool)> {
        let fs = self.fs.lock();
        let inode_id = {
            self.read_disk_inode(|disk_inode| {
                assert!(disk_inode.is_dir());
                self.find_inode_id(name, disk_inode)
            })?
        };
        let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
        let inode = Arc::new(Self::new(
            block_id,
            block_offset,
            inode_id,
            self.fs.clone(),
            self.block_device.clone(),
        ));
        let last_link = inode.dec_nlink();
        self.modify_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut found_idx = None;
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device);
                if dirent.name() == name {
                    found_idx = Some(i);
                    break;
                }
            }
            if let Some(idx) = found_idx {
                let last_idx = file_count - 1;
                if idx != last_idx {
                    let mut last_dirent = DirEntry::empty();
                    disk_inode.read_at(
                        last_idx * DIRENT_SZ,
                        last_dirent.as_bytes_mut(),
                        &self.block_device,
                    );
                    disk_inode.write_at(idx * DIRENT_SZ, last_dirent.as_bytes(), &self.block_device);
                }
                disk_inode.size = ((file_count - 1) * DIRENT_SZ) as u32;
            }
        });
        block_cache_sync_all();
        Some((inode, last_link))
    }
    /// Free an inode that has lost its last directory entry (nlink == 0).
    /// Frees its data blocks, clears its inode record, and frees the inode bit.
    ///
    /// One easy-fs block holds the inode *bitmap* plus the first `|DiskInode|`
    /// records, so for inode 0 the bitmap block **is** the inode block (block 1).
    /// If `block_cache_sync_all` runs in between, it writes and drops the dirty
    /// cache entry; the re-read in `dealloc_inode` would then hand the bitmap the
    /// zeroed record copy, silently leaking the bit. Therefore all three steps
    /// must happen with no sync in the middle.
    pub fn free_inode(&self) {
        let mut fs = self.fs.lock();
        // 1. release the data blocks
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        // 2. clear the inode record
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, |disk_inode: &mut DiskInode| {
                disk_inode.initialize(DiskInodeType::File);
            });
        // 3. release the inode bitmap bit (no sync above, see doc comment)
        fs.dealloc_inode(self.inode_id);
        block_cache_sync_all();
    }
}
