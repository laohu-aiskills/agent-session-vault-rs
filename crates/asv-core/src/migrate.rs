//! 跨 Agent 迁移。
//!
//! ⚠ 能力边界（与 Node 版一致，必须如实告知）：
//! 各家工具调用 schema、推理块格式、会话索引库互不兼容，
//! 迁移【只能保住用户消息与助手文本】，工具调用/结果/推理必然丢失。
//! 且不写目标 Agent 的索引库（如 workbuddy.db），产出「格式文件 + 落位说明 + 丢失清单」。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::model::{Message, Session};

#[derive(Clone, Copy)]
pub struct Target {
    pub id: &'static str,
    pub label: &'static str,
    pub dialect: Dialect,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Dialect {
    Codebuddy,
    Claude,
}

pub const TARGETS: [Target; 4] = [
    Target { id: "workbuddy", label: "WorkBuddy", dialect: Dialect::Codebuddy },
    Target { id: "claude-code", label: "Claude Code", dialect: Dialect::Claude },
    Target { id: "qwenworkcn", label: "Qwen Work", dialect: Dialect::Claude },
    Target { id: "codebuddy", label: "CodeBuddy", dialect: Dialect::Codebuddy },
];

pub fn find_target(id: &str) -> Option<Target> {
    TARGETS.iter().copied().find(|t| t.id == id)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

impl Target {
    pub fn project_root(&self) -> PathBuf {
        match self.id {
            "workbuddy" => home().join(".workbuddy").join("projects"),
            "claude-code" => home().join(".claude").join("projects"),
            "qwenworkcn" => home().join(".qwenworkcn").join("projects"),
            "codebuddy" => home().join(".codebuddy").join("projects"),
            _ => home().clone(),
        }
    }
}

/// 项目路径 → 目标 Agent 的分片目录名。
/// 两种实测编码：codebuddy 系 `c-Users-me-x`；claude 系 `C--Users-me-x`。
pub fn slug_for(project_path: &str, dialect: Dialect) -> String {
    let norm = std::path::absolute(project_path).unwrap_or_else(|_| PathBuf::from(project_path));
    let norm = norm.to_string_lossy().to_string();
    let bytes = norm.as_bytes();
    if norm.len() >= 2 && bytes[1] == b':' {
        let rest = norm[2..].replace(['\\', '/'], "-").trim_start_matches('-').to_string();
        return if dialect == Dialect::Claude {
            format!("{}--{}", norm[..1].to_uppercase(), rest)
        } else {
            format!("{}-{}", norm[..1].to_lowercase(), rest)
        };
    }
    let rest = norm.trim_start_matches(['\\', '/']).replace(['\\', '/'], "-");
    format!("-{rest}")
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// codebuddy（WorkBuddy / CodeBuddy）方言的一行。
fn line_codebuddy(m: &Message, sid: &str, cwd: &str, parent_id: Option<&str>) -> Value {
    let id = Uuid::new_v4().to_string();
    let mut base = json!({
        "id": id,
        "parentId": parent_id,
        "timestamp": m.ts.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
        "sessionId": sid,
        "cwd": cwd,
    });
    match m.kind.as_str() {
        "text" => {
            base["type"] = json!("message");
            base["role"] = json!(if m.role == "user" { "user" } else { "assistant" });
            base["status"] = json!("completed");
            base["content"] = json!([{
                "type": if m.role == "user" { "input_text" } else { "output_text" },
                "text": m.text,
            }]);
        }
        "reasoning" => {
            base["type"] = json!("reasoning");
            base["content"] = json!([]);
            base["rawContent"] = json!([{ "type": "reasoning_text", "text": m.text }]);
        }
        "tool_call" => {
            base["type"] = json!("function_call");
            base["name"] = json!(m.tool_name.clone().unwrap_or_else(|| "unknown".into()));
            base["callId"] = json!(format!("call_{}", &id[..12]));
            base["arguments"] = json!(m.text);
        }
        "tool_result" => {
            base["type"] = json!("function_call_result");
            base["name"] = json!(m.tool_name.clone().unwrap_or_else(|| "unknown".into()));
            base["callId"] = json!(format!("call_{}", &id[..12]));
            base["status"] = json!("completed");
            base["output"] = json!({ "type": "text", "text": m.text });
        }
        _ => return Value::Null,
    }
    base
}

/// claude（Claude Code / Qwen Work）方言的一行。
fn line_claude(m: &Message, sid: &str, cwd: &str) -> Value {
    let uuid = Uuid::new_v4().to_string();
    let mut base = json!({
        "uuid": uuid,
        "timestamp": iso(m.ts.unwrap_or_else(|| chrono::Utc::now().timestamp_millis())),
        "sessionId": sid,
        "cwd": cwd,
        "version": "asv-import",
    });
    match m.kind.as_str() {
        "text" => {
            base["type"] = json!(if m.role == "user" { "user" } else { "assistant" });
            base["message"] = json!({
                "role": if m.role == "user" { "user" } else { "assistant" },
                "content": [{ "type": "text", "text": m.text }],
            });
        }
        "reasoning" => {
            base["type"] = json!("assistant");
            base["message"] = json!({
                "role": "assistant",
                "content": [{ "type": "thinking", "thinking": m.text, "signature": "" }],
            });
        }
        "tool_call" => {
            base["type"] = json!("assistant");
            let u = base["uuid"].as_str().unwrap_or("").to_string();
            base["message"] = json!({
                "role": "assistant",
                "content": [{ "type": "tool_use", "id": format!("toolu_{}", &u[..20.min(u.len())]), "name": m.tool_name.clone().unwrap_or_else(|| "unknown".into()), "input": m.tool_input.clone().unwrap_or(json!({})) }],
            });
        }
        "tool_result" => {
            base["type"] = json!("user");
            base["message"] = json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": null, "content": m.text }],
            });
        }
        _ => return Value::Null,
    }
    base
}

/// 统计迁移中会丢失的内容。
pub fn loss_report(messages: &[Message]) -> Value {
    let mut lost = json!({ "tool_call": 0, "tool_result": 0, "reasoning": 0, "other": 0 });
    let mut kept = 0i64;
    for m in messages {
        match m.kind.as_str() {
            "text" => kept += 1,
            k if k == "tool_call" || k == "tool_result" || k == "reasoning" => {
                lost[k] = json!(lost[k].as_i64().unwrap_or(0) + 1);
            }
            _ => {
                lost["other"] = json!(lost["other"].as_i64().unwrap_or(0) + 1);
            }
        }
    }
    json!({ "keptMessages": kept, "lostBreakdown": lost })
}

pub struct MigrateReport {
    pub file: PathBuf,
    pub note_file: PathBuf,
    pub dir: PathBuf,
    pub slug: String,
    pub new_session_id: String,
    pub wrote_messages: usize,
    pub report: Value,
    pub target: Target,
}

/// 执行迁移：产出目标格式文件 + 落位说明。
/// sanitize=true 只保留正文（目标格式承载不了工具调用/推理）。
pub fn migrate(session: &Session, messages: &[Message], to: &str, out: &Path, sanitize: bool) -> anyhow::Result<MigrateReport> {
    let target = find_target(to)
        .ok_or_else(|| anyhow::anyhow!("不支持迁移到该 Agent：{to}（当前支持：{}）", TARGETS.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")))?;

    let report = loss_report(messages);
    let sid = Uuid::new_v4().to_string();
    let cwd = session
        .project_path
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().to_string_lossy().to_string());
    let slug = slug_for(&cwd, target.dialect);
    let dest_dir = out.join(&slug);
    fs::create_dir_all(&dest_dir)?;
    let dest_file = dest_dir.join(format!("{sid}.jsonl"));

    let usable: Vec<&Message> = if sanitize {
        messages
            .iter()
            .filter(|m| m.kind == "text" && !m.text.trim().is_empty())
            .collect()
    } else {
        messages.iter().collect()
    };

    let mut lines: Vec<String> = Vec::new();
    if target.dialect == Dialect::Claude {
        lines.push(json!({ "type": "mode", "mode": "normal", "sessionId": sid }).to_string());
    }
    let mut parent_id: Option<String> = None;
    for m in &usable {
        let obj = if target.dialect == Dialect::Claude {
            line_claude(m, &sid, &cwd)
        } else {
            line_codebuddy(m, &sid, &cwd, parent_id.as_deref())
        };
        if obj.is_null() {
            continue;
        }
        parent_id = obj
            .get("id")
            .or_else(|| obj.get("uuid"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or(parent_id);
        lines.push(obj.to_string());
    }

    let title_text = session.title.clone().unwrap_or_else(|| "（无标题）".into());
    let title_line = if target.dialect == Dialect::Claude {
        json!({ "type": "ai-title", "aiTitle": title_text, "sessionId": sid })
    } else {
        json!({ "type": "ai-title", "timestamp": chrono::Utc::now().timestamp_millis(), "aiTitle": title_text, "sessionId": sid, "cwd": cwd })
    };
    lines.push(title_line.to_string());

    // 首部插入迁移标记，便于日后识别「这条是导入的」
    let marker = if target.dialect == Dialect::Claude {
        json!({
            "type": "system", "subtype": "asv_migration",
            "uuid": Uuid::new_v4().to_string(),
            "timestamp": iso(chrono::Utc::now().timestamp_millis()),
            "sessionId": sid, "cwd": cwd, "version": "asv-import",
            "note": format!("imported by agent-session-vault from {}:{}", session.agent, session.session_id),
        })
    } else {
        json!({
            "id": Uuid::new_v4().to_string(),
            "timestamp": chrono::Utc::now().timestamp_millis(),
            "type": "asv-migration",
            "sessionId": sid, "cwd": cwd,
            "note": format!("imported by agent-session-vault from {}:{}", session.agent, session.session_id),
        })
    };
    let at = if target.dialect == Dialect::Claude { 1 } else { 0 };
    lines.insert(at, marker.to_string());

    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(&dest_file, content)?;

    let note_file = dest_dir.join(format!("{sid}.asv-migration.json"));
    let note = json!({
        "format": "agent-session-vault/migration-note",
        "createdAt": chrono::Utc::now().timestamp_millis(),
        "source": { "agent": session.agent, "sessionId": session.session_id, "title": session.title },
        "target": { "agent": target.id, "label": target.label, "dialect": format!("{:?}", target.dialect).to_lowercase() },
        "outputFile": dest_file,
        "newSessionId": sid,
        "title": title_text,
        "keptMessages": usable.len(),
        "lost": report["lostBreakdown"],
        "instructions": [
            format!("把 {} 整个目录复制到目标 Agent 的项目根目录：", dest_dir.display()),
            format!("  {}", target.project_root().display()),
            "即最终路径为：".to_string(),
            format!("  {}", target.project_root().join(&slug).join(format!("{sid}.jsonl")).display()),
            "注意：本工具不会替目标 Agent 更新它的会话索引库（如 workbuddy.db），".to_string(),
            "      因此目标 Agent 的会话列表中可能不会立即出现这条会话。".to_string(),
            "      请先用目标 Agent 打开一次该目录，或在其设置中触发一次重建索引。".to_string(),
        ],
        "warning": [
            "跨 Agent 迁移只能保住用户消息与助手文本。".to_string(),
            "工具调用、工具结果、推理过程因格式不兼容已被剔除，无法恢复。".to_string(),
            "若需要完整内容，请使用 export 命令导出 Markdown/HTML，或保留原始备份。".to_string(),
        ],
    });
    fs::write(&note_file, serde_json::to_string_pretty(&note)?)?;

    Ok(MigrateReport {
        file: dest_file,
        note_file,
        dir: dest_dir,
        slug,
        new_session_id: sid,
        wrote_messages: usable.len(),
        report,
        target,
    })
}
