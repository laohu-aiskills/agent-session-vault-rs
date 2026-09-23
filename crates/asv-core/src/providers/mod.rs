//! Provider 注册表：统一入口，形状差异用 enum 抹平。

pub mod codex;
pub mod jsonl_project;
pub mod kimi;
pub mod pi;
pub mod restricted;
pub mod sqlite_opencode;
pub mod title_index;

use serde_json::Value;

use crate::model::{Message, Session};
use crate::util;

use jsonl_project::JsonlProjectProvider;
use kimi::{KimiCliProvider, KimiCodeProvider};
use pi::PiProvider;
use restricted::RestrictedProvider;
use sqlite_opencode::SqliteOpencodeProvider;

/// 统一的探测结果。
pub struct Detect {
    pub available: bool,
    pub root: String,
    pub reason: String,
    pub restricted: bool,
    pub installed: bool,
}

pub enum Provider {
    Jsonl(JsonlProjectProvider),
    Codex(codex::CodexProvider),
    Pi(pi::PiProvider),
    KimiCli(KimiCliProvider),
    KimiCode(KimiCodeProvider),
    Sqlite(SqliteOpencodeProvider),
    Restricted(RestrictedProvider),
}

impl Provider {
    pub fn id(&self) -> &str {
        match self {
            Provider::Jsonl(p) => &p.id,
            Provider::Codex(_) => "codex",
            Provider::Pi(_) => "pi",
            Provider::KimiCli(_) => "kimi-cli",
            Provider::KimiCode(_) => "kimi-code",
            Provider::Sqlite(p) => &p.id,
            Provider::Restricted(p) => &p.id,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Provider::Jsonl(p) => p.label.clone(),
            Provider::Codex(p) => p.label().into(),
            Provider::Pi(p) => p.label().into(),
            Provider::KimiCli(_) => "Kimi CLI".into(),
            Provider::KimiCode(_) => "Kimi Code".into(),
            Provider::Sqlite(p) => p.label.clone(),
            Provider::Restricted(p) => p.label.clone(),
        }
    }

    pub fn restricted(&self) -> bool {
        matches!(self, Provider::Restricted(_))
    }

