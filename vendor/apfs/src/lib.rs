// Not public: its comparator contract fails silently when broken, so callers
// are served by the readers below instead. See the module docs.
pub(crate) mod btree;
pub mod catalog;
pub mod error;
pub mod extents;
pub mod fletcher;
pub mod object;
pub mod omap;
pub mod superblock;
/// LOCAL PATCH: synthetic image builder for tests (feature `testutil`).
#[cfg(feature = "testutil")]
pub mod testutil;

pub use error::{ApfsError, Result};

/// Failure while decoding a transparently compressed file.
pub use cmpfs::CmpfsError as CompressionError;
/// `com.apple.decmpfs` header of a transparently compressed file.
pub use cmpfs::Header as CompressionHeader;
/// Where a compressed file's payload lives.
pub use cmpfs::Storage as CompressionStorage;
/// Whether an extended attribute is user data or compression machinery.
pub use cmpfs::XattrKind;

use std::io::{Read, Seek, Write};

/// Entry kind in the filesystem
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// A directory entry returned by list_directory
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub oid: u64,
    pub kind: EntryKind,
    pub size: u64,
    pub create_time: i64,
    pub modify_time: i64,
    /// LOCAL PATCH: the inode carries `INODE_WAS_CLONED` / `INODE_WAS_EVER_CLONED`
    /// (file created by, or the source of, a `clonefile`). Only set for files.
    pub cloned: bool,
}

/// Detailed file/directory metadata
#[derive(Debug, Clone)]
pub struct FileStat {
    pub oid: u64,
    pub kind: EntryKind,
    pub size: u64,
    pub create_time: i64,
    pub modify_time: i64,
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub nlink: u32,
    /// Present when the file is transparently compressed. `size` above is then
    /// the decompressed size, which the inode does not record.
    ///
    /// Also the signal that the file's compression is already resolved: the
    /// attributes listed as [`XattrKind::Compression`] must not be replicated
    /// onto an extracted copy.
    pub compression: Option<CompressionHeader>,
}

/// An extended attribute name, classified so a caller replicating metadata can
/// skip compression machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XattrEntry {
    /// Attribute name.
    pub name: String,
    /// Whether this is user data or transparent-compression machinery.
    pub kind: XattrKind,
}

/// Entry from walk() — includes full path
#[derive(Debug, Clone)]
pub struct WalkEntry {
    pub path: String,
    pub entry: DirEntry,
}

/// `apfs_fs_flags`: the volume is NOT encrypted (the bit is clear on every
/// encrypted volume, including hardware-encrypted ones on T2 / Apple silicon).
pub const APFS_FS_UNENCRYPTED: u64 = 0x1;
/// `apfs_incompatible_features`: case-insensitive names.
pub const APFS_INCOMPAT_CASE_INSENSITIVE: u64 = 0x1;
/// `apfs_incompatible_features`: encryption rolling in progress.
pub const APFS_INCOMPAT_ENC_ROLLED: u64 = 0x4;
/// `apfs_incompatible_features`: normalization-insensitive names.
pub const APFS_INCOMPAT_NORMALIZATION_INSENSITIVE: u64 = 0x8;
/// `apfs_incompatible_features`: restore from backup did not finish.
pub const APFS_INCOMPAT_INCOMPLETE_RESTORE: u64 = 0x10;
/// `apfs_incompatible_features`: sealed (signed system) volume.
pub const APFS_INCOMPAT_SEALED_VOLUME: u64 = 0x20;

/// Volume information
#[derive(Debug, Clone)]
pub struct VolumeInfo {
    pub name: String,
    pub block_size: u32,
    pub num_files: u64,
    pub num_directories: u64,
    pub num_symlinks: u64,
    /// LOCAL PATCH: slot of the volume in the container's `nx_fs_oid` array.
    pub index: usize,
    /// LOCAL PATCH: `apfs_fs_flags`.
    pub fs_flags: u64,
    /// LOCAL PATCH: `apfs_incompatible_features`.
    pub incompatible_features: u64,
    /// LOCAL PATCH: number of snapshots recorded in the volume superblock.
    pub num_snapshots: u64,
    /// LOCAL PATCH: volume UUID.
    pub uuid: [u8; 16],
}

