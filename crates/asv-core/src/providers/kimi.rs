//! Kimi CLI（协议 1.1）与 Kimi Code（协议 1.3）适配器（✅ 实测）。
//! 两家同名不同协议，不能共用解析器。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{derive_title, Message, Session};
use crate::util;

/// 通用兜底提取：未识别类型里只要有文本就拿出来，避免漏内容。
fn extract_text(payload: Option<&Value>) -> String {
    let Some(p) = payload else { return String::new() };
    if let Some(arr) = p.get("user_input").and_then(|v| v.as_array()) {
        let s = arr
            .iter()
            .map(|b| {
                b.get("text")
                    .or_else(|| b.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !s.is_empty() {
            return s;
        }
    }
    for key in ["text", "content", "message", "output"] {
        match p.get(key) {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Array(arr)) => {
                let s = arr
                    .iter()
                    .map(|b| {
                        b.get("text")
                            .or_else(|| b.get("content"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !s.is_empty() {
                    return s;
                }
            }
            _ => {}
        }
    }
    String::new()
}

/* ---------------- Kimi CLI（协议 1.1） ---------------- */

pub struct KimiCliProvider {
    pub root: PathBuf,
}

impl Default for KimiCliProvider {
    fn default() -> Self {
        Self {
            root: util::expand_home("~/.kimi/sessions"),
        }
    }
}

impl KimiCliProvider {
    pub fn detect(&self) -> (bool, String, String) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()));
        }
        let n = util::walk_files(&self.root, &["wire.jsonl"], 8).len();
        if n == 0 {
            return (false, self.root.to_string_lossy().to_string(), "未发现 wire.jsonl 会话文件".into());
        }
        (true, self.root.to_string_lossy().to_string(), format!("发现 {n} 个会话"))
    }

    pub fn discover(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        for ws_dir in util::list_dirs(&self.root) {
            for sess_dir in util::list_dirs(&ws_dir) {
                // 有的会话只有空的 context.jsonl（打开即中断），也如实登记
                let wire = sess_dir.join("wire.jsonl");
                let ctx = sess_dir.join("context.jsonl");
                let target = if wire.exists() { wire } else if ctx.exists() { ctx } else { continue };
                let Ok(st) = fs::metadata(&target) else { continue };

                let mut first_ts = None;
                let mut cwd = None;
                let mut user_texts: Vec<String> = Vec::new();
                for o in util::read_head_objects(&target, 12, 1024 * 1024) {
                    if first_ts.is_none() {
                        first_ts = util::to_ms(o.get("timestamp"));
                    }
                    let m = o.get("message");
                    if let Some(p) = m.and_then(|m| m.get("payload")) {
                        if cwd.is_none() {
                            if let Some(c) = p.get("cwd").and_then(|v| v.as_str()) {
                                cwd = Some(c.to_string());
                            }
                        }
                    }
                    if m.and_then(|m| m.get("type")).and_then(|v| v.as_str()) == Some("TurnBegin") {
                        let t = extract_text(m.and_then(|m| m.get("payload")));
                        if !t.is_empty() {
                            user_texts.push(t);
                        }
                    }
                }
                let title = derive_title(
                    &user_texts
                        .iter()
                        .map(|t| Message::new(0, "user", "text", None, t.clone()))
                        .collect::<Vec<_>>(),
                );
                let sid = sess_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mtime = st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);

                sessions.push(Session {
                    uid: format!("kimi-cli:{sid}"),
                    agent: "kimi-cli".into(),
                    session_id: sid,
                    project_path: cwd,
                    path_source: "content".into(),
                    title,
                    created_at: first_ts.or(mtime),
                    updated_at: mtime,
                    bytes: st.len(),
                    source_path: Some(target.to_string_lossy().to_string()),
                    source_db: None,
                    meta: serde_json::json!({
                        "workspace": ws_dir.file_name().map(|s| s.to_string_lossy().to_string()),
                        "empty": st.len() == 0,
                    }),
                });
            }
        }
        sessions
    }

    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<serde_json::Value>)> {
        let path = Path::new(session.source_path.as_deref().ok_or_else(|| anyhow::anyhow!("缺 source_path"))?);
        let mut msgs = Vec::new();
        let mut seq: i64 = 0;
        util::each_line(path, |line, _| {
            let Some(obj) = util::try_parse(line) else { return true };
            let Some(m) = obj.get("message").filter(|v| v.is_object()) else { return true };
            let ts = util::to_ms(obj.get("timestamp")).or_else(|| util::to_ms(m.get("timestamp")));
            let t = m.get("type").and_then(|v| v.as_str()).unwrap_or("");

            let text = if t == "TurnBegin" {
                extract_text(m.get("payload"))
            } else if matches!(t, "StepBegin" | "StepInterrupted" | "TurnEnd") {
                String::new()
            } else {
                // 通用兜底：未识别类型只要有文本就当助手输出
                extract_text(m.get("payload"))
            };
            if !text.is_empty() {
                let role = if t == "TurnBegin" { "user" } else { "assistant" };
                msgs.push(Message::new(seq, role, "text", ts, text));
                seq += 1;
            }
            true
        })?;
        Ok((msgs, None, None))
    }

    pub fn original_paths(&self, session: &Session) -> Vec<PathBuf> {
        session
            .source_path
            .as_ref()
            .and_then(|p| Path::new(p).parent())
            .map(|d| vec![d.to_path_buf()])
            .unwrap_or_default()
    }
}

