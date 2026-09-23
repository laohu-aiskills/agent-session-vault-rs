//! 还原：默认【只演练不落盘】。
//!
//! 还原写回 ~/.claude、~/.workbuddy 这类个人数据目录，必须先出计划、列冲突；
//! 显式 apply 才写入，覆盖已存在文件还需 overwrite。
//! 跨机器还原：target 参数把源机 home 前缀整体替换。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::backup::{rel_to_abs, sha256};

#[derive(Debug)]
pub struct RestoreItem {
    pub rel: String,
    pub src: PathBuf,
    pub dest: PathBuf,
    pub size: u64,
    pub conflict: bool,
    pub identical_size: bool,
    pub remapped: bool,
    pub integrity: String, // not-checked | ok | MISMATCH
}

pub struct RestorePlan {
    pub manifest: Value,
    pub dir: PathBuf,
    pub target: Option<String>,
    pub src_home: String,
    pub sessions: usize,
    pub items: Vec<RestoreItem>,
    pub conflicts: usize,
    pub missing: usize,
    pub bytes: u64,
}

/// 生成还原计划（不落盘）。
pub fn plan_restore(dir: &Path, agent: Option<&str>, uid: Option<&str>, target: Option<&str>, verify: bool) -> anyhow::Result<RestorePlan> {
    let manifest_file = dir.join("manifest.json");
    if !manifest_file.exists() {
        anyhow::bail!("不是有效的备份目录（缺少 manifest.json）：{}", dir.display());
    }
    let manifest: Value = serde_json::from_str(&fs::read_to_string(&manifest_file)?)?;
    if manifest["format"].as_str() != Some("agent-session-vault/backup") {
        anyhow::bail!("备份格式不匹配：{}", manifest["format"]);
    }

    let empty = Vec::new();
    let all_sessions = manifest["sessions"].as_array().unwrap_or(&empty);
    let sessions: Vec<&Value> = all_sessions
        .iter()
        .filter(|s| agent.map(|a| s["agent"].as_str() == Some(a)).unwrap_or(true))
        .filter(|s| uid.map(|u| s["uid"].as_str() == Some(u)).unwrap_or(true))
        .collect();

    // rel → 该文件归属的会话
    let mut wanted: std::collections::HashMap<String, &Value> = Default::default();
    for s in &sessions {
        for rel in s["files"].as_array().unwrap_or(&empty) {
            if let Some(r) = rel.as_str() {
                wanted.insert(r.to_string(), s);
            }
        }
    }

    let src_home = manifest["home"].as_str().unwrap_or("").to_string();
    let dst_home = target.map(String::from);

    let mut items = Vec::new();
    for f in manifest["files"].as_array().unwrap_or(&empty) {
        let Some(rel) = f["rel"].as_str() else { continue };
        if !wanted.contains_key(rel) {
            continue;
        }
        let abs = rel_to_abs(rel);
        let mut dest = PathBuf::from(&abs);
        let mut remapped = false;
        if let (Some(dst), true) = (&dst_home, !src_home.is_empty()) {
            if abs.to_lowercase().starts_with(&src_home.to_lowercase()) {
                let rest = abs[src_home.len().min(abs.len())..].trim_start_matches(['\\', '/']);
                dest = PathBuf::from(dst).join(rest);
                remapped = true;
            }
        }
        let src = dir.join("files").join(rel);
        let conflict = dest.exists();
        let identical_size = conflict && fs::metadata(&dest).map(|m| m.len() == f["size"].as_u64().unwrap_or(0)).unwrap_or(false) && !remapped;
        let integrity = if verify {
            if src.exists() && sha256(&src).map(|s| s == f["sha256"].as_str().unwrap_or("")).unwrap_or(false) {
                "ok".into()
            } else {
                "MISMATCH".into()
            }
        } else {
            "not-checked".into()
        };
        items.push(RestoreItem {
            rel: rel.to_string(),
            src,
            dest,
            size: f["size"].as_u64().unwrap_or(0),
            conflict,
            identical_size,
            remapped,
            integrity,
        });
    }

    let conflicts = items.iter().filter(|i| i.conflict).count();
    let missing = items.iter().filter(|i| !i.src.exists()).count();
    let bytes = items.iter().map(|i| i.size).sum();
    let n_sessions = sessions.len();
    drop(wanted);
    Ok(RestorePlan {
        manifest,
        dir: dir.to_path_buf(),
        target: dst_home,
        src_home,
        sessions: n_sessions,
        items,
        conflicts,
        missing,
        bytes,
    })
}

pub struct ApplyResult {
    pub written: usize,
    pub skipped: usize,
    pub failed: usize,
    pub details: Vec<(String, String)>, // (status, dest)
}

/// 执行还原。未授权覆盖时冲突项一律跳过。
pub fn apply_restore(plan: &RestorePlan, overwrite: bool) -> ApplyResult {
    let mut result = ApplyResult { written: 0, skipped: 0, failed: 0, details: Vec::new() };
    for item in &plan.items {
        if !item.src.exists() {
            result.failed += 1;
            result.details.push(("missing-source".into(), item.dest.to_string_lossy().to_string()));
            continue;
        }
        if item.integrity == "MISMATCH" {
            result.failed += 1;
            result.details.push(("checksum-mismatch".into(), item.dest.to_string_lossy().to_string()));
            continue;
        }
        if item.conflict && !overwrite {
            result.skipped += 1;
            result.details.push(("conflict-skipped".into(), item.dest.to_string_lossy().to_string()));
            continue;
        }
        match fs::create_dir_all(item.dest.parent().unwrap_or(Path::new(".")))
            .and_then(|_| fs::copy(&item.src, &item.dest))
        {
            Ok(_) => {
                result.written += 1;
                result.details.push(("written".into(), item.dest.to_string_lossy().to_string()));
            }
            Err(e) => {
                result.failed += 1;
                result.details.push((format!("error: {e}"), item.dest.to_string_lossy().to_string()));
            }
        }
    }
    result
}
