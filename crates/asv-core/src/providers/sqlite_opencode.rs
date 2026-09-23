//! SQLite 家族适配器：ZCode / opencode（✅ 实测）。
//!
//! 表：session / message / part，data 列为 JSON。
//! 目标应用运行中库是热的且带未合并 WAL：直连只读失败则把 db+wal+shm
//! 复制到临时目录再读快照。

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde_json::Value;

use crate::model::{Message, Session};
use crate::util;

pub struct SqliteOpencodeProvider {
    pub id: String,
    pub label: String,
    pub db_candidates: Vec<PathBuf>,
    resolved: std::cell::RefCell<Option<PathBuf>>,
}

impl SqliteOpencodeProvider {
    pub fn new(id: &str, label: &str, db_candidates: &[&str]) -> Self {
        SqliteOpencodeProvider {
            id: id.into(),
            label: label.into(),
            db_candidates: db_candidates.iter().map(|p| util::expand_home(p)).collect(),
            resolved: std::cell::RefCell::new(None),
        }
    }

    /// db + wal + shm 一起复制到临时目录，读快照用。
    fn snapshot(src: &Path) -> anyhow::Result<(PathBuf, tempfile_guard::TempDir)> {
        let dir = tempfile_guard::TempDir::new()?;
        let base = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "db.sqlite".into());
        let dst = dir.path().join(&base);
        fs::copy(src, &dst)?;
        for suffix in ["-wal", "-shm"] {
            let s = PathBuf::from(format!("{}{}", src.to_string_lossy(), suffix));
            if s.exists() {
                let _ = fs::copy(&s, PathBuf::from(format!("{}{}", dst.to_string_lossy(), suffix)));
            }
        }
        Ok((dst, dir))
    }

    fn open_readonly(path: &Path) -> anyhow::Result<rusqlite::Connection> {
        Ok(rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?)
    }

    fn resolve(&self) -> Option<PathBuf> {
        if let Some(p) = self.resolved.borrow().as_ref() {
            return Some(p.clone());
        }
        for cand in &self.db_candidates {
            if !cand.exists() {
                continue;
            }
            if self.schema_ok(cand) {
                *self.resolved.borrow_mut() = Some(cand.clone());
                return self.resolved.borrow().clone();
            }
        }
        None
    }

    /// 表结构指纹校验，避免把同名库当目标库。
    fn schema_ok(&self, p: &Path) -> bool {
        let con = match Self::open_readonly(p) {
            Ok(c) => c,
            Err(_) => {
                let (snap, guard) = match Self::snapshot(p) {
                    Ok(x) => x,
                    Err(_) => return false,
                };
                let ok = Self::open_readonly(&snap)
                    .and_then(|c| {
                        let mut stmt = c.prepare("select name from sqlite_master where type='table'")?;
                        let names: Vec<String> = stmt
                            .query_map([], |r| r.get::<_, String>(0))?
                            .flatten()
                            .collect();
                        Ok(names)
                    })
                    .map(|names| {
                        names.contains(&"session".to_string())
                            && names.contains(&"message".to_string())
                            && names.contains(&"part".to_string())
                    })
                    .unwrap_or(false);
                drop(guard);
                return ok;
            }
        };
        let mut stmt = match con.prepare("select name from sqlite_master where type='table'") {
            Ok(s) => s,
            Err(_) => return false,
        };
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        names.contains(&"session".to_string())
            && names.contains(&"message".to_string())
            && names.contains(&"part".to_string())
    }

    pub fn detect(&self) -> (bool, String, String) {
        if let Some(p) = self.resolve() {
            return (true, p.to_string_lossy().to_string(), format!("数据库可用：{}", p.display()));
        }
        let missing = self.db_candidates.iter().all(|c| !c.exists());
        if missing {
            return (
                false,
                self.db_candidates
                    .first()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                "未找到数据库（该 Agent 未安装或未产生会话）".into(),
            );
        }
        (
            false,
            self.db_candidates
                .iter()
                .find(|c| c.exists())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            "存在同名文件但表结构不匹配（非 session/message/part 布局）".into(),
        )
    }

    /// 打开可用连接；直连失败降级临时快照。
    fn open_with_fallback(&self) -> anyhow::Result<(rusqlite::Connection, Option<tempfile_guard::TempDir>)> {
        let p = self.resolve().ok_or_else(|| anyhow::anyhow!("{}: 数据库不可用", self.label))?;
        match Self::open_readonly(&p) {
            Ok(c) => Ok((c, None)),
            Err(_) => {
                let (snap, guard) = Self::snapshot(&p)?;
                Ok((Self::open_readonly(&snap)?, Some(guard)))
            }
        }
    }

    pub fn discover(&self) -> anyhow::Result<Vec<Session>> {
        let Some(db) = self.resolve() else { return Ok(Vec::new()) };
        let (con, _guard) = self.open_with_fallback()?;
        let mut stmt = con.prepare(
            "select id, directory, path, title, version, time_created, time_updated from session order by time_updated desc",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut sessions = Vec::new();
        for row in rows.flatten() {
            let (id, directory, path_, title, version, created, updated) = row;
            let (count, last): (i64, Option<i64>) = con
                .query_row(
                    "select count(*), max(time_updated) from message where session_id = ?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((0, None));
            sessions.push(Session {
                uid: format!("{}:{}", self.id, id),
                agent: self.id.clone(),
                session_id: id,
                project_path: directory.or(path_),
                path_source: "db".into(),
                title,
                created_at: created,
                updated_at: updated.or(last),
                bytes: 0,
                source_path: None,
                source_db: Some(db.to_string_lossy().to_string()),
                meta: serde_json::json!({ "version": version, "messageCount": count }),
            });
        }
        Ok(sessions)
    }

    pub fn parse(&self, session: &Session) -> anyhow::Result<(Vec<Message>, Option<String>, Option<serde_json::Value>)> {
        let (con, _guard) = self.open_with_fallback()?;
        let mut msgs = con.prepare(
            "select id, time_created, data from message where session_id = ?1 order by time_created asc, sequence asc",
        )?
        .query_map([&session.session_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))
        })?
        .flatten()
        .collect::<Vec<_>>();

        let mut out = Vec::new();
        let mut seq: i64 = 0;
        for (id, mtime, data) in msgs.drain(..) {
            let mdata: Value = serde_json::from_str(&data).unwrap_or(serde_json::json!({}));
            let role = if mdata.get("role").and_then(|v| v.as_str()) == Some("user") { "user" } else { "assistant" };
            let mut stmt = con.prepare(
                "select time_created, data from part where message_id = ?1 order by sequence asc, time_created asc",
            )?;
            let parts = stmt
                .query_map([id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?)))?
                .flatten()
                .collect::<Vec<_>>();
            for (ptime, pdata) in parts {
                let d: Value = serde_json::from_str(&pdata).unwrap_or(serde_json::json!({}));
                let ts = d
                    .pointer("/time/start")
                    .or_else(|| d.pointer("/time/end"))
                    .and_then(|v| util::to_ms(Some(v)))
                    .or(ptime)
                    .or(mtime);
                match d.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text" => {
                        let t = d.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !t.is_empty() {
                            out.push(Message::new(seq, role, "text", ts, t));
                            seq += 1;
                        }
                    }
                    "reasoning" => {
                        let t = d.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !t.is_empty() {
                            out.push(Message::new(seq, "assistant", "reasoning", ts, t));
                            seq += 1;
                        }
                    }
                    "tool" => {
                        let input = d.pointer("/state/input").cloned().unwrap_or(serde_json::json!({}));
                        let text = match &input {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        let tool = d.get("tool").and_then(|v| v.as_str()).map(String::from);
                        let mut m = Message::new(seq, "assistant", "tool_call", ts, text);
                        m.tool_name = tool.clone();
                        m.tool_input = Some(input);
                        out.push(m);
                        seq += 1;

                        let res = d
                            .pointer("/state/output")
                            .or_else(|| d.pointer("/state/error"))
                            .or_else(|| d.pointer("/state/metadata"))
                            .cloned();
                        let text = match res {
                            Some(Value::String(s)) => s,
                            Some(other @ Value::Object(_)) | Some(other @ Value::Array(_)) => {
                                serde_json::to_string(&other).unwrap_or_default()
                            }
                            _ => String::new(),
                        };
                        if !text.is_empty() {
                            let mut m = Message::new(seq, "tool", "tool_result", ts, text.clone());
                            m.tool_name = tool.clone();
                            m.tool_result = Some(text);
                            out.push(m);
                            seq += 1;
                        }
                    }
                    // file / timeline / step-start / step-finish 不进正文
                    _ => {}
                }
            }
        }
        Ok((out, None, None))
    }

    pub fn original_paths(&self) -> Vec<PathBuf> {
        let Some(p) = self.resolve() else { return Vec::new() };
        [p.clone(), PathBuf::from(format!("{}-wal", p.display())), PathBuf::from(format!("{}-shm", p.display()))]
            .into_iter()
            .filter(|x| x.exists())
            .collect()
    }
}

/// 临时目录 RAII：Drop 时递归删除。
mod tempfile_guard {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().join(format!("asv-snap-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path)?;
            Ok(TempDir { path })
        }
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
