//! Agent Session Vault — Tauri GUI。
//! 复用 asv-core：providers / index / scan / backup 的能力直接挂成 IPC 命令。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use serde_json::{json, Value};

use asv_core::index_db::{IndexDb, ListOpts};
use asv_core::providers::{self, Provider};
use asv_core::scan;

struct AppState {
    providers: Mutex<Vec<Provider>>,
}

fn index_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".agent-session-vault")
        .join("index.db")
}

fn with_index<T>(f: impl FnOnce(&IndexDb) -> T) -> Result<T, String> {
    let db = IndexDb::open(&index_path()).map_err(|e| e.to_string())?;
    Ok(f(&db))
}

#[tauri::command]
async fn agents(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let mut provs = state.providers.lock().map_err(|e| e.to_string())?;
    let d = scan::discover_all(&mut provs);
    let agents: Vec<Value> = d
        .agents
        .iter()
        .map(|(id, label, e)| {
            json!({
                "id": id, "label": label,
                "available": e.available, "restricted": e.restricted,
                "reason": e.reason, "count": e.count, "bytes": e.bytes,
                "verified": providers::is_verified(id),
            })
        })
        .collect();
    Ok(json!(agents))
}

#[tauri::command]
async fn stats() -> Result<Value, String> {
    with_index(|db| db.stats())
}

#[tauri::command]
async fn refresh(state: tauri::State<'_, AppState>, app: tauri::AppHandle) -> Result<Value, String> {
    let mut provs = state.providers.lock().map_err(|e| e.to_string())?;
    let db = IndexDb::open(&index_path()).map_err(|e| e.to_string())?;
    let r = scan::build_index(&mut provs, &db, false, |agent, skipped, msgs| {
        use tauri::Emitter;
        let payload = if skipped {
            json!({ "kind": "progress", "skipped": true, "agent": agent })
        } else {
            json!({ "kind": "progress", "skipped": false, "agent": agent, "messages": msgs })
        };
        let _ = app.emit("asv://progress", payload);
    }, |kind, text| {
        use tauri::Emitter;
        let _ = app.emit("asv://progress", json!({ "kind": kind, "text": text }));
    })
    .map_err(|e| e.to_string())?;
    let st = db.stats();
    Ok(json!({
        "sessions": r.sessions, "indexed": r.indexed, "skipped": r.skipped,
        "failed": r.failed, "pruned": r.pruned, "elapsedMs": r.elapsed_ms,
        "stats": st,
    }))
}

#[tauri::command]
async fn list(agent: Option<String>, order: Option<String>, dir: Option<String>, limit: Option<i64>) -> Result<Value, String> {
    with_index(|db| {
        let opts = ListOpts {
            agent,
            project: None,
            limit: limit.unwrap_or(0),
            offset: 0,
            order: order.unwrap_or_else(|| "updated".into()),
            dir: dir.unwrap_or_else(|| "desc".into()),
        };
        let rows = db.list_sessions(&opts);
        json!({ "rows": rows, "total": db.stats()["sessions"] })
    })
}

#[tauri::command]
async fn search(query: String, agent: Option<String>, limit: Option<i64>) -> Result<Value, String> {
    with_index(|db| db.search(&query, agent.as_deref(), None, None, limit.unwrap_or(200)))
}

#[tauri::command]
async fn show(uid: String) -> Result<Option<Value>, String> {
    with_index(|db| db.get_session(&uid).map(|(s, m)| json!({ "session": s, "messages": m })))
}

/// 在项目目录打开终端（claude/codex 续接命令由前端拼好传进来）。
#[tauri::command]
async fn open_terminal(dir: String, command: Option<String>) -> Result<Value, String> {
    use std::process::{Command, Stdio};
    if !std::path::Path::new(&dir).exists() {
        return Ok(json!({ "ok": false, "error": format!("目录不存在：{dir}") }));
    }
    let cmdline = match &command {
        Some(c) if !c.is_empty() => format!("cd /d \"{dir}\" && {c}"),
        _ => format!("cd /d \"{dir}\""),
    };
    // 优先 Windows Terminal
    let wt = Command::new("wt.exe")
        .args(["-d", &dir, "cmd.exe", "/k", command.as_deref().unwrap_or("")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match wt {
        Ok(_) => Ok(json!({ "ok": true, "via": "wt" })),
        Err(_) => {
            let via_cmd = Command::new("cmd.exe")
                .args(["/c", "start", "", "cmd.exe", "/k", &cmdline])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match via_cmd {
                Ok(_) => Ok(json!({ "ok": true, "via": "cmd" })),
                Err(e) => Ok(json!({ "ok": false, "error": e.to_string() })),
            }
        }
    }
}

fn main() {
    tauri::Builder::default()
        .manage(AppState { providers: Mutex::new(providers::all()) })
        .invoke_handler(tauri::generate_handler![agents, stats, refresh, list, search, show, open_terminal])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