impl VolumeInfo {
    fn from_superblock(sb: &superblock::ApfsSuperblock, block_size: u32, index: usize) -> Self {
        VolumeInfo {
            name: sb.volume_name.clone(),
            block_size,
            num_files: sb.num_files,
            num_directories: sb.num_directories,
            num_symlinks: sb.num_symlinks,
            index,
            fs_flags: sb.fs_flags,
            incompatible_features: sb.incompatible_features,
            num_snapshots: sb.num_snapshots,
            uuid: sb.uuid,
        }
    }

    /// Encrypted (software or hardware): file data AND metadata are ciphertext.
    pub fn is_encrypted(&self) -> bool {
        self.fs_flags & APFS_FS_UNENCRYPTED == 0
    }

    /// Sealed system volume (hashed B-tree nodes, possibly without object headers).
    pub fn is_sealed(&self) -> bool {
        self.incompatible_features & APFS_INCOMPAT_SEALED_VOLUME != 0
    }

    pub fn is_case_insensitive(&self) -> bool {
        self.incompatible_features & APFS_INCOMPAT_CASE_INSENSITIVE != 0
    }

    pub fn is_incomplete_restore(&self) -> bool {
        self.incompatible_features & APFS_INCOMPAT_INCOMPLETE_RESTORE != 0
    }

    pub fn is_encryption_rolling(&self) -> bool {
        self.incompatible_features & APFS_INCOMPAT_ENC_ROLLED != 0
    }
}

/// LOCAL PATCH: one used slot of the container's volume array.
#[derive(Debug)]
pub struct VolumeSlot {
    /// Position in `nx_fs_oid` (what [`ApfsVolume::open_index`] takes).
    pub index: usize,
    /// The volume's superblock summary, or why it could not be read.
    pub info: Result<VolumeInfo>,
}

/// LOCAL PATCH: enumerate every volume of the container (a macOS container
/// usually holds System, Data, Preboot, Recovery and VM). A volume whose
/// superblock cannot be read is still listed, with the error.
pub fn list_volumes<R: Read + Seek>(reader: &mut R) -> Result<Vec<VolumeSlot>> {
    let nxsb = superblock::read_nxsb(reader)?;
    let nxsb = superblock::find_latest_nxsb(reader, &nxsb)?;
    let block_size = nxsb.block_size;
    let container_omap_root = omap::read_omap_tree_root(reader, nxsb.omap_oid, block_size)?;
    let mut out = Vec::new();
    for (index, &oid) in nxsb.fs_oids.iter().enumerate() {
        if oid == 0 {
            continue;
        }
        let info = read_volume_superblock(reader, container_omap_root, block_size, oid)
            .map(|sb| VolumeInfo::from_superblock(&sb, block_size, index));
        out.push(VolumeSlot { index, info });
    }
    Ok(out)
}

fn read_volume_superblock<R: Read + Seek>(
    reader: &mut R,
    container_omap_root: u64,
    block_size: u32,
    vol_oid: u64,
) -> Result<superblock::ApfsSuperblock> {
    let vol_block = omap::omap_lookup(reader, container_omap_root, block_size, vol_oid)?;
    let (_, vol_data) = object::read_object(reader, vol_block, block_size)?;
    superblock::ApfsSuperblock::parse(&vol_data)
}

/// Pair each attribute name with its kind.
///
/// The file counts as compressed when the header attribute is present, which is
/// the same condition `read_file` decompresses on. Deriving it from the listing
/// keeps the two in step: if `read_file` treated the resource fork as a
/// payload, this reports it as machinery.
fn classify(names: Vec<String>) -> Vec<XattrEntry> {
    let compressed = names.iter().any(|name| name == cmpfs::XATTR_NAME);
    names
        .into_iter()
        .map(|name| XattrEntry {
            kind: cmpfs::classify_xattr(&name, compressed),
            name,
        })
        .collect()
}

/// High-level read-only APFS volume reader
pub struct ApfsVolume<R: Read + Seek> {
    reader: R,
    block_size: u32,
    vol_omap_root_block: u64,
    catalog_root_block: u64,
    info: VolumeInfo,
}

