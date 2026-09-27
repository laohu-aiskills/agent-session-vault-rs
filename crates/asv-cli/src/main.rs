//! asv — Agent Session Vault 命令行（Rust 版）。

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use asv_core::index_db::{IndexDb, ListOpts};
use asv_core::providers::{self, Provider};
use asv_core::scan;
use asv_core::util;

#[derive(Parser)]
#[command(name = "asv", version, about = "统一读取、检索、备份、还原多种 AI Agent 的本地会话记录（Rust 版）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出本机所有 Agent 及其数据来源状态
    Agents,
    /// 只扫描会话，不建索引
    Scan { #[arg(long)] agent: Option<String> },
    /// 建/更新全文索引（默认增量，--force 全量重建）
    Index {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        agent: Option<String>,
        /// 索引库路径（默认 ~/.agent-session-vault/index.db）
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 查看索引库概览
    Stats {
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 列出会话
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// 每页数量，0 = 全部
        #[arg(long, default_value_t = 30)]
        limit: i64,
        /// updated | created
        #[arg(long, default_value = "updated")]
        order: String,
        /// asc | desc
        #[arg(long, default_value = "desc")]
        dir: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 查看某会话的消息流
    Show {
        uid: String,
        #[arg(long)]
        full: bool,
        #[arg(long, default_value_t = 40)]
        limit: i64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 全文检索（含中文；同一会话聚合为一条）
    Search {
        query: String,
        #[arg(long)]
        agent: Option<String>,
        /// 逗号分隔：text,reasoning,tool_call,tool_result
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: i64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 导出为 Markdown / HTML
    Export {
        uid: String,
        #[arg(long)]
        out: PathBuf,
        /// md | html（默认按扩展名推断）
        #[arg(long)]
        format: Option<String>,
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// 原样备份（含 SQLite 的 wal/shm）
    Backup {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        /// 目标目录已有备份时允许覆盖
        #[arg(long)]
        force: bool,
    },
    /// 还原。默认只演练；--apply 才写入
    Restore {
        dir: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        uid: Option<String>,
        /// 目标家目录（跨机器还原时重映射前缀）
        #[arg(long)]
        target: Option<PathBuf>,
        /// 校验 sha256
        #[arg(long)]
        verify: bool,
        /// 真正写入
        #[arg(long)]
        apply: bool,
        /// 显式确认写入（--apply 时必填）
        #[arg(long)]
        yes: bool,
        /// 允许覆盖已存在文件
        #[arg(long)]
        overwrite: bool,
    },
    /// 跨 Agent 迁移（仅保真文本，详见输出提示）
    Migrate {
        uid: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        out: PathBuf,
        /// 不清洗：连工具调用/推理一起写入（目标未必能解析）
        #[arg(long)]
        force: bool,
        #[arg(long)]
        index: Option<PathBuf>,
    },
}

fn default_index() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-session-vault")
        .join("index.db")
}

fn open_index(path: &Option<PathBuf>) -> anyhow::Result<IndexDb> {
    IndexDb::open(path.as_deref().unwrap_or(&default_index()))
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli.cmd) {
        eprintln!("错误：{e}");
        let mut src = e.source();
        while let Some(s) = src {
            eprintln!("  ↳ {s}");
            src = s.source();
        }
        std::process::exit(1);
    }
}

fn resolve_providers(spec: &Option<String>) -> anyhow::Result<Vec<Provider>> {
    let mut all = providers::all();
    let ids = providers::resolve(spec.as_deref().unwrap_or("all"), &mut all)?;
    Ok(all.into_iter().filter(|p| ids.iter().any(|id| id == p.id())).collect())
}

fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Agents => cmd_agents(),
        Cmd::Scan { agent } => cmd_scan(&mut resolve_providers(&agent)?),
        Cmd::Index { force, agent, index } => cmd_index(force, &agent, &index),
        Cmd::Stats { index } => cmd_stats(&index),
        Cmd::List { agent, project, limit, order, dir, json, index } => {
            cmd_list(agent, project, limit, order, dir, json, &index)
        }
        Cmd::Show { uid, full, limit, json, index } => cmd_show(uid, full, limit, json, &index),
        Cmd::Search { query, agent, kind, limit, json, index } => {
            cmd_search(query, agent, kind, limit, json, &index)
        }
        Cmd::Export { uid, out, format, index } => cmd_export(uid, &out, format.as_deref(), &index),
        Cmd::Backup { out, agent, force } => cmd_backup(out, agent, force),
        Cmd::Restore { dir, agent, uid, target, verify, apply, yes, overwrite } => {
            cmd_restore(&dir, agent, uid, target.as_deref(), verify, apply, yes, overwrite)
        }
        Cmd::Migrate { uid, to, out, force, index } => cmd_migrate(uid, &to, &out, force, &index),
    }
}

fn cmd_agents() -> anyhow::Result<()> {
    let mut all = providers::all();
    let d = scan::discover_all(&mut all);
    println!("本机可用的 Agent 会话来源\n");
    println!("  {:<14}{:<14}{:>8}{:>10}  状态", "标识", "名称", "会话数", "体积");
    println!("  {}", "-".repeat(72));
    for (id, label, e) in &d.agents {
        let mark = if e.available && !e.restricted {
            "可用"
        } else if e.restricted {
            "受限"
        } else {
            "无数据"
        };
        let verified = if providers::is_verified(id) { "" } else { " [未实测]" };
        println!(
            "  {:<14}{:<14}{:>8}{:>10}  {}{}",
            id,
            label,
            e.count,
            util::human_size(e.bytes),
            mark,
            verified
        );
        if !e.available {
            println!("  {:<14}{}", "", e.reason);
        }
    }
    Ok(())
}

fn cmd_scan(providers: &mut [Provider]) -> anyhow::Result<()> {
    let d = scan::discover_all(providers);
    println!("扫描到 {} 个会话\n", d.sessions.len());
    let mut groups: std::collections::BTreeMap<String, (usize, u64)> = Default::default();
    for s in &d.sessions {
        let e = groups.entry(s.agent.clone()).or_default();
        e.0 += 1;
        e.1 += s.bytes;
    }
    for (agent, (n, bytes)) in groups {
        println!("  {agent:<14}{n} 会话  {}", util::human_size(bytes));
    }
    Ok(())
}

fn cmd_index(force: bool, agent: &Option<String>, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let mut provs = resolve_providers(agent)?;
    let db = open_index(index)?;
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let last = std::cell::RefCell::new(String::new());
    let r = scan::build_index(
        &mut provs,
        &db,
        force,
        |agent, skipped, msgs| {
            if !tty {
                return;
            }
            let line = if skipped {
                format!("  索引中 {agent:<14} 跳过(未变更)")
            } else {
                format!("  索引中 {agent:<14} {msgs} 条")
            };
            if line != last.borrow().as_str() {
                eprint!("\r{}", line);
                *last.borrow_mut() = line;
            }
        },
        |kind, text| {
            eprintln!("  [{kind}] {text}");
        },
    )?;
    if tty {
        eprint!("\r{}", " ".repeat(60));
        eprintln!("\r");
    }
    let st = db.stats();
    println!("索引完成");
    println!(
        "  会话 {} 个（新增/更新 {}，未变更跳过 {}，失败 {}{}）",
        r.sessions,
        r.indexed,
        r.skipped,
        r.failed,
        if r.pruned > 0 { format!("，清理失效 {}", r.pruned) } else { String::new() }
    );
    println!("  消息 {} 条，耗时 {:.1}s", st["messages"].as_i64().unwrap_or(0), r.elapsed_ms as f64 / 1000.0);
    println!("  索引库 {}  {}", st["dbFile"].as_str().unwrap_or(""), util::human_size(st["dbSize"].as_u64().unwrap_or(0)));
    Ok(())
}

fn cmd_stats(index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let st = db.stats();
    if st["sessions"].as_i64().unwrap_or(0) == 0 {
        println!("索引库为空，请先运行：asv index");
        return Ok(());
    }
    println!("索引库概览");
    println!("  路径      {}", st["dbFile"].as_str().unwrap_or(""));
    println!("  体积      {}", util::human_size(st["dbSize"].as_u64().unwrap_or(0)));
    println!("  会话      {}", st["sessions"].as_i64().unwrap_or(0));
    println!("  消息      {}", st["messages"].as_i64().unwrap_or(0));
    println!("  建索引于  {}", st["indexedAt"].as_str().unwrap_or("-"));
    println!();
    println!("  {:<16}{:>8}{:>10}", "Agent", "会话", "消息");
    if let Some(arr) = st["byAgent"].as_array() {
        for a in arr {
            println!(
                "  {:<16}{:>8}{:>10}",
                a["agent"].as_str().unwrap_or(""),
                a["n"].as_i64().unwrap_or(0),
                a["m"].as_i64().unwrap_or(0)
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_list(agent: Option<String>, project: Option<String>, limit: i64, order: String, dir: String, json: bool, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let opts = ListOpts {
        agent,
        project,
        limit,
        offset: 0,
        order,
        dir,
    };
    let rows = db.list_sessions(&opts);
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("索引库为空，请先运行：asv index");
        return Ok(());
    }
    println!("最近 {} 个会话\n", rows.len());
    for r in &rows {
        let title = r["title"].as_str().unwrap_or("(无标题)");
        println!("  {:<13} {}", r["agent"].as_str().unwrap_or(""), truncate(title, 44));
        println!(
            "  {:<13} {} 条 · {} · {}",
            "",
            r["msg_count"].as_i64().unwrap_or(0),
            r["updated_at"].as_i64().map(util::format_time).unwrap_or_else(|| "-".into()),
            r["project_path"].as_str().unwrap_or("未知项目")
        );
        println!("  {:<13} {}", "", r["uid"].as_str().unwrap_or(""));
    }
    Ok(())
}

fn cmd_show(uid: String, full: bool, limit: i64, json: bool, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let Some((session, messages)) = db.get_session(&uid) else {
        anyhow::bail!("未找到会话：{uid}（可先运行 asv index，或检查 ID）");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "session": session, "messages": messages }))?);
        return Ok(());
    }
    println!("{}", session["title"].as_str().unwrap_or("(无标题)"));
    println!("  Agent      {}", session["agent"].as_str().unwrap_or(""));
    println!("  会话 ID    {}", session["session_id"].as_str().unwrap_or(""));
    println!("  项目       {}", session["project_path"].as_str().unwrap_or("未知"));
    println!(
        "  创建/更新  {} / {}",
        session["created_at"].as_i64().map(util::format_time).unwrap_or_else(|| "-".into()),
        session["updated_at"].as_i64().map(util::format_time).unwrap_or_else(|| "-".into())
    );
    println!("  消息       {} 条", messages.len());
    println!();

    let shown = if full { messages.len() } else { limit as usize };
    for m in messages.iter().take(shown) {
        let kind = m["kind"].as_str().unwrap_or("");
        let role = m["role"].as_str().unwrap_or("");
        let tag = if kind == "text" { String::new() } else { format!(" [{kind}]") };
        let ts = m["ts"].as_i64().map(util::format_time).unwrap_or_default();
        println!("#{} {role}{tag} {ts}", m["seq"].as_i64().unwrap_or(0));
        if kind == "tool_call" {
            println!("  工具 {}", m["tool_name"].as_str().unwrap_or(""));
        }
        let text = m["text"].as_str().unwrap_or("");
        let clipped = if full { text.to_string() } else if text.chars().count() > 600 { format!("{}…（--full 查看完整内容）", text.chars().take(600).collect::<String>()) } else { text.to_string() };
        for line in clipped.lines() {
            println!("  {line}");
        }
        println!();
    }
    if messages.len() > shown {
        println!("  仅显示前 {shown} 条，共 {} 条。用 --full 查看全部。", messages.len());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_search(query: String, agent: Option<String>, kind: Option<String>, limit: i64, json: bool, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let kinds: Option<Vec<String>> = kind.map(|k| k.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
    let t0 = std::time::Instant::now();
    let r = db.search(&query, agent.as_deref(), None, kinds.as_deref(), limit);
    let ms = t0.elapsed().as_millis();
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    let hits = r["hits"].as_array().cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("未命中：{query}");
        if r["mode"] == "like" {
            println!("  （短查询走全表扫描；如需更精确，请用 3 个字符以上的关键词走索引）");
        }
        return Ok(());
    }
    println!(
        "命中 {} 个会话 · {} 处消息  {ms}ms · {}",
        hits.len(),
        r["total_matches"].as_i64().unwrap_or(0),
        if r["mode"] == "fts" { "索引检索(trigram)" } else { "全表扫描(短查询兜底)" }
    );
    println!();
    for h in &hits {
        let mc = h["match_count"].as_i64().unwrap_or(1);
        println!("{} {}{}", h["agent"].as_str().unwrap_or(""), truncate(h["title"].as_str().unwrap_or("(无标题)"), 46), if mc > 1 { format!("  {mc} 处") } else { String::new() });
        println!(
            "  {} · {} · {}",
            h["role"].as_str().unwrap_or(""),
            h["kind"].as_str().unwrap_or(""),
            h["ts"].as_i64().map(util::format_time).unwrap_or_else(|| "-".into())
        );
        println!("  {}", scan::make_snippet(h["text"].as_str().unwrap_or(""), &query, 70));
        println!("  {}", h["uid"].as_str().unwrap_or(""));
        println!();
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > n {
        format!("{}…", chars[..n].iter().collect::<String>())
    } else {
        s.to_string()
    }
}

fn cmd_export(uid: String, out: &PathBuf, format: Option<&str>, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let Some((session, messages)) = db.get_session(&uid) else {
        anyhow::bail!("未找到会话：{uid}");
    };
    let session: asv_core::model::Session = serde_json::from_value(session)?;
    let messages: Vec<asv_core::model::Message> = messages
        .into_iter()
        .map(|m| serde_json::from_value(m))
        .collect::<Result<_, _>>()?;
    let (file, fmt, bytes) = asv_core::export::export_session(&session, &messages, out, format)?;
    println!("导出成功");
    println!("  文件  {}", file.display());
    println!("  格式  {fmt}  {}", util::human_size(bytes));
    println!("  内容  {} 条消息", messages.len());
    Ok(())
}

fn cmd_backup(out: PathBuf, agent: Option<String>, force: bool) -> anyhow::Result<()> {
    if out.join("manifest.json").exists() && !force {
        anyhow::bail!(
            "目标目录已存在备份：{}\n  如需覆盖请加 --force（旧备份不会被自动删除）",
            out.display()
        );
    }
    let mut provs = resolve_providers(&agent)?;
    let n = std::cell::Cell::new(0u32);
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let r = asv_core::backup::backup(&mut provs, &out, agent.as_deref(), |rel, _size| {
        let c = n.get() + 1;
        n.set(c);
        if tty && c % 10 == 0 {
            eprint!("\r  已复制 {} 个文件…", c);
        }
        let _ = rel;
    });
    let r = match r {
        Ok(r) => r,
        Err(e) => anyhow::bail!(e),
    };
    if tty {
        eprint!("\r{}", " ".repeat(40));
        eprintln!("\r");
    }
    let s = &r.manifest["summary"];
    println!("备份完成");
    println!("  目录  {}", r.out.display());
    println!("  会话  {}", s["sessions"].as_i64().unwrap_or(0));
    println!("  文件  {} 个，{}", s["files"].as_i64().unwrap_or(0), util::human_size(s["bytes"].as_u64().unwrap_or(0)));
    let errors = s["errors"].as_i64().unwrap_or(0);
    if errors > 0 {
        println!("  错误 {errors} 个（详见 manifest.json 的 errors 字段）");
    }
    println!();
    println!("  还原请用：asv restore <备份目录> --apply");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_restore(
    dir: &PathBuf,
    agent: Option<String>,
    uid: Option<String>,
    target: Option<&std::path::Path>,
    verify: bool,
    apply: bool,
    yes: bool,
    overwrite: bool,
) -> anyhow::Result<()> {
    if !apply {
        let p = asv_core::restore::plan_restore(dir, agent.as_deref(), uid.as_deref(), target.map(|p| p.to_str().unwrap_or("")), verify)?;
        println!("还原演练（未写入任何文件）\n");
        println!("  备份创建于  {}", p.manifest["createdAt"].as_i64().map(util::format_time).unwrap_or_else(|| "-".into()));
        println!("  源机器      {}  家目录 {}", p.manifest["host"].as_str().unwrap_or(""), p.src_home);
        if let Some(t) = &p.target {
            println!("  目标家目录  {t}（路径前缀已重映射）");
        }
        println!("  会话        {} 个", p.sessions);
        println!("  文件        {} 个，{}", p.items.len(), util::human_size(p.bytes));
        println!("  将覆盖      {} 个已存在文件", p.conflicts);
        if p.missing > 0 {
            println!("  备份内缺失 {} 个文件", p.missing);
        }
        println!();
        println!("  待写入清单：");
        for it in p.items.iter().take(40) {
            println!("    [{}]  {}", if it.conflict { "覆盖" } else { "新建" }, it.dest.display());
        }
        if p.items.len() > 40 {
            println!("    … 其余 {} 个", p.items.len() - 40);
        }
        println!();
        println!("  以上仅为演练。确认无误后加 --apply 才会真正写入。");
        if p.conflicts > 0 {
            println!("  覆盖已存在文件还需额外加 --overwrite。");
        }
        return Ok(());
    }

    if !yes {
        let p = asv_core::restore::plan_restore(dir, agent.as_deref(), uid.as_deref(), target.map(|p| p.to_str().unwrap_or("")), verify)?;
        println!("即将写入 {} 个文件，其中覆盖 {} 个已存在文件。", p.items.len(), p.conflicts);
        println!("  非交互环境不会自动确认：请显式加 --yes 表示你已确认。");
        println!("  建议先不加 --apply 运行一次，查看完整清单。");
        return Ok(());
    }

    let p = asv_core::restore::plan_restore(dir, agent.as_deref(), uid.as_deref(), target.map(|p| p.to_str().unwrap_or("")), verify)?;
    if p.conflicts > 0 && !overwrite {
        println!("检测到 {} 个已存在文件，未授权覆盖，这些文件将被跳过。", p.conflicts);
        println!("  如需覆盖请加 --overwrite。");
    }
    let result = asv_core::restore::apply_restore(&p, overwrite);
    println!("还原执行完毕");
    println!("  写入  {}", result.written);
    println!("  跳过  {}{}", result.skipped, if result.skipped > 0 { "（冲突未授权覆盖）" } else { "" });
    println!("  失败  {}", result.failed);
    let bad: Vec<_> = result.details.iter().filter(|(s, _)| s != "written" && s != "conflict-skipped").collect();
    for (status, dest) in bad.iter().take(20) {
        println!("    [{status}]  {dest}");
    }
    Ok(())
}

fn cmd_migrate(uid: String, to: &str, out: &PathBuf, force: bool, index: &Option<PathBuf>) -> anyhow::Result<()> {
    let db = open_index(index)?;
    let Some((session, messages)) = db.get_session(&uid) else {
        anyhow::bail!("未找到会话：{uid}（请先 asv index）");
    };
    let session: asv_core::model::Session = serde_json::from_value(session)?;
    let messages: Vec<asv_core::model::Message> = messages
        .into_iter()
        .map(|m| serde_json::from_value(m))
        .collect::<Result<_, _>>()?;

    let r = asv_core::migrate::migrate(&session, &messages, to, out, !force)?;
    if !force {
        let lost = &r.report["lostBreakdown"];
        println!("迁移产出完成");
        println!("  目标  {}（{} 方言）", r.target.label, if r.target.dialect == asv_core::migrate::Dialect::Claude { "claude" } else { "codebuddy" });
        println!("  文件  {}", r.file.display());
        println!("  说明  {}", r.note_file.display());
        println!("  保留  {} 条文本消息", r.wrote_messages);
        println!();
        println!("  ⚠ 内容损失（格式不兼容，无法恢复）：");
        println!(
            "    工具调用 {} 条、工具结果 {} 条、推理过程 {} 条",
            lost["tool_call"].as_i64().unwrap_or(0),
            lost["tool_result"].as_i64().unwrap_or(0),
            lost["reasoning"].as_i64().unwrap_or(0)
        );
        println!("    源会话共 {} 条消息，仅 {} 条被保留", messages.len(), r.report["keptMessages"].as_i64().unwrap_or(0));
        println!();
        println!("  下一步：把产出目录复制到目标 Agent 的项目根目录");
        println!("    {}", r.target.project_root().display());
        println!("  注意：本工具不会改写目标 Agent 的会话索引库，其会话列表可能不会立即显示这条会话。");
        return Ok(());
    }
    println!("迁移产出完成（--force，不做清洗）");
    println!("  文件  {}", r.file.display());
    println!("  已使用 --force：工具调用与推理块被原样写入，目标 Agent 可能无法解析这些块。");
    Ok(())
}
