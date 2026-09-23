# Agent Session Vault — Rust 实现

[Node 版](https://github.com/) 的 Rust 重写：同样的功能，**CLI 1.7MB / GUI 4MB**（Node 版 CLI 88.5MB）。

## 结构

```
crates/
  asv-core/   providers（8 家适配器 + 4 家受限标注）、FTS5 双通道检索、备份还原、迁移导出
  asv-cli/    asv 命令行
  asv-gui/    Tauri 2 桌面界面（系统 WebView2，无 Electron）
  ui/→asv-gui/ui  零构建前端（vanilla HTML/JS）
```

## 构建

```bash
cargo build --release            # CLI + GUI 一次产出
# 产物：target/release/asv.exe（CLI）、target/release/asv-gui.exe（GUI）
cargo test                       # 单元测试
```

依赖：Rust 1.75+（Windows 需 MSVC Build Tools；GUI 需系统 WebView2，Win10/11 自带）。

## CLI

```bash
asv agents                       # 探测本机各家 Agent
asv index [--force] [--agent X]  # 建索引（增量）
asv search "关键词" [--agent X]   # 全文检索，按会话聚合 + 命中数
asv list [--order created] [--dir asc]
asv show <uid> [--full]
asv export <uid> --out x.md|html
asv backup --out DIR [--agent X] [--force]
asv restore DIR [--target HOME] [--apply --yes --overwrite] [--verify]
asv migrate <uid> --to claude-code --out DIR
```

索引库与 Node 版**完全兼容**（同 schema），可互读；备份 manifest 也同格式。

## GUI

双击 `asv-gui.exe`：会话列表（排序/方向/Agent 过滤）、全文检索、详情分批渲染、
增量刷新 + 自动刷新、终端继续（claude/codex 续接 + 权限开关）。
页内查找用系统 Ctrl+F。

## 与 Node 版的差异

- 备份/还原/迁移/导出格式逐字段对齐，两版可互换
- GUI 功能为精简版：会话内查找用 WebView 原生 Ctrl+F；「新更新」标注、页内高亮导航待补
- 解析性能相近（IO 为主），内存占用低一个量级
