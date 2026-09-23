//! OpenAI Codex CLI 适配器（✅ 实测）。
//!
//! `~/.codex/sessions/<年>/<月>/<日>/rollout-<ISO>-<uuid>.jsonl`
//! 行结构 `{ timestamp, type, payload }`。session_meta 里的 AGENTS.md 指令是噪声，不索引。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{derive_title, Message, Session};
use crate::util;

pub struct CodexProvider {
    pub root: PathBuf,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self {
            root: util::expand_home("~/.codex/sessions"),
        }
    }
}

fn payload_text(payload: &Value) -> String {
    for key in ["message", "text"] {
        if let Some(s) = payload.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

/// response_item → 0..N 条消息。
fn map_response_item(payload: &Value, ts: Option<i64>, seq_start: i64) -> Vec<Message> {
    let mut out = Vec::new();
    let mut seq = seq_start;
    let Some(t) = payload.get("type").and_then(|v| v.as_str()) else { return out };

    if t == "message" {
        let role = match payload.get("role").and_then(|v| v.as_str()) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => "system",
        };
        let mut emitted = false;
        if let Some(content) = payload.get("content").and_then(|v| v.as_array()) {
            for b in content {
                let text = ["text", "input_text", "output_text"]
                    .iter()
                    .find_map(|k| b.get(k).and_then(|v| v.as_str()))
                    .unwrap_or("");
                if !text.is_empty() {
                    out.push(Message::new(seq, role, "text", ts, text));
                    seq += 1;
                    emitted = true;
                }
            }
        }
        if !emitted && !payload.get("content").map(|v| v.is_array()).unwrap_or(false) {
            let fallback = payload_text(payload);
            if !fallback.is_empty() {
                out.push(Message::new(seq, role, "text", ts, fallback));
            }
        }
        return out;
    }

    if t == "reasoning" {
        let txt = match payload.get("summary").and_then(|v| v.as_array()) {
            Some(arr) => arr
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(|x| x.to_string())
                        .or_else(|| s.get("text").and_then(|v| v.as_str()).map(String::from))
                        .unwrap_or_default()
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            None => payload_text(payload),
        };
        if !txt.is_empty() {
            out.push(Message::new(seq, "assistant", "reasoning", ts, txt));
        }
        return out;
    }

    if matches!(t, "function_call" | "local_shell_call" | "custom_tool_call") {
        let args = payload
            .get("arguments")
            .or_else(|| payload.get("action"))
            .or_else(|| payload.get("input"))
            .cloned()
            .unwrap_or(Value::String(String::new()));
        let text = match &args {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let mut m = Message::new(seq, "assistant", "tool_call", ts, text);
        m.tool_name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| Some(t.to_string()));
        m.tool_input = Some(args);
        out.push(m);
        return out;
    }

    if matches!(t, "function_call_output" | "custom_tool_call_output") {
        let text = match payload.get("output") {
            Some(Value::String(s)) => s.clone(),
            Some(o @ Value::Object(_)) => serde_json::to_string(o).unwrap_or_default(),
            _ => String::new(),
        };
        let mut m = Message::new(seq, "tool", "tool_result", ts, text.clone());
        m.tool_name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| Some(t.to_string()));
        m.tool_result = Some(text);
        out.push(m);
    }
    out
}

