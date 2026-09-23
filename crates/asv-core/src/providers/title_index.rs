//! 外部标题索引读取器。
//!
//! 部分 Agent 把「会话 id → 标题/模型」单独存在索引里（WorkBuddy 存 workbuddy.db），
//! 比从正文里猜可靠。全部只读打开；被写锁占用时不强求，返回空表由正文兜底。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::OpenFlags;

use super::jsonl_project::TitleIndexCfg;

/// 返回 (title, model) 映射。
pub fn read_sqlite_index(cfg: &TitleIndexCfg) -> HashMap<String, (Option<String>, Option<String>)> {
    let mut out = HashMap::new();
    if !cfg.db.exists() {
        return out;
    }
    let Ok(con) = rusqlite::Connection::open_with_flags(
        &cfg.db,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return out;
    };
    let mut cols = vec![cfg.id_col.clone()];
    cols.extend(cfg.title_cols.iter().cloned());
    if let Some(m) = &cfg.model_col {
        cols.push(m.clone());
    }
    let sql = format!("select {} from {}", cols.join(", "), cfg.table);
    let Ok(mut stmt) = con.prepare(&sql) else { return out };
    let Ok(rows) = stmt.query_map([], |row| {
        let id: Option<String> = row.get::<_, Option<String>>(0).unwrap_or(None);
        let mut titles: Vec<Option<String>> = Vec::new();
        for i in 1..=cfg.title_cols.len() {
            titles.push(row.get::<_, Option<String>>(i).unwrap_or(None));
        }
        let model = cfg
            .model_col
            .as_ref()
            .map(|_| row.get::<_, Option<String>>(cfg.title_cols.len() + 1).unwrap_or(None));
        Ok((id, titles, model))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        let (Some(id), titles, model) = row else { continue };
        let title = titles
            .into_iter()
            .flatten()
            .map(|s| s.trim().to_string())
            .find(|s| !s.is_empty());
        let model = model.flatten().and_then(|m| {
            let t = m.trim();
            (!t.is_empty()).then(|| t.to_string())
        });
        if title.is_some() || model.is_some() {
            out.insert(id, (title, model));
        }
    }
    out
}

/// JSONL 索引文件（每行一条 JSON）→ id → title。
pub fn read_jsonl_index(path: &Path, id_field: &str, title_fields: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(t) else { continue };
        let Some(id) = obj.get(id_field).and_then(|v| v.as_str()) else { continue };
        let title = title_fields
            .iter()
            .find_map(|f| obj.get(*f).and_then(|v| v.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(title) = title {
            out.insert(id.to_string(), title);
        }
    }
    out
}
