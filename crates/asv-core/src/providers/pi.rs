//! Pi agent 适配器（✅ 实测）。
//!
//! `~/.pi/agent/sessions/<cwd-slug>/<ISO时间>_<uuid>.jsonl`
//! 行：session / model_change / thinking_level_change / message{role, content[], usage}。
//! usage 逐条累加出会话 token 与美元成本。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{derive_title, Message, Session};
use crate::util;

pub struct PiProvider {
    pub root: PathBuf,
}

impl Default for PiProvider {
    fn default() -> Self {
        Self {
            root: util::expand_home("~/.pi/agent/sessions"),
        }
    }
}

/// content（字符串或块数组）拍平成纯文本。
pub fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|b| {
                b.get("text")
                    .or_else(|| b.get("content"))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn num(v: Option<&Value>) -> f64 {
    v.and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn map_pi_message(obj: &Value, start_seq: i64) -> Vec<Message> {
    let mut out = Vec::new();
    let mut seq = start_seq;
    let Some(m) = obj.get("message").filter(|v| v.is_object()) else { return out };
    let ts = util::to_ms(obj.get("timestamp")).or_else(|| util::to_ms(m.get("timestamp")));
    let role_name = m.get("role").and_then(|v| v.as_str()).unwrap_or("");

    if role_name == "toolResult" || role_name == "bashExecution" {
        let text = block_text(m.get("content"));
        let mut msg = Message::new(seq, "tool", "tool_result", ts, text.clone());
        msg.tool_name = m
            .get("toolName")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| (role_name == "bashExecution").then(|| "bash".to_string()));
        msg.tool_result = Some(text);
        out.push(msg);
        return out;
    }

    let role = if role_name == "user" { "user" } else { "assistant" };
    if let Some(content) = m.get("content").and_then(|v| v.as_array()) {
        for b in content {
            let bt = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match bt {
                "text" => {
                    let text = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        out.push(Message::new(seq, role, "text", ts, text));
                        seq += 1;
                    }
                }
                "thinking" => {
                    let text = b.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        out.push(Message::new(seq, "assistant", "reasoning", ts, text));
                        seq += 1;
                    }
                }
                "toolCall" => {
                    let args = b.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                    let text = match &args {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    let mut msg = Message::new(seq, "assistant", "tool_call", ts, text);
                    msg.tool_name = b.get("name").and_then(|v| v.as_str()).map(String::from);
                    msg.tool_input = Some(args);
                    out.push(msg);
                    seq += 1;
                }
                _ => {}
            }
        }
    }
    out
}

impl PiProvider {
    pub fn id(&self) -> &'static str {
        "pi"
    }
    pub fn label(&self) -> &'static str {
        "Pi"
    }

    pub fn detect(&self) -> (bool, String, String) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()));
        }
        let n = util::walk_files(&self.root, &["jsonl"], 6).len();
        if n == 0 {
            return (false, self.root.to_string_lossy().to_string(), "未发现会话文件".into());
        }
        (true, self.root.to_string_lossy().to_string(), format!("发现 {n} 个会话文件"))
    }

    pub fn discover(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        for file in util::walk_files(&self.root, &["jsonl"], 6) {
            let Ok(st) = fs::metadata(&file) else { continue };
            if st.len() == 0 {
                continue;
            }

            let mut sid = None;
            let mut cwd = None;
            let mut first_ts = None;
            let mut model = None;
            let mut provider = None;
            let mut user_texts: Vec<String> = Vec::new();
            for o in util::read_head_objects(&file, 12, 1024 * 1024) {
                if first_ts.is_none() {
                    first_ts = util::to_ms(o.get("timestamp"));
                }
                match o.get("type").and_then(|v| v.as_str()) {
                    Some("session") => {
                        if sid.is_none() {
                            sid = o.get("id").and_then(|v| v.as_str()).map(String::from);
                        }
                        if cwd.is_none() {
                            cwd = o.get("cwd").and_then(|v| v.as_str()).map(String::from);
                        }
                    }
                    Some("model_change") => {
                        if model.is_none() {
                            model = o.get("modelId").and_then(|v| v.as_str()).map(String::from);
                        }
                        if provider.is_none() {
                            provider = o.get("provider").and_then(|v| v.as_str()).map(String::from);
                        }
                    }
                    _ => {}
                }
                if let Some(m) = o.get("message") {
                    if m.get("role").and_then(|v| v.as_str()) == Some("user") {
                        let t = block_text(m.get("content"));
                        if !t.is_empty() {
                            user_texts.push(t);
                        }
                    }
                }
            }
            let title = derive_title(
                &user_texts
                    .iter()
                    .map(|t| Message::new(0, "user", "text", None, t.clone()))
                    .collect::<Vec<_>>(),
            );

            let sid = sid.unwrap_or_else(|| {
                file.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let mtime = st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
            let slug = file
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            sessions.push(Session {
                uid: format!("pi:{sid}"),
                agent: "pi".into(),
                session_id: sid,
                project_path: cwd,
                path_source: "content".into(),
                title,
                created_at: first_ts.or(mtime),
                updated_at: mtime,
                bytes: st.len(),
                source_path: Some(file.to_string_lossy().to_string()),
                source_db: None,
                meta: serde_json::json!({ "model": model, "provider": provider, "slug": slug }),
            });
        }
        sessions
    }

    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<serde_json::Value>)> {
        let path = Path::new(session.source_path.as_deref().ok_or_else(|| anyhow::anyhow!("缺 source_path"))?);
        let mut msgs = Vec::new();
        let mut seq: i64 = 0;
        let mut tokens = serde_json::json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0, "costUsd": 0 });
        let mut model = None;
        let mut provider = None;

        util::each_line(path, |line, _| {
            let Some(obj) = util::try_parse(line) else { return true };
            match obj.get("type").and_then(|v| v.as_str()) {
                Some("model_change") => {
                    if model.is_none() {
                        model = obj.get("modelId").and_then(|v| v.as_str()).map(String::from);
                    }
                    if provider.is_none() {
                        provider = obj.get("provider").and_then(|v| v.as_str()).map(String::from);
                    }
                    return true;
                }
                Some("message") => {}
                _ => return true,
            }
            let produced = map_pi_message(&obj, seq);
            if let Some(last) = produced.last() {
                seq = last.seq + 1;
                msgs.extend(produced);
            }
            if let Some(u) = obj.pointer("/message/usage").filter(|v| v.is_object()) {
                for (k, field) in [("input", "input"), ("output", "output"), ("cacheRead", "cacheRead"), ("cacheWrite", "cacheWrite"), ("totalTokens", "total")] {
                    tokens[field] = serde_json::json!(tokens[field].as_f64().unwrap_or(0.0) + num(u.get(k)));
                }
                if let Some(c) = u.get("cost").filter(|v| v.is_object()) {
                    let add = if c.get("total").map(|v| v.is_number()).unwrap_or(false) {
                        num(c.get("total"))
                    } else {
                        num(c.get("input")) + num(c.get("output"))
                    };
                    tokens["costUsd"] = serde_json::json!(tokens["costUsd"].as_f64().unwrap_or(0.0) + add);
                }
            }
            true
        })?;

        let mut meta = serde_json::json!({ "tokens": tokens });
        if let Some(m) = model {
            meta["model"] = Value::String(m);
        }
        if let Some(p) = provider {
            meta["provider"] = Value::String(p);
        }
        Ok((msgs, None, Some(meta)))
    }

    pub fn original_paths(&self, session: &Session) -> Vec<PathBuf> {
        session.source_path.iter().map(PathBuf::from).collect()
    }
}
