//! 检索索引库（SQLite FTS5 trigram + 短查询 LIKE 兜底双通道）。
//!
//! trigram 对少于 3 字符的查询会静默返回空，中文两字词最常见，
//! 因此检索双通道，返回值带 mode 如实告知。

use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

use crate::model::{Message, Session};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT
);
CREATE TABLE IF NOT EXISTS sessions (
  uid          TEXT PRIMARY KEY,
  agent        TEXT NOT NULL,
  session_id   TEXT NOT NULL,
  project_path TEXT,
  path_source  TEXT,
  title        TEXT,
  created_at   INTEGER,
  updated_at   INTEGER,
  bytes        INTEGER,
  source_path  TEXT,
  source_db    TEXT,
  meta_json    TEXT,
  msg_count    INTEGER,
  src_mtime    REAL,
  indexed_at   INTEGER
);
CREATE INDEX IF NOT EXISTS ix_sessions_agent ON sessions(agent);
CREATE INDEX IF NOT EXISTS ix_sessions_updated ON sessions(updated_at DESC);

CREATE TABLE IF NOT EXISTS messages (
  id        INTEGER PRIMARY KEY,
  uid       TEXT NOT NULL,
  seq       INTEGER NOT NULL,
  role      TEXT,
  kind      TEXT,
  ts        INTEGER,
  text      TEXT,
  tool_name TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_messages ON messages(uid, seq);
CREATE INDEX IF NOT EXISTS ix_messages_uid ON messages(uid);
CREATE INDEX IF NOT EXISTS ix_messages_kind ON messages(kind);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
  USING fts5(text, content='messages', content_rowid='id', tokenize='trigram');
"#;

pub struct IndexDb {
    pub file: PathBuf,
    con: Connection,
}

pub struct ListOpts {
    pub agent: Option<String>,
    pub project: Option<String>,
    pub limit: i64,
    pub offset: i64,
    pub order: String, // updated | created
    pub dir: String,   // asc | desc
}

impl Default for ListOpts {
    fn default() -> Self {
        ListOpts { agent: None, project: None, limit: 50, offset: 0, order: "updated".into(), dir: "desc".into() }
    }
}

fn mtime_of(session: &Session) -> Option<f64> {
    let p = session.source_path.as_deref().or(session.source_db.as_deref())?;
    let md = std::fs::metadata(p).ok()?;
    let t = md.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as f64)
}

