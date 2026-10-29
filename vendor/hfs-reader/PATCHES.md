# Local patches to hfs-reader 0.1.1

Upstream: https://crates.io/crates/hfs-reader 0.1.1 (**GPL-3.0-or-later**, repository https://github.com/feralfirmware/tailtalk).
The only upstream file is `src/lib.rs`; it was reworked as follows, `src/testutil.rs` is new.

**License note:** this crate is GPL-3.0-or-later and the whole `macfs-plugins` workspace is therefore GPL-3.0-or-later too
(the plugin binaries `macfs_wcx.wcx64` and `macfs.spl` link it). The upstream package ships no LICENSE file, the GPL text
was added here (`LICENSE`) as it is the license declared in the upstream `Cargo.toml`. If that is ever a problem, `hfs-reader`
can be replaced by an own HFS reader (the interface the rest of the workspace uses is only `crates/macfs-core/src/classic.rs`).

* The image is no longer a `&[u8]` held in memory: `HfsVolume<R: Read + Seek>`, `HfsVolume::open(reader)`. Only the catalog
  and the extents overflow B-tree (capped at 256 MiB) are loaded; forks are read on demand (`copy_data_fork` /
  `copy_rsrc_fork` stream in 1 MiB pieces, `read_data_fork` / `read_rsrc_fork` return a `Vec`). `HfsVolume::parse(&[u8])`
  stays (including stripping a Disk Copy 4.2 header) as a wrapper over a `Cursor`. The read methods take `&mut self`.
* Errors are `HfsError::{Io, Format}` instead of `String`, so the plugins can tell a failing device from a damaged volume.
  A fork whose extents cover fewer bytes than its logical size is an error (upstream returned the short data silently).
* Names are decoded from MacRoman (upstream turned every byte outside printable ASCII into `_`, which also made different
  names collide). `/` is kept in `name` (it is legal in a classic HFS name) and written as `:` in `rel_path`.
* New public data: `cnid`, `name`, `flags`, `create_date`, `modify_date`, `data_size()`, `rsrc_size()` of a file;
  `cnid`, `parent_cnid`, `name`, dates of a directory. Dates are Mac seconds since 1904 in the local time of the creating Mac.
* Empty directories are listed (upstream only listed directories that had children) and finding the directories is linear
  (upstream scanned all entries for every directory: quadratic on large volumes). Orphans of a damaged catalog are dropped.
* Hardening of the catalog / overflow parsing: empty record at the end of a node (index out of bounds), record count larger
  than a node can hold, name longer than its key, allocation block size that is not a multiple of 512.
* Feature `testutil`: `testutil::HfsImageBuilder` builds synthetic volumes (multi-leaf catalog with an index node,
  directory / thread records, resource forks, fragmented forks with overflow extents).
