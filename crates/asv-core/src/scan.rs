//! 扫描与索引编排：全量发现 + 增量索引。

use serde_json::Value;

use crate::index_db::IndexDb;
use crate::model::derive_title;
use crate::providers::Provider;

/// 发现所有 Provider 的会话。
pub struct AgentEntry {
    pub available: bool,
    pub restricted: bool,
    pub reason: String,
    pub count: usize,
    pub bytes: u64,
    pub verified: bool,
}

pub struct Discovered {
    pub sessions: Vec<crate::model::Session>,
    pub agents: Vec<(String, String, AgentEntry)>, // (id, label, entry)
}

pub fn discover_all(providers: &mut [Provider]) -> Discovered {
    discover_all_filtered(providers, None)
}

/// only=Some(ids) 时只发现指定 provider（备份 --agent 用）。
pub fn discover_all_filtered(providers: &mut [Provider], only: Option<&[String]>) -> Discovered {
    let mut sessions = Vec::new();
    let mut agents = Vec::new();
    for p in providers.iter_mut() {
        if let Some(list) = only {
            if !list.iter().any(|id| id == p.id()) {
                continue;
            }
        }
        let detect = p.detect();
        let mut entry = AgentEntry {
            available: detect.available,
            restricted: detect.restricted,
            reason: detect.reason.clone(),
            count: 0,
            bytes: 0,
            verified: crate::providers::is_verified(p.id()),
        };
        if detect.available && !detect.restricted {
            match p.discover() {
                Ok(found) => {
                    for s in found {
                        entry.count += 1;
                        entry.bytes += s.bytes;
                        sessions.push(s);
                    }
                }
                Err(e) => {
                    entry.available = false;
                    entry.reason = format!("扫描失败：{e}");
                }
            }
        }
        agents.push((p.id().to_string(), p.label(), entry));
    }
    Discovered { sessions, agents }
}

pub struct IndexReport {
    pub sessions: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pruned: usize,
    pub elapsed_ms: i64,
    pub agents: Vec<(String, String, AgentEntry)>,
}

/// 增量建索引。force=true 时忽略 mtime 全量重建。
/// 解析以文件 IO 为主；SQLite 写入串行。Rust 版解析快，暂不开线程池。
pub fn build_index(
    providers: &mut [Provider],
    index: &IndexDb,
    force: bool,
    mut on_progress: impl FnMut(&str, bool, usize),
    mut on_log: impl FnMut(&str, String),
) -> anyhow::Result<IndexReport> {
    let t0 = std::time::Instant::now();
    let Discovered { sessions, agents } = discover_all(providers);
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    // 索引库里有、这次扫描没再出现的会话 → 源已删除，清理掉。
    // 只清理本次扫描覆盖的 agent：--agent 过滤时其它 agent 的会话不归这次管
    let live: std::collections::HashSet<&str> = sessions.iter().map(|s| s.uid.as_str()).collect();
    let scanned_agents: std::collections::HashSet<&str> = providers.iter().map(|p| p.id()).collect();
    let mut pruned = 0usize;
    for uid in index.all_uids() {
        let agent = uid.split(':').next().unwrap_or("");
        if scanned_agents.contains(agent) && !live.contains(uid.as_str()) {
            index.remove_session(&uid)?;
            pruned += 1;
        }
    }
    if pruned > 0 {
        on_log("prune", format!("清理已消失的会话 {pruned} 个"));
    }

    // 按 provider 分组
    let mut by_agent: std::collections::HashMap<String, Vec<usize>> = Default::default();
    for (i, s) in sessions.iter().enumerate() {
        by_agent.entry(s.agent.clone()).or_default().push(i);
    }

    for p in providers.iter_mut() {
        let pid = p.id().to_string();
        let Some(idxs) = by_agent.get(&pid).cloned() else { continue };
        for i in idxs {
            let sid = sessions[i].session_id.clone();
            let session = &sessions[i];
            if !force && !index.needs_index(session) {
                skipped += 1;
                on_progress(&pid, true, 0);
                continue;
            }
            match p.parse(session) {
                Ok((mut messages, title, meta)) => {
                    let mut s = session.clone();
                    if s.title.is_none() {
                        s.title = title.or_else(|| derive_title(&messages));
                    }
                    if let Some(Value::Object(m)) = meta {
                        if let Value::Object(base) = &mut s.meta {
                            base.extend(m);
                        }
                    }
                    messages.sort_by_key(|m| m.seq);
                    index.put_session(&s, &messages)?;
                    indexed += 1;
                    on_progress(&pid, false, messages.len());
                }
                Err(e) => {
                    failed += 1;
                    on_log("error", format!("{pid}/{sid} 解析失败：{e}"));
                }
            }
        }
    }

    index.set_meta("indexed_at", &chrono::Utc::now().timestamp_millis().to_string())?;
    Ok(IndexReport {
        sessions: sessions.len(),
        indexed,
        skipped,
        failed,
        pruned,
        elapsed_ms: t0.elapsed().as_millis() as i64,
        agents,
    })
}

/// 在正文中定位关键词并截取上下文片段。
pub fn make_snippet(text: &str, query: &str, radius: usize) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return String::new();
    }
    let lower_t = t.to_lowercase();
    let lower_q = query.to_lowercase();
    let at = lower_t.find(&lower_q).unwrap_or(0);
    let start = at.saturating_sub(radius);
    let end = (at + query.len() + radius).min(t.len());
    // 字节下标对齐到字符边界
    let start = (start..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(start);
    let end = (start..=t.len()).rev().find(|i| t.is_char_boundary(*i)).unwrap_or(end);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&t[start..end]);
    if end < t.len() {
        out.push('…');
    }
    out
}
