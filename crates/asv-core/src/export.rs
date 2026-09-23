//! 导出为可读文档（Markdown / HTML）。
//! HTML 完全自包含：不引用任何 CDN、字体或外部资源，内网双击即开。

use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::model::{Message, Session};
use crate::util::format_time;

fn esc(s: impl AsRef<str>) -> String {
    s.as_ref()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn kind_tag(kind: &str) -> &'static str {
    match kind {
        "reasoning" => "推理",
        "tool_call" => "工具调用",
        "tool_result" => "工具结果",
        "meta" => "元信息",
        _ => "",
    }
}

fn role_name(role: &str) -> &str {
    match role {
        "user" => "用户",
        "assistant" => "助手",
        "system" => "系统",
        "tool" => "工具",
        other => other,
    }
}

fn pretty_maybe_json(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() || !(t.starts_with('[') || t.starts_with('{')) {
        return s.to_string();
    }
    serde_json::from_str::<serde_json::Value>(t)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| s.to_string())
}

pub fn to_markdown(session: &Session, messages: &[Message]) -> String {
    let title = session.title.clone().unwrap_or_else(|| format!("会话 {}", session.session_id));
    let mut l = Vec::new();
    l.push(format!("# {}", title));
    l.push(String::new());
    l.push("| 项 | 值 |".into());
    l.push("| --- | --- |".into());
    l.push(format!("| Agent | `{}` |", session.agent));
    l.push(format!("| 会话 ID | `{}` |", session.session_id));
    l.push(format!("| 项目路径 | `{}` |", session.project_path.clone().unwrap_or_else(|| "(未知)".into())));
    l.push(format!("| 创建时间 | {} |", session.created_at.map(format_time).unwrap_or_else(|| "-".into())));
    l.push(format!("| 更新时间 | {} |", session.updated_at.map(format_time).unwrap_or_else(|| "-".into())));
    l.push(format!("| 消息条数 | {} |", messages.len()));
    l.push(String::new());
    l.push("---".into());
    l.push(String::new());

    for (i, m) in messages.iter().enumerate() {
        let n = i + 1;
        let kind = kind_tag(&m.kind);
        let role = role_name(&m.role);
        let head = if kind.is_empty() { role.to_string() } else { format!("{role} · {kind}") };
        l.push(format!(
            "### {n}. {head}{}",
            if let Some(ts) = m.ts { format!("  <sub>{}</sub>", format_time(ts)) } else { String::new() }
        ));
        l.push(String::new());
        match m.kind.as_str() {
            "tool_call" => {
                l.push(format!("`{}`", m.tool_name.clone().unwrap_or_else(|| "工具".into())));
                l.push(String::new());
                l.push("```json".into());
                l.push(pretty_maybe_json(&m.text));
                l.push("```".into());
                l.push(String::new());
            }
            "tool_result" => {
                l.push("```text".into());
                let t: String = m.text.chars().take(20000).collect();
                l.push(t);
                l.push("```".into());
                if m.text.chars().count() > 20000 {
                    l.push("> （内容过长，已截断，完整内容见原始会话文件）".into());
                }
                l.push(String::new());
            }
            "reasoning" => {
                l.push("<details><summary>展开推理过程</summary>".into());
                l.push(String::new());
                l.push(m.text.clone());
                l.push(String::new());
                l.push("</details>".into());
                l.push(String::new());
            }
            _ => {
                l.push(m.text.clone());
                l.push(String::new());
            }
        }
    }
    l.join("\n")
}