impl<R: Read + Seek> ApfsVolume<R> {
    /// Open an APFS container and mount the first volume.
    ///
    /// 1. Read block 0 → parse NX superblock, validate NXSB magic + Fletcher-64
    /// 2. Scan checkpoint descriptor area for latest valid NX superblock
    /// 3. Read container OMAP at omap_oid physical block
    /// 4. Find first non-zero OID in fs_oids array
    /// 5. Resolve volume OID → physical block via container OMAP
    /// 6. Parse volume superblock (APSB magic)
    /// 7. Read volume OMAP at vol.omap_oid physical block
    /// 8. Resolve vol.root_tree_oid → physical block via volume OMAP → catalog B-tree root
    /// 9. Store all state
    pub fn open(reader: R) -> Result<Self> {
        Self::open_slot(reader, None)
    }

    /// LOCAL PATCH: mount the volume in slot `index` of the container's volume
    /// array (see [`list_volumes`]) instead of the first one.
    pub fn open_index(reader: R, index: usize) -> Result<Self> {
        Self::open_slot(reader, Some(index))
    }

    fn open_slot(mut reader: R, slot: Option<usize>) -> Result<Self> {
        // Step 1-2: Read and validate container superblock
        let nxsb = superblock::read_nxsb(&mut reader)?;
        let nxsb = superblock::find_latest_nxsb(&mut reader, &nxsb)?;
        let block_size = nxsb.block_size;

        // Step 3: Read container OMAP
        let container_omap_root =
            omap::read_omap_tree_root(&mut reader, nxsb.omap_oid, block_size)?;

        // Step 4: Pick the volume OID (first non-zero one, or the requested slot)
        let (index, vol_oid) = match slot {
            None => nxsb
                .fs_oids
                .iter()
                .enumerate()
                .find(|&(_, &o)| o != 0)
                .map(|(i, &o)| (i, o)),
            Some(i) => nxsb.fs_oids.get(i).filter(|&&o| o != 0).map(|&o| (i, o)),
        }
        .ok_or(ApfsError::NoVolume)?;

        // Steps 5-6: Resolve volume OID via container OMAP, parse volume superblock
        let vol_sb =
            read_volume_superblock(&mut reader, container_omap_root, block_size, vol_oid)?;

        // Step 7: Read volume OMAP
        let vol_omap_root_block =
            omap::read_omap_tree_root(&mut reader, vol_sb.omap_oid, block_size)?;

        // Step 8: Resolve catalog root tree OID via volume OMAP
        let catalog_root_block = omap::omap_lookup(
            &mut reader,
            vol_omap_root_block,
            block_size,
            vol_sb.root_tree_oid,
        )?;

        // Step 9: Store state
        let info = VolumeInfo::from_superblock(&vol_sb, block_size, index);

        Ok(ApfsVolume {
            reader,
            block_size,
            vol_omap_root_block,
            catalog_root_block,
            info,
        })
    }

    /// Get volume metadata
    pub fn volume_info(&self) -> &VolumeInfo {
        &self.info
    }

    /// List entries in a directory by path
    pub fn list_directory(&mut self, path: &str) -> Result<Vec<DirEntry>> {
        let (oid, _inode) = if path == "/" || path.is_empty() {
            // Root directory has a well-known OID
            (catalog::ROOT_DIR_PARENT, catalog::ROOT_DIR_RECORD)
        } else {
            let (oid, inode) = catalog::resolve_path(
                &mut self.reader,
                self.catalog_root_block,
                self.vol_omap_root_block,
                self.block_size,
                path,
            )?;
            if inode.kind() != catalog::INODE_DIR_TYPE {
                return Err(ApfsError::NotADirectory(path.to_string()));
            }
            (oid, oid)
        };

        let parent = if path == "/" || path.is_empty() {
            catalog::ROOT_DIR_RECORD
        } else {
            oid
        };

        let mut entries = catalog::list_directory(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            parent,
        )?;
        self.resolve_entry_sizes(&mut entries)?;
        Ok(entries)
    }

    /// Make listed sizes agree with `stat`. A compressed file's inode records
    /// no size of its own — the real length is in the decmpfs header — and a
    /// symlink inode reports 0 where `stat` reports the target length.
    fn resolve_entry_sizes(&mut self, entries: &mut [DirEntry]) -> Result<()> {
        for entry in entries.iter_mut() {
            match entry.kind {
                EntryKind::File => {
                    if let Some(header) = self.compression(entry.oid)? {
                        entry.size = header.uncompressed_size;
                    }
                }
                EntryKind::Symlink => {
                    if let Some(target) = self.symlink_target(entry.oid)? {
                        entry.size = target.len() as u64;
                    }
                }
                EntryKind::Directory => {}
            }
        }
        Ok(())
    }

