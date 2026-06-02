//! clove optional SQLite index (M1+).
//!
//! Mirrors the file store into an FTS5-backed SQLite cache for fast queries at
//! scale. Fully rebuildable from the files and `.gitignore`d — deleting it loses
//! nothing. Empty in M0 (file-only milestone); see IMPLEMENTATION_PLAN.md M1.