pub fn to_html(session: &Session, messages: &[Message]) -> String {
    let title = session.title.clone().unwrap_or_else(|| format!("会话 {}", session.session_id));
    let mut rows: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let n = i + 1;
        let kind = kind_tag(&m.kind);
        let role = role_name(&m.role);
        let cls = format!("m-{} k-{}", m.role, m.kind);
        let body = match m.kind.as_str() {
            "tool_call" => format!(
                "<div class=\"tool\">{}</div><pre class=\"json\">{}</pre>",
                esc(m.tool_name.clone().unwrap_or_else(|| "工具".into())),
                esc(pretty_maybe_json(&m.text))
            ),
            "tool_result" => {
                let t: String = m.text.chars().take(20000).collect();
                let note = if m.text.chars().count() > 20000 { "<p class=\"note\">内容过长已截断</p>" } else { "" };
                format!("<pre class=\"out\">{}</pre>{note}", esc(t))
            }
            "reasoning" => format!(
                "<details><summary>展开推理过程</summary><pre class=\"think\">{}</pre></details>",
                esc(&m.text)
            ),
            _ => format!("<div class=\"txt\">{}</div>", esc(&m.text).replace('\n', "<br>")),
        };
        rows.push(format!(
            "<article class=\"m {cls}\">\n  <div class=\"hd\"><span class=\"seq\">{n}</span><span class=\"role\">{}</span>{}<span class=\"ts\">{}</span></div>\n  <div class=\"bd\">{body}</div>\n</article>",
            esc(role),
            if kind.is_empty() { String::new() } else { format!("<span class=\"kind\">{}</span>", esc(kind)) },
            esc(&m.ts.map(format_time).unwrap_or_default()),
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{}</title>
<style>
:root{{color-scheme:light}}
*{{box-sizing:border-box}}
body{{margin:0;padding:32px 20px;background:#f6f6f4;color:#1f1f1d;
  font:14px/1.7 -apple-system,BlinkMacSystemFont,"Segoe UI","Microsoft YaHei",sans-serif}}
.wrap{{max-width:920px;margin:0 auto}}
h1{{font-size:22px;font-weight:600;margin:0 0 16px}}
table{{border-collapse:collapse;width:100%;margin-bottom:24px;background:#fff;
  border:1px solid #e2e2de;border-radius:10px;overflow:hidden}}
td{{padding:8px 14px;border-bottom:1px solid #eeeeea;font-size:13px}}
tr:last-child td{{border-bottom:0}}
td:first-child{{width:110px;color:#6b6b66}}
code{{font-family:ui-monospace,Consolas,monospace;font-size:12.5px;background:#f0f0ec;padding:1px 5px;border-radius:4px}}
article{{background:#fff;border:1px solid #e2e2de;border-radius:10px;padding:14px 16px;margin-bottom:12px}}
article.m-user{{border-left:3px solid #185fa5}}
article.m-assistant{{border-left:3px solid #0f6e56}}
article.m-tool{{border-left:3px solid #888780}}
.hd{{display:flex;align-items:center;gap:8px;margin-bottom:8px;font-size:12px;color:#6b6b66}}
.seq{{display:inline-flex;min-width:22px;height:22px;align-items:center;justify-content:center;
  background:#f0f0ec;border-radius:6px;font-weight:500;color:#444}}
.role{{font-weight:500;color:#1f1f1d}}
.kind{{background:#eef3f8;color:#185fa5;padding:1px 6px;border-radius:4px}}
.ts{{margin-left:auto}}
.bd{{font-size:13.5px}}
.txt{{white-space:pre-wrap;word-break:break-word}}
pre{{margin:0;padding:10px 12px;background:#fafaf8;border:1px solid #eeeeea;border-radius:8px;
  overflow-x:auto;font:12.5px/1.6 ui-monospace,Consolas,monospace;white-space:pre-wrap;word-break:break-word}}
pre.out{{max-height:420px;overflow:auto;color:#444}}
pre.think{{color:#5f5e5a}}
.tool{{display:inline-block;font:12.5px ui-monospace,Consolas,monospace;background:#eef3f8;
  color:#185fa5;padding:2px 8px;border-radius:5px;margin-bottom:6px}}
details summary{{cursor:pointer;font-size:12.5px;color:#6b6b66}}
.note{{font-size:12px;color:#888780;margin:6px 0 0}}
footer{{margin-top:28px;font-size:12px;color:#888780;text-align:center}}
</style>
</head>
<body>
<div class="wrap">
<h1>{}</h1>
<table>
<tr><td>Agent</td><td><code>{}</code></td></tr>
<tr><td>会话 ID</td><td><code>{}</code></td></tr>
<tr><td>项目路径</td><td><code>{}</code></td></tr>
<tr><td>创建时间</td><td>{}</td></tr>
<tr><td>更新时间</td><td>{}</td></tr>
<tr><td>消息条数</td><td>{}</td></tr>
</table>
{}
<footer>由 Agent Session Vault 导出于 {}</footer>
</div>
</body>
</html>"#,
        esc(&title),
        esc(&title),
        esc(&session.agent),
        esc(&session.session_id),
        esc(session.project_path.clone().unwrap_or_else(|| "(未知)".into())),
        esc(&session.created_at.map(format_time).unwrap_or_else(|| "-".into())),
        esc(&session.updated_at.map(format_time).unwrap_or_else(|| "-".into())),
        messages.len(),
        rows.join("\n"),
        esc(&format_time(chrono::Utc::now().timestamp_millis())),
    )
}

/// 导出到文件，格式由扩展名或显式 format 决定（"md" | "html"）。
pub fn export_session(session: &Session, messages: &[Message], out: &Path, format: Option<&str>) -> anyhow::Result<(PathBuf, String, u64)> {
    let fmt = format
        .map(String::from)
        .unwrap_or_else(|| if out.to_string_lossy().to_lowercase().ends_with(".md") { "md".into() } else { "html".into() });
    let content = if fmt == "md" { to_markdown(session, messages) } else { to_html(session, messages) };
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).with_context(|| format!("创建目录失败：{}", dir.display()))?;
    }
    fs::write(out, &content).with_context(|| format!("写入失败：{}", out.display()))?;
    Ok((out.to_path_buf(), fmt, content.len() as u64))
}

use std::path::PathBuf;