    /// Read an entire file into memory
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.read_file_to(path, &mut buf)?;
        Ok(buf)
    }

    /// Symlink targets on APFS are stored in a "com.apple.fs.symlink"
    /// extended attribute (NUL-terminated), not as file data. Returns the
    /// target bytes for symlink inodes, or None when no xattr is present.
    fn symlink_target(&mut self, oid: u64) -> Result<Option<Vec<u8>>> {
        let Some(xattr) = catalog::lookup_xattr(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            oid,
            catalog::SYMLINK_XATTR_NAME,
        )?
        else {
            return Ok(None);
        };
        let end = xattr.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        Ok(Some(xattr[..end].to_vec()))
    }

    /// Read an extended attribute, or `None` when the inode has no attribute
    /// of that name.
    ///
    /// Resolves both storage forms: values held in the catalog record and
    /// values held in a data stream, which is how resource forks are stored.
    pub fn get_xattr(&mut self, path: &str, name: &str) -> Result<Option<Vec<u8>>> {
        let (oid, _inode) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;
        self.read_xattr(oid, name)
    }

    /// Every extended attribute on a file or directory, classified.
    ///
    /// Nothing is filtered out: a caller inspecting the volume sees what the
    /// volume holds. [`XattrKind::Compression`] marks the attributes macOS
    /// hides, which [`Self::read_file`] has already resolved and which must
    /// not be copied onto an extracted file.
    pub fn list_xattrs(&mut self, path: &str) -> Result<Vec<XattrEntry>> {
        let (oid, _inode) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;
        let names = catalog::list_xattr_names(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            oid,
        )?;
        Ok(classify(names))
    }

    fn read_xattr(&mut self, oid: u64, name: &str) -> Result<Option<Vec<u8>>> {
        let value = catalog::lookup_xattr_value(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            oid,
            name,
        )?;

        match value {
            None => Ok(None),
            Some(catalog::XattrValue::Embedded(data)) => Ok(Some(data)),
            Some(catalog::XattrValue::DataStream { obj_id, size }) => {
                let extents = catalog::lookup_extents(
                    &mut self.reader,
                    self.catalog_root_block,
                    self.vol_omap_root_block,
                    self.block_size,
                    obj_id,
                )?;
                let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
                extents::read_file_data(
                    &mut self.reader,
                    self.block_size,
                    &extents,
                    size,
                    &mut data,
                )?;
                Ok(Some(data))
            }
        }
    }

    /// The `com.apple.decmpfs` header of a transparently compressed inode, or
    /// `None` when the file stores its bytes in the data fork as usual.
    fn compression(&mut self, oid: u64) -> Result<Option<CompressionHeader>> {
        let Some(attr) = self.read_xattr(oid, cmpfs::XATTR_NAME)? else {
            return Ok(None);
        };
        Ok(Some(CompressionHeader::parse(&attr)?))
    }

    /// Decompress a transparently compressed file.
    ///
    /// Whole-file, unlike the extent path: compression blocks are addressed
    /// relative to the decompressed output, so nothing can be emitted before
    /// the block covering it has been decoded.
    fn read_compressed(&mut self, oid: u64, header: &CompressionHeader) -> Result<Vec<u8>> {
        let attr = self.read_xattr(oid, cmpfs::XATTR_NAME)?.ok_or_else(|| {
            ApfsError::CorruptedData(format!("inode {oid} lost its decmpfs attribute"))
        })?;

        let resource_fork = match header.storage() {
            CompressionStorage::ResourceFork => Some(
                self.read_xattr(oid, cmpfs::RESOURCE_FORK_XATTR_NAME)?
                    .ok_or_else(|| {
                        ApfsError::CorruptedData(format!(
                            "inode {oid} is compressed into its resource fork, but has none"
                        ))
                    })?,
            ),
            _ => None,
        };

        Ok(cmpfs::decompress(&attr, resource_fork.as_deref())?)
    }

    /// Stream a file to a writer
    ///
    /// Transparently compressed files are decompressed; see [`Self::stat`] to
    /// detect one first.
    pub fn read_file_to<W: Write>(&mut self, path: &str, writer: &mut W) -> Result<u64> {
        let (oid, inode) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;

        if inode.kind() != catalog::INODE_SYMLINK_TYPE
            && let Some(header) = self.compression(oid)?
        {
            let data = self.read_compressed(oid, &header)?;
            writer.write_all(&data)?;
            return Ok(data.len() as u64);
        }

        // Symlink inodes carry no extents; read the target from the xattr.
        // Falls through to the extent read for images that store the target
        // as file data.
        if inode.kind() == catalog::INODE_SYMLINK_TYPE
            && let Some(target) = self.symlink_target(oid)?
        {
            writer.write_all(&target)?;
            return Ok(target.len() as u64);
        }

        // File extents are keyed by private_id, not the inode OID
        let file_extents = catalog::lookup_extents(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            inode.private_id,
        )?;

        extents::read_file_data(
            &mut self.reader,
            self.block_size,
            &file_extents,
            inode.size(),
            writer,
        )
    }

    /// Open a file for streaming Read+Seek access
    ///
    /// Fails on a transparently compressed file: its data fork is empty, so a
    /// reader over the extents would report a successful read of nothing. Use
    /// [`Self::read_file`] for those.
    pub fn open_file(&mut self, path: &str) -> Result<extents::ApfsForkReader<'_, R>> {
        let (oid, _) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;
        if let Some(header) = self.compression(oid)? {
            return Err(ApfsError::Unsupported(format!(
                "{path} is decmpfs-compressed (type {}); read_file decompresses it, \
                 streaming does not",
                header.compression_type
            )));
        }

        let (_oid, inode) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;

        // File extents are keyed by private_id, not the inode OID
        let file_extents = catalog::lookup_extents(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            inode.private_id,
        )?;

        Ok(extents::ApfsForkReader::new(
            &mut self.reader,
            self.block_size,
            file_extents,
            inode.size(),
        ))
    }

    /// Get metadata for a file or directory
    pub fn stat(&mut self, path: &str) -> Result<FileStat> {
        let (oid, inode) = catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        )?;

        // Symlink inodes report size 0; use the xattr target length instead.
        let mut compression = None;
        let size = if inode.kind() == catalog::INODE_SYMLINK_TYPE {
            self.symlink_target(oid)?
                .map_or(inode.size(), |t| t.len() as u64)
        } else {
            // A compressed inode records no size of its own — the data fork is
            // empty and the real length is in the decmpfs header.
            compression = self.compression(oid)?;
            compression.map_or_else(|| inode.size(), |h| h.uncompressed_size)
        };

        Ok(FileStat {
            oid,
            kind: match inode.kind() {
                catalog::INODE_DIR_TYPE => EntryKind::Directory,
                catalog::INODE_SYMLINK_TYPE => EntryKind::Symlink,
                _ => EntryKind::File,
            },
            size,
            create_time: inode.create_time,
            modify_time: inode.modify_time,
            uid: inode.uid,
            gid: inode.gid,
            mode: inode.mode,
            nlink: inode.nlink(),
            compression,
        })
    }

    /// Recursive walk of all entries
    pub fn walk(&mut self) -> Result<Vec<WalkEntry>> {
        let mut entries = Vec::new();
        self.walk_recursive(catalog::ROOT_DIR_RECORD, "", &mut entries)?;
        Ok(entries)
    }

    /// Check if a path exists
    pub fn exists(&mut self, path: &str) -> Result<bool> {
        match catalog::resolve_path(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            path,
        ) {
            Ok(_) => Ok(true),
            Err(ApfsError::FileNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn walk_recursive(
        &mut self,
        parent_oid: u64,
        parent_path: &str,
        entries: &mut Vec<WalkEntry>,
    ) -> Result<()> {
        let mut dir_entries = catalog::list_directory(
            &mut self.reader,
            self.catalog_root_block,
            self.vol_omap_root_block,
            self.block_size,
            parent_oid,
        )?;
        self.resolve_entry_sizes(&mut dir_entries)?;

        for entry in dir_entries {
            let full_path = if parent_path.is_empty() {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", parent_path, entry.name)
            };

            let is_dir = entry.kind == EntryKind::Directory;
            let oid = entry.oid;

            entries.push(WalkEntry {
                path: full_path.clone(),
                entry,
            });

            if is_dir {
                self.walk_recursive(oid, &full_path, entries)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {}