/* ---------------- Kimi Code（协议 1.3） ---------------- */

pub struct KimiCodeProvider {
    pub root: PathBuf,
}

impl Default for KimiCodeProvider {
    fn default() -> Self {
        Self {
            root: util::expand_home("~/.kimi-code/sessions"),
        }
    }
}

impl KimiCodeProvider {
    pub fn detect(&self) -> (bool, String, String) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()));
        }
        let n = util::walk_files(&self.root, &["wire.jsonl"], 8).len();
        if n == 0 {
            return (false, self.root.to_string_lossy().to_string(), "未发现 wire.jsonl 会话文件".into());
        }
        (true, self.root.to_string_lossy().to_string(), format!("发现 {n} 个会话"))
    }

    pub fn discover(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        for wd_dir in util::list_dirs(&self.root) {
            for sess_dir in util::list_dirs(&wd_dir) {
                let wire = sess_dir.join("agents").join("main").join("wire.jsonl");
                let Ok(st) = fs::metadata(&wire) else { continue };

                let mut first_ts = None;
                let mut model = None;
                let mut cwd = None;
                let mut user_texts: Vec<String> = Vec::new();
                for o in util::read_head_objects(&wire, 20, 2 * 1024 * 1024) {
                    if first_ts.is_none() {
                        first_ts = util::to_ms(o.get("created_at")).or_else(|| util::to_ms(o.get("time")));
                    }
                    if o.get("type").and_then(|v| v.as_str()) == Some("config.update") {
                        if model.is_none() {
                            model = o.get("modelAlias").and_then(|v| v.as_str()).map(String::from);
                        }
                    }
                    if o.get("type").and_then(|v| v.as_str()) == Some("turn.prompt") {
                        if let Some(input) = o.get("input").and_then(|v| v.as_array()) {
                            let t = input
                                .iter()
                                .map(|b| b.get("text").and_then(|v| v.as_str()).unwrap_or(""))
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !t.is_empty() {
                                user_texts.push(t);
                            }
                        }
                    }
                    if cwd.is_none() {
                        if let Some(c) = o.pointer("/message/payload/cwd").and_then(|v| v.as_str()) {
                            cwd = Some(c.to_string());
                        }
                    }
                }
                let title = derive_title(
                    &user_texts
                        .iter()
                        .map(|t| Message::new(0, "user", "text", None, t.clone()))
                        .collect::<Vec<_>>(),
                );
                // state.json 里通常带工作目录，优先采信
                let state_file = sess_dir.join("state.json");
                if let Ok(text) = fs::read_to_string(&state_file) {
                    if let Ok(stj) = serde_json::from_str::<Value>(&text) {
                        let cand = stj
                            .get("cwd")
                            .or_else(|| stj.get("workspace"))
                            .or_else(|| stj.get("directory"))
                            .and_then(|v| v.as_str());
                        if let Some(c) = cand {
                            if !c.is_empty() {
                                cwd = Some(c.to_string());
                            }
                        }
                        if model.is_none() {
                            model = stj.get("model").and_then(|v| v.as_str()).map(String::from);
                        }
                    }
                }
                let sid = sess_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mtime = st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);

                sessions.push(Session {
                    uid: format!("kimi-code:{sid}"),
                    agent: "kimi-code".into(),
                    session_id: sid,
                    project_path: cwd,
                    path_source: "content".into(),
                    title,
                    created_at: first_ts.or(mtime),
                    updated_at: mtime,
                    bytes: st.len(),
                    source_path: Some(wire.to_string_lossy().to_string()),
                    source_db: None,
                    meta: serde_json::json!({
                        "workspace": wd_dir.file_name().map(|s| s.to_string_lossy().to_string()),
                        "model": model,
                    }),
                });
            }
        }
        sessions
    }

    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<serde_json::Value>)> {
        let path = Path::new(session.source_path.as_deref().ok_or_else(|| anyhow::anyhow!("缺 source_path"))?);
        let mut msgs = Vec::new();
        let mut seq: i64 = 0;
        util::each_line(path, |line, _| {
            let Some(obj) = util::try_parse(line) else { return true };
            let ts = util::to_ms(obj.get("time")).or_else(|| util::to_ms(obj.get("created_at")));
            let mut push = |role: &str, kind: &str, text: String, tool_name: Option<String>, tool_input: Option<Value>, tool_result: Option<String>| {
                let mut m = Message::new(seq, role, kind, ts, text);
                m.tool_name = tool_name;
                m.tool_input = tool_input;
                m.tool_result = tool_result;
                msgs.push(m);
                seq += 1;
            };

            match obj.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "turn.prompt" => {
                    let text = obj
                        .get("input")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .map(|b| b.get("text").and_then(|v| v.as_str()).unwrap_or(""))
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    if !text.is_empty() {
                        push("user", "text", text, None, None, None);
                    }
                }
                "context.append_message" => {
                    let m = obj.get("message").cloned().unwrap_or(serde_json::json!({}));
                    let role = if m.get("role").and_then(|v| v.as_str()) == Some("user") { "user" } else { "assistant" };
                    if let Some(content) = m.get("content").and_then(|v| v.as_array()) {
                        for b in content {
                            if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                                    if !t.is_empty() {
                                        push(role, "text", t.to_string(), None, None, None);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(calls) = m.get("toolCalls").and_then(|v| v.as_array()) {
                        for tc in calls {
                            let args = tc
                                .pointer("/function/arguments")
                                .cloned()
                                .or_else(|| tc.get("args").cloned());
                            let text = match &args {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => serde_json::to_string(other).unwrap_or_default(),
                                None => String::new(),
                            };
                            let name = tc
                                .get("name")
                                .and_then(|v| v.as_str())
                                .or_else(|| tc.pointer("/function/name").and_then(|v| v.as_str()))
                                .map(String::from);
                            push("assistant", "tool_call", text, name, args, None);
                        }
                    }
                }
                "context.append_loop_event" => {
                    let ev = obj.get("event").cloned().unwrap_or(serde_json::json!({}));
                    match ev.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                        "content.part" => {
                            let part = ev.get("part").cloned().unwrap_or(serde_json::json!({}));
                            match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                                "think" => {
                                    if let Some(t) = part.get("think").and_then(|v| v.as_str()) {
                                        if !t.is_empty() {
                                            push("assistant", "reasoning", t.to_string(), None, None, None);
                                        }
                                    }
                                }
                                "text" => {
                                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                        if !t.is_empty() {
                                            push("assistant", "text", t.to_string(), None, None, None);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        "tool.call" => {
                            let args = ev.get("args").cloned();
                            let text = match &args {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => serde_json::to_string(other).unwrap_or_default(),
                                None => String::new(),
                            };
                            push(
                                "assistant",
                                "tool_call",
                                text,
                                ev.get("name").and_then(|v| v.as_str()).map(String::from),
                                args,
                                None,
                            );
                        }
                        "tool.result" => {
                            let o = ev.pointer("/result/output");
                            let text = match o {
                                Some(Value::String(s)) => s.clone(),
                                Some(other @ Value::Object(_)) => serde_json::to_string(other).unwrap_or_default(),
                                _ => String::new(),
                            };
                            if !text.is_empty() {
                                push("tool", "tool_result", text.clone(), None, None, Some(text));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {} // metadata / config.update / usage.record / tools.* 不进正文
            }
            true
        })?;
        Ok((msgs, None, None))
    }

    pub fn original_paths(&self, session: &Session) -> Vec<PathBuf> {
        session
            .source_path
            .as_ref()
            .and_then(|p| Path::new(p).ancestors().nth(3))
            .map(|d| vec![d.to_path_buf()])
            .unwrap_or_default()
    }
}
