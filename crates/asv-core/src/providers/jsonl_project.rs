//! jsonl-project 家族适配器：WorkBuddy / Claude Code / QwenWorkCN / CodeBuddy。
//!
//! 布局：`<root>/<项目slug>/<会话id>.jsonl`（Claude 另有 `<会话id>/subagents/*.jsonl`）。
//! 两种方言按形状识别：codebuddy 顶层 content[]/rawContent[]；claude 系 message.content[]。

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::title_index;
use crate::model::{derive_title, Message, Session};
use crate::util;

/// 行类型 → (role, kind)。未识别返回 None。
fn classify(obj: &Value) -> Option<(String, String)> {
    let t = obj.get("type")?.as_str()?;
    match t {
        "message" => Some((
            if obj.get("role").and_then(|r| r.as_str()) == Some("user") { "user" } else { "assistant" }.into(),
            "text".into(),
        )),
        "reasoning" => Some(("assistant".into(), "reasoning".into())),
        "function_call" => Some(("assistant".into(), "tool_call".into())),
        "function_call_result" => Some(("tool".into(), "tool_result".into())),
        "user" => Some(("user".into(), "text".into())),
        "assistant" => Some(("assistant".into(), "text".into())),
        "system" => Some(("system".into(), "meta".into())),
        _ => None,
    }
}

/// 收集一条记录里所有可能承载内容的块（三种嵌套位置）。
fn extract_blocks(obj: &Value) -> Vec<&Value> {
    let mut blocks = Vec::new();
    for key in ["content", "rawContent"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            blocks.extend(arr.iter().filter(|b| b.is_object()));
        }
    }
    if let Some(arr) = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_array())
    {
        blocks.extend(arr.iter().filter(|b| b.is_object()));
    }
    blocks
}

