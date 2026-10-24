# Local patches to apfs 0.4.1

Upstream: https://crates.io/crates/apfs 0.4.1 (MIT, repository https://github.com/Dil4rd/dpp).
This directory is the unmodified crate except for (all changes are marked `LOCAL PATCH` in the code):

* `src/lib.rs`
  * `list_volumes()` + `VolumeSlot` – enumerate **all** volumes of a container (macOS containers hold
    System, Data, Preboot, Recovery, VM...). Upstream only mounts the first non-empty volume.
  * `ApfsVolume::open_index()` – mount the volume in a given `nx_fs_oid` slot (`open()` is unchanged:
    first volume).
  * `VolumeInfo` gained `index`, `fs_flags`, `incompatible_features`, `num_snapshots`, `uuid` and the
    helpers `is_encrypted()`, `is_sealed()`, `is_case_insensitive()`, `is_incomplete_restore()`,
    `is_encryption_rolling()`, so a caller can tell what the volume uses before reading it.
  * `DirEntry::cloned` – set for files whose inode carries `INODE_WAS_CLONED` / `INODE_WAS_EVER_CLONED`.
* `src/catalog.rs` – the two inode flag constants and the `cloned` value in `list_directory()`.
* `src/testutil.rs` (feature `testutil`) – builder of small synthetic APFS containers (several volumes,
  files, directories, symlinks, clones, snapshots count, encrypted / sealed flags) used by the tests of
  the plugins. Written from Apple File System Reference, **not** validated against macOS-written images.
* `tests/synthetic.rs` – tests of the above.
* `Cargo.toml` – `testutil` feature, benchmark and `criterion` dev-dependency removed (the bench file is
  not shipped).

What the crate does and does not do with the features the plugins warn about (verified by reading the
code, not on a real macOS image):

* **Encryption** – not supported at all. Both file data and catalog nodes of an encrypted volume are
  ciphertext; the plugins therefore refuse to read such a volume and only report it.
* **Snapshots** – only the current (live) state of a volume is read; snapshot contents are not reachable.
* **Clones** – a clone owns an inode but shares the data stream (`private_id`) and therefore the
  `j_file_extent` records of its source. Extents are looked up by `private_id`, so clone data reads
  through the shared extents. What is not available is the clone relationship / space sharing information.
* **Sealed (signed system) volumes** – tree nodes may be hashed / stored without object headers; the
  reader's checksum check is expected to fail on those, the plugins report the volume as unreadable.