impl IndexDb {
    pub fn open(file: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let con = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)
            .with_context(|| format!("打开索引库失败：{}", file.display()))?;
        con.pragma_update(None, "journal_mode", "WAL").ok();
        con.pragma_update(None, "synchronous", "NORMAL").ok();
        con.execute_batch(SCHEMA)?;
        Ok(IndexDb { file: file.to_path_buf(), con })
    }

    /// 该会话是否需要（重新）索引：源文件未变且已索引过则跳过。
    pub fn needs_index(&self, session: &Session) -> bool {
        let Ok(mtime) = self
            .con
            .query_row("select src_mtime from sessions where uid = ?1", [&session.uid], |r| r.get::<_, Option<f64>>(0))
        else {
            return true;
        };
        let Some(prev) = mtime else { return true };
        match mtime_of(session) {
            Some(cur) => (prev - cur).abs() > 0.5,
            None => true,
        }
    }

    /// 覆盖式写入一个会话及其消息，整体一个事务。
    pub fn put_session(&self, session: &Session, messages: &[Message]) -> anyhow::Result<()> {
        let mtime = mtime_of(session);
        let now = chrono::Utc::now().timestamp_millis();
        self.con.execute_batch("BEGIN")?;
        let result = (|| -> anyhow::Result<()> {
            // FTS 是 external content 表，必须显式发 delete 指令再删主表
            let mut stmt = self.con.prepare("select id, text from messages where uid = ?1")?;
            let old: Vec<(i64, String)> = stmt
                .query_map([&session.uid], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?
                .flatten()
                .collect();
            drop(stmt);
            {
                let mut del = self.con.prepare("insert into messages_fts(messages_fts, rowid, text) values('delete', ?1, ?2)")?;
                for (id, text) in &old {
                    del.execute(params![id, text])?;
                }
            }
            self.con.execute("delete from messages where uid = ?1", [&session.uid])?;

            self.con.execute(
                "insert or replace into sessions
                 (uid, agent, session_id, project_path, path_source, title, created_at, updated_at,
                  bytes, source_path, source_db, meta_json, msg_count, src_mtime, indexed_at)
                 values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    session.uid,
                    session.agent,
                    session.session_id,
                    session.project_path,
                    session.path_source,
                    session.title,
                    session.created_at,
                    session.updated_at,
                    session.bytes as i64,
                    session.source_path,
                    session.source_db,
                    session.meta.to_string(),
                    messages.len() as i64,
                    mtime,
                    now,
                ],
            )?;

            let mut ins_fts = self.con.prepare("insert into messages_fts(rowid, text) values (?1, ?2)")?;
            for m in messages {
                self.con.execute(
                    "insert into messages(uid, seq, role, kind, ts, text, tool_name) values (?1,?2,?3,?4,?5,?6,?7)",
                    params![session.uid, m.seq, m.role, m.kind, m.ts, m.text, m.tool_name],
                )?;
                let rowid = self.con.last_insert_rowid();
                ins_fts.execute(params![rowid, m.text])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.con.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                self.con.execute_batch("ROLLBACK").ok();
                Err(e)
            }
        }
    }

    pub fn all_uids(&self) -> Vec<String> {
        self.con
            .prepare("select uid from sessions")
            .map(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()))
            .map(|x| x.unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn remove_session(&self, uid: &str) -> anyhow::Result<()> {
        let old: Vec<(i64, String)> = self
            .con
            .prepare("select id, text from messages where uid = ?1")?
            .query_map([uid], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?
            .flatten()
            .collect();
        self.con.execute_batch("BEGIN")?;
        let r = (|| -> anyhow::Result<()> {
            {
                let mut del = self.con.prepare("insert into messages_fts(messages_fts, rowid, text) values('delete', ?1, ?2)")?;
                for (id, text) in &old {
                    del.execute(params![id, text])?;
                }
            }
            self.con.execute("delete from messages where uid = ?1", [uid])?;
            self.con.execute("delete from sessions where uid = ?1", [uid])?;
            Ok(())
        })();
        match r {
            Ok(()) => {
                self.con.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                self.con.execute_batch("ROLLBACK").ok();
                Err(e)
            }
        }
    }

    pub fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.con.execute(
            "insert or replace into meta(key, value) values (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Value {
        let (n, m, b) = self
            .con
            .query_row(
                "select count(*), coalesce(sum(msg_count),0), coalesce(sum(bytes),0) from sessions",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
            )
            .unwrap_or((0, 0, 0));
        let mut stmt = self
            .con
            .prepare("select agent, count(*), coalesce(sum(msg_count),0) from sessions group by agent order by 2 desc")
            .unwrap();
        let by_agent: Vec<Value> = stmt
            .query_map([], |r| Ok(json!({ "agent": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)?, "m": r.get::<_, i64>(2)? })))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        let indexed_at: Option<String> = self
            .con
            .query_row("select value from meta where key='indexed_at'", [], |r| r.get(0))
            .ok();
        json!({
            "sessions": n,
            "messages": m,
            "bytes": b,
            "byAgent": by_agent,
            "indexedAt": indexed_at,
            "dbFile": self.file.to_string_lossy(),
            "dbSize": std::fs::metadata(&self.file).map(|m| m.len()).unwrap_or(0),
        })
    }

    pub fn list_sessions(&self, opts: &ListOpts) -> Vec<Value> {
        let mut where_parts = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(a) = &opts.agent {
            where_parts.push("agent = ?".to_string());
            args.push(Box::new(a.clone()));
        }
        if let Some(p) = &opts.project {
            where_parts.push("(project_path like ? or title like ?)".to_string());
            args.push(Box::new(format!("%{p}%")));
            args.push(Box::new(format!("%{p}%")));
        }
        let col = if opts.order == "created" { "created_at" } else { "updated_at" };
        let dir = if opts.dir.eq_ignore_ascii_case("asc") { "asc" } else { "desc" };
        // limit <= 0 视为不限制
        let limit_sql = if opts.limit > 0 {
            format!(" limit {} offset {}", opts.limit, opts.offset)
        } else {
            String::new()
        };
        let sql = format!(
            "select * from sessions {} order by {} {}{}",
            if where_parts.is_empty() { String::new() } else { format!("where {}", where_parts.join(" and ")) },
            col,
            dir,
            limit_sql,
        );
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        self.con
            .prepare(&sql)
            .map(|mut s| rows_to_json(&mut s, &params_ref))
            .unwrap_or_default()
    }

    pub fn get_session(&self, uid: &str) -> Option<(Value, Vec<Value>)> {
        let session = self.session_json_by_uid(uid)?;
        let messages = self.messages_json(uid);
        Some((session, messages))
    }

    fn session_json_by_uid(&self, uid: &str) -> Option<Value> {
        self.con
            .prepare("select * from sessions where uid = ?1")
            .ok()?
            .query_row([uid], |r| {
                let mut obj = serde_json::Map::new();
                for (i, name) in r.as_ref().column_names().iter().enumerate() {
                    let v: Value = match r.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Null) => Value::Null,
                        Ok(rusqlite::types::ValueRef::Integer(x)) => json!(x),
                        Ok(rusqlite::types::ValueRef::Real(x)) => json!(x),
                        Ok(rusqlite::types::ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
                        Ok(rusqlite::types::ValueRef::Blob(_)) => Value::Null,
                        Err(_) => Value::Null,
                    };
                    obj.insert(name.to_string(), v);
                }
                Ok(Value::Object(obj))
            })
            .ok()
    }

    fn messages_json(&self, uid: &str) -> Vec<Value> {
        self.con
            .prepare("select seq, role, kind, ts, text, tool_name from messages where uid = ?1 order by seq asc")
            .map(|mut s| {
                s.query_map([uid], |r| {
                    Ok(json!({
                        "seq": r.get::<_, i64>(0)?,
                        "role": r.get::<_, Option<String>>(1)?,
                        "kind": r.get::<_, Option<String>>(2)?,
                        "ts": r.get::<_, Option<i64>>(3)?,
                        "text": r.get::<_, Option<String>>(4)?,
                        "tool_name": r.get::<_, Option<String>>(5)?,
                    }))
                })
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// 双通道全文检索，结果按会话聚合（同一会话只出一条，match_count 记命中消息数）。
    pub fn search(&self, query: &str, agent: Option<&str>, project: Option<&str>, kinds: Option<&[String]>, limit: i64) -> Value {
        let q = query.trim();
        if q.is_empty() {
            return json!({ "mode": "fts", "hits": [], "total_matches": 0, "truncated": false });
        }
        let mut filters: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(a) = agent {
            filters.push("s.agent = ?".into());
            args.push(Box::new(a.to_string()));
        }
        if let Some(p) = project {
            filters.push("(s.project_path like ? or s.title like ?)".into());
            args.push(Box::new(format!("%{p}%")));
            args.push(Box::new(format!("%{p}%")));
        }
        if let Some(k) = kinds {
            if !k.is_empty() {
                filters.push(format!("m.kind in ({})", k.iter().map(|_| "?".to_string()).collect::<Vec<_>>().join(",")));
                for x in k {
                    args.push(Box::new(x.clone()));
                }
            }
        }
        let filter_sql = if filters.is_empty() { String::new() } else { format!("and {}", filters.join(" and ")) };

        let raw_limit = 2000i64;
        let cols = "s.uid, s.agent, s.title, s.project_path, s.updated_at, s.source_path, m.seq, m.role, m.kind, m.ts, m.text";
        let (mode, sql, phrase) = if q.chars().count() >= 3 {
            let phrase = format!("\"{}\"", q.replace('"', "\"\""));
            (
                "fts",
                format!(
                    "select {cols} from messages_fts f
                     join messages m on m.id = f.rowid
                     join sessions s on s.uid = m.uid
                     where messages_fts match ?1 {filter_sql}
                     order by s.updated_at desc, m.seq asc limit {raw_limit}"
                ),
                phrase,
            )
        } else {
            (
                "like",
                format!(
                    "select {cols} from messages m
                     join sessions s on s.uid = m.uid
                     where m.text like ?1 {filter_sql}
                     order by s.updated_at desc, m.seq asc limit {raw_limit}"
                ),
                format!("%{q}%"),
            )
        };

        let mut all_args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(phrase)];
        all_args.extend(args);

        let rows = self
            .con
            .prepare(&sql)
            .map(|mut s| {
                let pref: Vec<&dyn rusqlite::types::ToSql> = all_args.iter().map(|b| b.as_ref()).collect();
                s.query_map(&pref[..], |r| {
                    Ok(json!({
                        "uid": r.get::<_, String>(0)?,
                        "agent": r.get::<_, String>(1)?,
                        "title": r.get::<_, Option<String>>(2)?,
                        "project_path": r.get::<_, Option<String>>(3)?,
                        "updated_at": r.get::<_, Option<i64>>(4)?,
                        "source_path": r.get::<_, Option<String>>(5)?,
                        "seq": r.get::<_, Option<i64>>(6)?,
                        "role": r.get::<_, Option<String>>(7)?,
                        "kind": r.get::<_, Option<String>>(8)?,
                        "ts": r.get::<_, Option<i64>>(9)?,
                        "text": r.get::<_, Option<String>>(10)?,
                        "match_count": 1,
                    }))
                })
                .map(|rows| rows.flatten().collect::<Vec<_>>())
                .unwrap_or_default()
            })
            .unwrap_or_default();

        // 按会话聚合（rows 已按 updated desc 排序，保持首现顺序）
        let mut by_uid: Vec<Value> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = Default::default();
        let mut total = 0i64;
        for r in rows {
            total += 1;
            let uid = r["uid"].as_str().unwrap_or("").to_string();
            match index.get(&uid) {
                Some(i) => {
                    let cur = by_uid[*i]["match_count"].as_i64().unwrap_or(0);
                    by_uid[*i]["match_count"] = json!(cur + 1);
                }
                None => {
                    index.insert(uid.clone(), by_uid.len());
                    by_uid.push(r);
                }
            }
        }
        if limit > 0 {
            by_uid.truncate(limit as usize);
        }
        json!({ "mode": mode, "hits": by_uid, "total_matches": total, "truncated": total >= raw_limit })
    }
}

/// 把查询结果的行转成 JSON 数组（列名 → 值）。
fn rows_to_json(stmt: &mut rusqlite::Statement, params: &[&dyn rusqlite::types::ToSql]) -> Vec<Value> {
    stmt.query_map(params, |r| {
        let mut obj = serde_json::Map::new();
        for (i, name) in r.as_ref().column_names().iter().enumerate() {
            let v: Value = match r.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => Value::Null,
                Ok(rusqlite::types::ValueRef::Integer(x)) => json!(x),
                Ok(rusqlite::types::ValueRef::Real(x)) => json!(x),
                Ok(rusqlite::types::ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
                Ok(rusqlite::types::ValueRef::Blob(_)) => Value::Null,
                Err(_) => Value::Null,
            };
            obj.insert(name.to_string(), v);
        }
        Ok(Value::Object(obj))
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}