fn as_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 一条原始记录 → 0..N 条统一消息。
pub fn map_line(obj: &Value, start_seq: i64) -> Vec<Message> {
    let mut out = Vec::new();
    let Some((role, kind)) = classify(obj) else { return out };
    let ts = util::to_ms(obj.get("timestamp"));
    let mut seq = start_seq;

    if kind == "tool_call" {
        let args = obj.get("arguments").cloned().unwrap_or(Value::String(String::new()));
        let text = match &args {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let mut m = Message::new(seq, "assistant", "tool_call", ts, text);
        m.tool_name = obj.get("name").and_then(|v| v.as_str()).map(String::from);
        m.tool_input = Some(args);
        out.push(m);
        return out;
    }
    if kind == "tool_result" {
        let text = match obj.get("output") {
            Some(Value::Object(o)) => o
                .get("text")
                .map(as_string)
                .unwrap_or_else(|| serde_json::to_string(obj.get("output").unwrap()).unwrap_or_default()),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let mut m = Message::new(seq, "tool", "tool_result", ts, text.clone());
        m.tool_name = obj.get("name").and_then(|v| v.as_str()).map(String::from);
        m.tool_result = Some(text);
        out.push(m);
        return out;
    }

    let blocks = extract_blocks(obj);
    if blocks.is_empty() {
        // Claude 系 user 消息 content 可能是纯字符串
        if let Some(s) = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
        {
            out.push(Message::new(seq, &role, &kind, ts, s));
        }
        return out;
    }

    for b in blocks {
        let bt = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match bt {
            "input_text" | "output_text" | "text" => {
                let text = b.get("text").map(as_string).unwrap_or_default();
                out.push(Message::new(seq, &role, "text", ts, text));
            }
            "thinking" | "reasoning_text" | "reasoning" => {
                let text = b
                    .get("thinking")
                    .or_else(|| b.get("text"))
                    .map(as_string)
                    .unwrap_or_default();
                out.push(Message::new(seq, "assistant", "reasoning", ts, text));
            }
            "tool_use" => {
                let input = b.get("input").cloned().unwrap_or_else(|| serde_json::json!({}));
                let text = match &input {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                let mut m = Message::new(seq, "assistant", "tool_call", ts, text);
                m.tool_name = b.get("name").and_then(|v| v.as_str()).map(String::from);
                m.tool_input = Some(input);
                out.push(m);
            }
            "tool_result" => {
                let text = match b.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(arr)) => arr
                        .iter()
                        .map(|x| {
                            x.get("text")
                                .or_else(|| x.get("content"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        })
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Some(other @ Value::Object(_)) => serde_json::to_string(other).unwrap_or_default(),
                    _ => String::new(),
                };
                let mut m = Message::new(seq, "tool", "tool_result", ts, text.clone());
                m.tool_name = b.get("name").and_then(|v| v.as_str()).map(String::from);
                m.tool_result = Some(text);
                out.push(m);
            }
            _ => {} // image 等不进索引
        }
        seq += 1;
    }
    out
}

/// discover 阶段的元数据累积器。
#[derive(Default)]
struct Acc {
    cwd: Option<String>,
    session_id: Option<String>,
    ts: Option<i64>,
    title: Option<String>,
    last_prompt: Option<String>,
    project_path: Option<String>,
    path_source: String,
    model: Option<String>,
    first_user_text: Option<String>,
}

fn harvest(obj: &Value, acc: &mut Acc) {
    if acc.cwd.is_none() {
        if let Some(c) = obj.get("cwd").and_then(|v| v.as_str()) {
            if !c.is_empty() {
                acc.cwd = Some(c.to_string());
            }
        }
    }
    if acc.session_id.is_none() {
        if let Some(s) = obj
            .get("sessionId")
            .or_else(|| obj.get("session_id"))
            .and_then(|v| v.as_str())
        {
            acc.session_id = Some(s.to_string());
        }
    }
    if acc.ts.is_none() {
        acc.ts = util::to_ms(obj.get("timestamp"));
    }
    if acc.model.is_none() {
        if let Some(m) = obj
            .get("model")
            .and_then(|v| v.as_str())
            .or_else(|| obj.pointer("/message/model").and_then(|v| v.as_str()))
            .or_else(|| obj.pointer("/providerData/model").and_then(|v| v.as_str()))
        {
            acc.model = Some(m.to_string());
        }
    }
    if acc.first_user_text.is_none() {
        if let Some((role, _)) = classify(obj) {
            if role == "user" {
                let blocks = extract_blocks(obj);
                if let Some(t) = blocks
                    .iter()
                    .find(|b| {
                        matches!(
                            b.get("type").and_then(|v| v.as_str()),
                            Some("input_text") | Some("text")
                        )
                    })
                    .and_then(|b| b.get("text"))
                    .and_then(|v| v.as_str())
                {
                    if !t.is_empty() {
                        acc.first_user_text = Some(t.to_string());
                    }
                } else if let Some(s) = obj
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|v| v.as_str())
                {
                    if !s.is_empty() {
                        acc.first_user_text = Some(s.to_string());
                    }
                }
            }
        }
    }
    match obj.get("type").and_then(|v| v.as_str()) {
        Some("ai-title") => {
            if let Some(t) = obj.get("aiTitle").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    acc.title = Some(t.to_string());
                }
            }
        }
        Some("last-prompt") => {
            if acc.last_prompt.is_none() {
                if let Some(p) = obj.get("lastPrompt").and_then(|v| v.as_str()) {
                    acc.last_prompt = Some(p.to_string());
                }
            }
        }
        Some("workspace-directories") => {
            if acc.project_path.is_none() {
                if let Some(d) = obj
                    .get("directories")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                {
                    if !d.is_empty() {
                        acc.project_path = Some(d.to_string());
                        acc.path_source = "content".into();
                    }
                }
            }
        }
        _ => {}
    }
}

/// 外部标题索引配置（WorkBuddy 的标题在 workbuddy.db 的 sessions 表里）。
#[derive(Clone)]
pub struct TitleIndexCfg {
    pub db: PathBuf,
    pub table: String,
    pub id_col: String,
    pub title_cols: Vec<String>,
    pub model_col: Option<String>,
}

pub struct JsonlProjectProvider {
    pub id: String,
    pub label: String,
    pub root: PathBuf,
    pub subagents: bool,
    pub title_index: Option<TitleIndexCfg>,
}

impl JsonlProjectProvider {
    pub fn new(id: &str, label: &str, root: &str, subagents: bool) -> Self {
        JsonlProjectProvider {
            id: id.into(),
            label: label.into(),
            root: util::expand_home(root),
            subagents,
            title_index: None,
        }
    }

    pub fn with_title_index(mut self, cfg: TitleIndexCfg) -> Self {
        self.title_index = Some(cfg);
        self
    }

    fn titles(&self) -> std::collections::HashMap<String, (Option<String>, Option<String>)> {
        match &self.title_index {
            Some(cfg) => title_index::read_sqlite_index(cfg),
            None => Default::default(),
        }
    }