    pub fn detect(&self) -> Detect {
        let restricted = self.restricted();
        let (available, root, reason, installed) = match self {
            Provider::Restricted(p) => {
                let (a, r, why, inst) = p.detect();
                (a, r, why, inst)
            }
            Provider::Jsonl(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
            Provider::Codex(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
            Provider::Pi(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
            Provider::KimiCli(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
            Provider::KimiCode(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
            Provider::Sqlite(p) => {
                let (a, r, why) = p.detect();
                (a, r, why, true)
            }
        };
        Detect { available, root, reason, restricted, installed }
    }

    pub fn discover(&mut self) -> anyhow::Result<Vec<Session>> {
        match self {
            Provider::Jsonl(p) => Ok(p.discover()),
            Provider::Codex(p) => Ok(p.discover()),
            Provider::Pi(p) => Ok(p.discover()),
            Provider::KimiCli(p) => Ok(p.discover()),
            Provider::KimiCode(p) => Ok(p.discover()),
            Provider::Sqlite(p) => p.discover(),
            Provider::Restricted(p) => Ok(p.discover()),
        }
    }

    /// 统一返回 (messages, title, meta)。meta 是 parse 阶段才能拿到的元数据（如 token 统计）。
    pub fn parse(&mut self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<Value>)> {
        match self {
            Provider::Jsonl(p) => {
                let (m, t) = p.parse(session)?;
                Ok((m, t, None))
            }
            Provider::Codex(p) => p.parse(session),
            Provider::Pi(p) => p.parse(session),
            Provider::KimiCli(p) => p.parse(session),
            Provider::KimiCode(p) => p.parse(session),
            Provider::Sqlite(p) => p.parse(session),
            Provider::Restricted(p) => {
                let (m, t) = p.parse(session);
                Ok((m, t, None))
            }
        }
    }
}

/// 实测验证过的 Provider（用本机真实数据跑通 discover + parse）。
pub const VERIFIED: [&str; 8] = ["workbuddy", "claude-code", "qwenworkcn", "codex", "kimi-cli", "kimi-code", "zcode", "pi"];

/// 全部注册的 Provider（每次返回新实例：SQLite 需要可变解析状态）。
pub fn all() -> Vec<Provider> {
    let mut workbuddy = JsonlProjectProvider::new("workbuddy", "WorkBuddy", "~/.workbuddy/projects", false);
    // workbuddy.db 的 sessions 表是标题的权威来源
    workbuddy.title_index = Some(jsonl_project::TitleIndexCfg {
        db: util::expand_home("~/.workbuddy/workbuddy.db"),
        table: "sessions".into(),
        id_col: "id".into(),
        title_cols: vec!["custom_title".into(), "title".into()],
        model_col: Some("model".into()),
    });

    vec![
        Provider::Jsonl(workbuddy),
        Provider::Jsonl(JsonlProjectProvider::new("claude-code", "Claude Code", "~/.claude/projects", true)),
        Provider::Jsonl(JsonlProjectProvider::new("qwenworkcn", "Qwen Work", "~/.qwenworkcn/projects", false)),
        Provider::Jsonl(JsonlProjectProvider::new("codebuddy", "CodeBuddy", "~/.codebuddy/projects", false)),
        Provider::Codex(codex::CodexProvider::default()),
        Provider::KimiCli(KimiCliProvider::default()),
        Provider::KimiCode(KimiCodeProvider::default()),
        Provider::Sqlite(SqliteOpencodeProvider::new(
            "zcode",
            "ZCode",
            &["~/.zcode/cli/db/db.sqlite"],
        )),
        Provider::Sqlite(SqliteOpencodeProvider::new(
            "opencode",
            "opencode",
            &[
                "~/.local/share/opencode/opencode.db",
                "~/.local/share/opencode/db/db.sqlite",
                "~/.opencode/db/db.sqlite",
            ],
        )),
        Provider::Pi(PiProvider::default()),
        Provider::Restricted(RestrictedProvider::new(
            "copilot",
            "Copilot CLI",
            "~/.copilot",
            "本机仅有 config.json 与运行日志，未见 session-state 会话目录（该工具的会话落盘位置，未产生数据）",
        )),
        Provider::Restricted(RestrictedProvider::new(
            "cursor",
            "Cursor",
            "~/.cursor",
            "磁盘仅有 projects/<slug>/mcps 目录，会话正文保存在其私有状态库内，无可用落盘格式",
        )),
        Provider::Restricted(RestrictedProvider::new(
            "cline",
            "Cline",
            "~/.cline",
            "仅有扩展状态文件（data/globalState.json、workspaces/*/workspaceState.json），不含会话正文",
        )),
        Provider::Restricted(RestrictedProvider::new(
            "antigravity",
            "Antigravity",
            "~/.gemini/antigravity",
            "brain 与 context_state 目录为空，未产生可解析的会话记录",
        )),
    ]
}

/// 把用户输入的 agent 标识解析成子集 id 列表；支持 "all" 与逗号分隔。
pub fn resolve(spec: &str, providers: &mut Vec<Provider>) -> anyhow::Result<Vec<String>> {
    let ids: Vec<String> = if spec.is_empty() || spec == "all" {
        providers.iter().map(|p| p.id().to_string()).collect()
    } else {
        spec.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    };
    let mut out = Vec::new();
    for id in ids {
        if !providers.iter().any(|p| p.id() == id) {
            let names: Vec<&str> = providers.iter().map(|p| p.id()).collect();
            anyhow::bail!("未知的 Agent 标识：{id}（可用：{}）", names.join(", "));
        }
        out.push(id);
    }
    Ok(out)
}

pub fn is_verified(id: &str) -> bool {
    VERIFIED.contains(&id)
}