impl CodexProvider {
    pub fn id(&self) -> &'static str {
        "codex"
    }
    pub fn label(&self) -> &'static str {
        "Codex CLI"
    }

    pub fn detect(&self) -> (bool, String, String) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()));
        }
        let n = util::walk_files(&self.root, &["jsonl"], 12).len();
        if n == 0 {
            return (false, self.root.to_string_lossy().to_string(), "未发现 rollout 会话文件".into());
        }
        (true, self.root.to_string_lossy().to_string(), format!("发现 {n} 个 rollout 文件"))
    }

    pub fn discover(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        for file in util::walk_files(&self.root, &["jsonl"], 12) {
            let Ok(st) = fs::metadata(&file) else { continue };
            if st.len() == 0 {
                continue;
            }

            let mut meta: Option<Value> = None;
            let mut first_ts = None;
            let mut user_texts: Vec<String> = Vec::new();
            for o in util::read_head_objects(&file, 40, 4 * 1024 * 1024) {
                if first_ts.is_none() {
                    first_ts = util::to_ms(o.get("timestamp"));
                }
                if o.get("type").and_then(|v| v.as_str()) == Some("session_meta") {
                    meta = o.get("payload").cloned();
                    continue;
                }
                let Some(p) = o.get("payload").filter(|v| v.is_object()) else { continue };
                let (otype, ptype) = (
                    o.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                    p.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                );
                // Codex 无标题字段；真实输入走 event_msg/user_message，
                // response_item/message 里多为 AGENTS.md 注入噪声
                if otype == "event_msg" && ptype == "user_message" {
                    let t = payload_text(p);
                    if !t.is_empty() {
                        user_texts.push(t);
                    }
                } else if otype == "response_item" && ptype == "message" {
                    if p.get("role").and_then(|v| v.as_str()) == Some("user") {
                        if let Some(content) = p.get("content").and_then(|v| v.as_array()) {
                            for b in content {
                                if let Some(s) = b.get("text").and_then(|v| v.as_str()) {
                                    user_texts.push(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
            let title = if user_texts.is_empty() {
                None
            } else {
                derive_title(
                    &user_texts
                        .iter()
                        .map(|t| Message::new(0, "user", "text", None, t.clone()))
                        .collect::<Vec<_>>(),
                )
            };

            let base = file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let sid = meta
                .as_ref()
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    // rollout-2026-04-27T10-10-34-<uuid>.jsonl
                    let re_suffix = base.chars().rev().take(36).collect::<String>().chars().rev().collect::<String>();
                    if re_suffix.chars().filter(|c| *c == '-').count() == 4 {
                        Some(re_suffix)
                    } else {
                        Some(base.clone())
                    }
                })
                .unwrap_or(base);

            let mtime = st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
            sessions.push(Session {
                uid: format!("codex:{sid}"),
                agent: "codex".into(),
                session_id: sid,
                project_path: meta.as_ref().and_then(|m| m.get("cwd")).and_then(|v| v.as_str()).map(String::from),
                path_source: if meta.as_ref().and_then(|m| m.get("cwd")).is_some() { "content".into() } else { "unknown".into() },
                title,
                created_at: meta
                    .as_ref()
                    .and_then(|m| util::to_ms(m.get("timestamp")))
                    .or(first_ts)
                    .or(mtime),
                updated_at: mtime,
                bytes: st.len(),
                source_path: Some(file.to_string_lossy().to_string()),
                source_db: None,
                meta: serde_json::json!({
                    "cliVersion": meta.as_ref().and_then(|m| m.get("cli_version")).and_then(|v| v.as_str()),
                    "originator": meta.as_ref().and_then(|m| m.get("originator")).and_then(|v| v.as_str()),
                }),
            });
        }
        sessions
    }

    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<serde_json::Value>)> {
        let path = Path::new(session.source_path.as_deref().ok_or_else(|| anyhow::anyhow!("缺 source_path"))?);
        let mut msgs = Vec::new();
        let mut seq: i64 = 0;
        let mut totals: Option<serde_json::Value> = None;
        util::each_line(path, |line, _| {
            let Some(obj) = util::try_parse(line) else { return true };
            let ts = util::to_ms(obj.get("timestamp"));
            let Some(payload) = obj.get("payload").filter(|v| v.is_object()) else { return true };

            if obj.get("type").and_then(|v| v.as_str()) == Some("event_msg")
                && payload.get("type").and_then(|v| v.as_str()) == Some("token_count")
            {
                if let Some(t) = payload.pointer("/info/total_token_usage") {
                    totals = Some(t.clone());
                }
                return true;
            }

            let otype = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let produced = match otype {
                "response_item" => map_response_item(payload, ts, seq),
                "event_msg" => {
                    let ptype = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if matches!(ptype, "user_message" | "agent_message") {
                        let text = payload_text(payload);
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![Message::new(
                                seq,
                                if ptype == "user_message" { "user" } else { "assistant" },
                                "text",
                                ts,
                                text,
                            )]
                        }
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };
            if let Some(last) = produced.last() {
                seq = last.seq + 1;
                msgs.extend(produced);
            }
            true
        })?;
        let meta = totals.map(|t| serde_json::json!({ "tokens": t }));
        Ok((msgs, None, meta))
    }

    pub fn original_paths(&self, session: &Session) -> Vec<PathBuf> {
        session.source_path.iter().map(PathBuf::from).collect()
    }
}