    /// 收集待扫描的 (文件, slug, 原生会话id)。
    fn files(&self) -> Vec<(PathBuf, String, String)> {
        let mut out = Vec::new();
        for proj_dir in util::list_dirs(&self.root) {
            let slug = proj_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(entries) = fs::read_dir(&proj_dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() && p.extension().map(|x| x == "jsonl").unwrap_or(false) {
                    let sid = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    out.push((p, slug.clone(), sid));
                } else if p.is_dir() && self.subagents {
                    let sid = p
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let sub = p.join("subagents");
                    if sub.is_dir() {
                        for f in util::walk_files(&sub, &["jsonl".into()], 4) {
                            let base = f
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            out.push((f, slug.clone(), format!("{}:{}", sid, base)));
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }

    pub fn detect(&self) -> (bool, String, String) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()));
        }
        let n = fs::read_dir(&self.root).map(|d| d.count()).unwrap_or(0);
        if n == 0 {
            return (false, self.root.to_string_lossy().to_string(), "数据目录为空（该 Agent 未产生会话）".into());
        }
        (true, self.root.to_string_lossy().to_string(), format!("发现 {n} 个项目目录"))
    }

    pub fn discover(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        let titles = self.titles();
        for (file, slug, sid) in self.files() {
            let Ok(st) = fs::metadata(&file) else { continue };
            if st.len() == 0 {
                continue;
            }
            let mut acc = Acc {
                session_id: Some(sid.clone()),
                path_source: "unknown".into(),
                ..Default::default()
            };
            for o in util::read_head_objects(&file, 40, 4 * 1024 * 1024) {
                harvest(&o, &mut acc);
            }
            if acc.title.is_none() {
                for o in util::read_tail_objects(&file, 8, 256 * 1024) {
                    harvest(&o, &mut acc);
                    if acc.title.is_some() {
                        break;
                    }
                }
            }

            let (mut project_path, mut path_source) = match (&acc.cwd, &acc.project_path) {
                (Some(c), _) => (Some(c.clone()), "content".to_string()),
                (None, Some(p)) => (Some(p.clone()), acc.path_source.clone()),
                _ => (None, "unknown".to_string()),
            };
            if project_path.is_none() {
                project_path = util::decode_slug(&slug);
                path_source = "slug".into();
            }

            // 标题优先级：外部索引 > 正文 ai-title > 末次输入 > 首条用户消息派生
            let idx = titles.get(&sid);
            let mut title = idx.as_ref().and_then(|(t, _)| t.clone()).or(acc.title.clone());
            if title.is_none() {
                if let Some(lp) = &acc.last_prompt {
                    title = Some(lp.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect());
                }
            }
            if title.is_none() {
                if let Some(ft) = &acc.first_user_text {
                    let m = [crate::model::Message::new(0, "user", "text", None, ft.clone())];
                    title = derive_title(&m);
                }
            }

            let mut meta = serde_json::json!({ "slug": slug });
            if let Some(m) = &acc.model {
                meta["model"] = Value::String(m.clone());
            }
            if let Some((_, im)) = idx {
                if let Some(m) = im {
                    if meta.get("model").map(|v| v.is_null()).unwrap_or(true) {
                        meta["model"] = Value::String(m.clone());
                    }
                }
            }

            sessions.push(Session {
                uid: format!("{}:{}", self.id, sid),
                agent: self.id.clone(),
                session_id: sid,
                project_path,
                path_source,
                title,
                created_at: acc.ts,
                updated_at: Some(st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)).flatten(),
                bytes: st.len(),
                source_path: Some(file.to_string_lossy().to_string()),
                source_db: None,
                meta,
            });
        }
        sessions
    }

    /// 全量解析正文。流式读取，脏尾行跳过，单行超大不炸。
    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>)> {
        let path = session
            .source_path
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("会话缺少 source_path"))?;
        let mut msgs: Vec<Message> = Vec::new();
        let mut seq: i64 = 0;
        let mut derived: Option<String> = None;
        util::each_line(&path, |line, _| {
            let Some(obj) = util::try_parse(line) else { return true };
            let produced = map_line(&obj, seq);
            if let Some(last) = produced.last() {
                seq = last.seq + 1;
                msgs.extend(produced);
            }
            if obj.get("type").and_then(|v| v.as_str()) == Some("ai-title") {
                if let Some(t) = obj.get("aiTitle").and_then(|v| v.as_str()) {
                    if !t.is_empty() {
                        derived = Some(t.to_string());
                    }
                }
            }
            true
        })?;
        Ok((msgs, derived))
    }

    pub fn original_paths(&self, session: &Session) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(sp) = &session.source_path {
            let p = PathBuf::from(sp);
            out.push(p.clone());
            let dir = p.with_extension("");
            if dir.is_dir() {
                out.push(dir);
            }
        }
        out
    }
}
