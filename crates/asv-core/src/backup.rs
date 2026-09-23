//! 备份：目录快照 + sha256 清单。
//!
//! 设计取舍与 Node 版一致：
//! - 目录快照而非 zip（无第三方压缩依赖，可增量对比、可人工检查、可直接还原）
//! - SQLite 连 -wal/-shm 一起拷，否则丢最新事务
//! - 每个文件记 sha256，还原时可校验
//!
//! 布局：`<out>/manifest.json` + `<out>/files/<相对路径>`（镜像原始绝对路径）。

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::providers::Provider;
use crate::scan;

/// 绝对路径 → 快照内相对路径。Windows 盘符降为小写目录名，POSIX 根归到 root/。
pub fn abs_to_rel(abs: &Path) -> String {
    let norm = abs.to_string_lossy().replace('/', "\\");
    // 形如 C:\...
    let bytes = norm.as_bytes();
    if norm.len() >= 2 && bytes[1] == b':' {
        let drive = norm[..1].to_lowercase();
        let rest = norm[2..].trim_start_matches(['\\', '/']);
        return format!("{}\\{}", drive, rest);
    }
    if norm.starts_with('\\') {
        return format!("root\\{}", norm.trim_start_matches(['\\', '/']));
    }
    norm.replace([':', '\\', '/'], "_")
}

/// 快照内相对路径 → 绝对路径（还原用）。
pub fn rel_to_abs(rel: &str) -> String {
    let parts: Vec<&str> = rel.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 && parts[0].len() == 1 && parts[0].chars().all(|c| c.is_ascii_lowercase()) {
        return format!("{}:\\{}", parts[0].to_uppercase(), parts[1..].join("\\"));
    }
    if parts[0] == "root" {
        return format!("/{}", parts[1..].join("/"));
    }
    parts.join("/")
}

/// 流式 sha256。
pub fn sha256(file: &Path) -> anyhow::Result<String> {
    let mut f = fs::File::open(file)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(h.finalize()))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// 把 provider 给出的原始路径（可能是目录）展开为文件清单。
pub fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for p in paths {
        if !p.exists() {
            continue;
        }
        if p.is_dir() {
            for f in crate::util::walk_files(p, &[], 8) {
                out.insert(f);
            }
        } else {
            out.insert(p.clone());
        }
    }
    out.into_iter().collect()
}

pub struct BackupReport {
    pub out: PathBuf,
    pub manifest: Value,
}

/// 执行备份。agent 为 Some 时只备份该 agent。
pub fn backup(
    providers: &mut [Provider],
    out: &Path,
    agent: Option<&str>,
    mut on_progress: impl FnMut(&str, u64),
) -> anyhow::Result<BackupReport> {
    if let Some(a) = agent {
        if !providers.iter().any(|p| p.id() == a) {
            anyhow::bail!("未知的 Agent：{a}");
        }
    }
    let picked_ids: Vec<String> = agent
        .map(|a| vec![a.to_string()])
        .unwrap_or_else(|| providers.iter().map(|p| p.id().to_string()).collect());
    // 只对 picked 的 provider 做发现
    let d = scan::discover_all_filtered(providers, Some(&picked_ids));

    fs::create_dir_all(out.join("files"))?;

    let mut agents_json = Vec::new();
    for (id, label, e) in &d.agents {
        agents_json.push(json!({
            "id": id, "label": label, "available": e.available, "count": e.count,
        }));
    }

    let mut manifest = json!({
        "format": "agent-session-vault/backup",
        "version": 1,
        "createdAt": chrono::Utc::now().timestamp_millis(),
        "host": hostname(),
        "platform": std::env::consts::OS,
        "home": dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "agents": agents_json,
        "sessions": [],
        "files": [],
        "errors": [],
    });

    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut total_bytes = 0u64;

    for s in &d.sessions {
        let paths = match providers
            .iter_mut()
            .find(|p| p.id() == s.agent)
            .map(|p| p.original_paths(s))
        {
            Some(paths) => paths,
            None => continue,
        };
        let expanded = match std::panic::catch_unwind(|| expand_paths(&paths)) {
            Ok(v) => v,
            Err(_) => {
                manifest["errors"].as_array_mut().unwrap().push(json!({
                    "uid": s.uid, "error": "收集原始路径失败",
                }));
                continue;
            }
        };

        manifest["sessions"].as_array_mut().unwrap().push(json!({
            "uid": s.uid,
            "agent": s.agent,
            "sessionId": s.session_id,
            "title": s.title,
            "projectPath": s.project_path,
            "createdAt": s.created_at,
            "updatedAt": s.updated_at,
            "sourcePath": s.source_path,
            "sourceDb": s.source_db,
            "bytes": s.bytes,
            "files": expanded.iter().map(|p| abs_to_rel(p)).collect::<Vec<_>>(),
        }));

        for abs in &expanded {
            let rel = abs_to_rel(abs);
            if done.contains(&rel) {
                continue;
            }
            done.insert(rel.clone());
            let dest = out.join("files").join(&rel);
            match fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))
                .and_then(|_| fs::copy(abs, &dest))
            {
                Ok(n) => {
                    let mtime = fs::metadata(abs)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    match sha256(&dest) {
                        Ok(sum) => {
                            total_bytes += n;
                            manifest["files"].as_array_mut().unwrap().push(json!({
                                "rel": rel, "size": n, "mtimeMs": mtime, "sha256": sum,
                            }));
                            on_progress(&rel, n);
                        }
                        Err(e) => {
                            manifest["errors"].as_array_mut().unwrap().push(json!({ "path": abs.to_string_lossy(), "error": e.to_string() }));
                        }
                    }
                }
                Err(e) => {
                    manifest["errors"].as_array_mut().unwrap().push(json!({ "path": abs.to_string_lossy(), "error": e.to_string() }));
                }
            }
        }
    }

    manifest["summary"] = json!({
        "sessions": manifest["sessions"].as_array().unwrap().len(),
        "files": manifest["files"].as_array().unwrap().len(),
        "bytes": total_bytes,
        "errors": manifest["errors"].as_array().unwrap().len(),
    });
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(BackupReport { out: out.to_path_buf(), manifest })
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}
