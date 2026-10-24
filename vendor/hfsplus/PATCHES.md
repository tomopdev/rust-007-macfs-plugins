# Local patches to hfsplus 0.4.0

Upstream: https://crates.io/crates/hfsplus 0.4.0 (MIT, repository https://github.com/Dil4rd/dpp).
This directory is the unmodified crate except for:

* `src/unicode.rs` – `compare_case_insensitive` / `case_fold` now follow Apple's
  FastUnicodeCompare: U+0000 folds to 0xFFFF (sorts last), ignorable characters
  (U+200C-200F, U+202A-202E, U+206A-206F, U+FEFF) are skipped, and Greek/Cyrillic/
  Armenian/Latin Extended Additional/etc. are case-folded. Without this, HFS+
  (case-insensitive) volumes failed to find `"\0\0\0\0HFS+ Private Data"` (the hard-link
  folder every macOS volume with hard links has) and could miss names whose
  case differs in scripts missing from the 119-entry table.
