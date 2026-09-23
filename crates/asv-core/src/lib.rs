//! Agent Session Vault — Rust 实现（core 库）。
//!
//! 与 Node 版同构：providers 适配各家落盘格式 → 统一 Session/Message 模型
//! → SQLite FTS5(trigram) 索引检索。CLI 与未来的 Tauri GUI 共用本库。

pub mod backup;
pub mod export;
pub mod index_db;
pub mod migrate;
pub mod model;
pub mod providers;
pub mod restore;
pub mod scan;
pub mod util;
