//! 统一会话模型：所有适配器最终归一到 Session / Message。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_TEXT: usize = 200 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub uid: String,
    pub agent: String,
    pub session_id: String,
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default)]
    pub path_source: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub source_db: Option<String>,
    #[serde(default = "default_meta")]
    pub meta: Value,
}

fn default_meta() -> Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub seq: i64,
    pub role: String,
    pub kind: String,
    #[serde(default)]
    pub ts: Option<i64>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_result: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}

impl Message {
    pub fn new(seq: i64, role: &str, kind: &str, ts: Option<i64>, text: impl Into<String>) -> Self {
        let mut text = text.into();
        let mut truncated = false;
        if text.chars().count() > MAX_TEXT {
            text = text.chars().take(MAX_TEXT).collect();
            truncated = true;
        }
        Message {
            seq,
            role: role.into(),
            kind: kind.into(),
            ts,
            text,
            tool_name: None,
            tool_input: None,
            tool_result: None,
            truncated,
        }
    }
}

/// 判断一段用户文本是不是 Agent 注入的噪声块（系统提示 / 技能注入等）。
/// 命中的文本不作为标题来源。
pub fn is_noise_text(t: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "<system-reminder",
        "<command-name>",
        "# AGENTS.md",
        "<INSTRUCTIONS>",
        "<identity_context>",
        "<skill ",
        "<user-memory>",
        "<environment_context>",
    ];
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// 从消息里挑一条像人写的标题：优先取干净的用户输入，
/// 全是噪声则剥掉 XML 标签后兜底。
pub fn derive_title(msgs: &[Message]) -> Option<String> {
    let mut fallback: Option<&str> = None;
    for m in msgs {
        if m.role != "user" || m.kind != "text" {
            continue;
        }
        let t = m.text.trim();
        if t.is_empty() {
            continue;
        }
        if !is_noise_text(t) {
            return Some(clip(t));
        }
        if fallback.is_none() {
            fallback = Some(t);
        }
    }
    fallback.map(|t| {
        let stripped = strip_xml_tags(t);
        clip(&stripped)
    })
}

fn strip_xml_tags(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = 0usize;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(t: &str) -> String {
    let s: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > 80 {
        format!("{}…", chars[..80].iter().collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_prefers_clean_input() {
        let msgs = vec![
            Message::new(0, "user", "text", None, "<system-reminder>x</system-reminder>"),
            Message::new(1, "user", "text", None, "帮我修一个登录 bug"),
        ];
        assert_eq!(derive_title(&msgs).unwrap(), "帮我修一个登录 bug");
    }

    #[test]
    fn title_falls_back_to_stripped_noise() {
        let msgs = vec![Message::new(0, "user", "text", None, "<skill name=\"x\">内容</skill>")];
        let t = derive_title(&msgs).unwrap();
        assert!(!t.contains('<'));
    }
}
