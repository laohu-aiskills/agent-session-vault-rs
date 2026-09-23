//! 工具函数：流式 JSONL、头尾廉价读取、时间戳归一、目录遍历。

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 逐行读取 UTF-8 文件，脏行/非 UTF-8 行容错。cb 返回 false 提前终止。
/// 返回读取的行数。
pub fn each_line<F: FnMut(&str, usize) -> bool>(path: &Path, mut cb: F) -> io::Result<usize> {
    let f = File::open(path)?;
    let mut reader = BufReader::with_capacity(1 << 20, f);
    let mut n = 0usize;
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                n += 1;
                let line = buf.trim_end_matches(['\n', '\r']);
                if line.is_empty() {
                    continue;
                }
                if !cb(line, n) {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// 解析 JSON，失败返回 None（脏尾行容错）。
pub fn try_parse(line: &str) -> Option<serde_json::Value> {
    serde_json::from_str(line).ok()
}

/// 只读文件头部若干条可解析 JSON 对象，命中上限或字节上限即停（大文件必须早停）。
pub fn read_head_objects(path: &Path, max_lines: usize, max_bytes: usize) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let _ = each_line(path, |line, _| {
        bytes += line.len();
        if let Some(v) = try_parse(line) {
            out.push(v);
        }
        out.len() < max_lines && bytes < max_bytes
    });
    out
}

/// 读文件尾部 size 字节内、从后往前的若干条可解析对象。
/// 首行可能是截断的半行，跳过。
pub fn read_tail_objects(path: &Path, max_lines: usize, size: u64) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(mut f) = File::open(path) else { return out };
    let len = match f.metadata() {
        Ok(m) => m.len().min(size),
        Err(_) => return out,
    };
    if f.seek(SeekFrom::End(-(len as i64))).is_err() {
        return out;
    }
    let mut buf = Vec::with_capacity(len as usize);
    if f.read_to_end(&mut buf).is_err() {
        return out;
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.split(['\n', '\r']).filter(|l| !l.trim().is_empty()).collect();
    for line in lines.iter().skip(1).rev() {
        if out.len() >= max_lines {
            break;
        }
        if let Some(v) = try_parse(line.trim()) {
            out.push(v);
        }
    }
    out
}

/// 时间戳归一化为毫秒。兼容：毫秒/秒整数、秒浮点、ISO 字符串。
pub fn to_ms(v: Option<&serde_json::Value>) -> Option<i64> {
    let v = v?;
    match v {
        serde_json::Value::Number(n) => {
            let f = n.as_f64()?;
            if f <= 0.0 || !f.is_finite() {
                return None;
            }
            // < 1e11 视为秒
            Some(if f < 1e11 { (f * 1000.0).round() as i64 } else { f.round() as i64 })
        }
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            if let Ok(n) = t.parse::<f64>() {
                return to_ms(Some(&serde_json::json!(n)));
            }
            chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| d.timestamp_millis())
        }
        _ => None,
    }
}

/// Claude 风格目录分片名尽力还原成路径。该编码不可逆，仅作兜底。
pub fn decode_slug(slug: &str) -> Option<String> {
    if slug.is_empty() {
        return None;
    }
    let s = slug.to_string();
    let s = if s.len() >= 3 && s.as_bytes()[1] == b'-' && s.as_bytes()[2] == b'-' && s.as_bytes()[0].is_ascii_alphabetic() {
        format!("{}:\\{}", &s[0..1], &s[3..])
    } else if s.len() >= 2 && s.as_bytes()[1] == b'-' && s.as_bytes()[0].is_ascii_alphabetic() {
        format!("{}:\\{}", &s[0..1], &s[2..])
    } else {
        s
    };
    Some(s.replace('-', "\\"))
}

/// 递归收集匹配后缀的文件，跳过超深层级。
pub fn walk_files(root: &Path, exts: &[&str], max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if exts.is_empty() || exts.iter().any(|x| name.ends_with(x)) {
            out.push(entry.into_path());
        }
    }
    out.sort();
    out
}

/// 安全列目录，不存在返回空。
pub fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}

pub fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

/// 展开路径里的 ~ 前缀。
pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// 人类可读字节数。
pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i > 0 && v < 10.0 {
        format!("{:.1}{}", v, U[i])
    } else {
        format!("{}{}", v.round() as u64, U[i])
    }
}

/// 本地时间格式化 yyyy-MM-dd HH:mm。
pub fn format_time(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(d) => d.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ms_variants() {
        assert_eq!(to_ms(Some(&serde_json::json!(1789543853895i64))), Some(1789543853895));
        assert_eq!(to_ms(Some(&serde_json::json!(1780296611.9164207))), Some(1780296611916));
        assert_eq!(
            to_ms(Some(&serde_json::json!("2026-04-27T02:10:34.671Z"))),
            chrono::DateTime::parse_from_rfc3339("2026-04-27T02:10:34.671Z").ok().map(|d| d.timestamp_millis())
        );
        assert_eq!(to_ms(None), None);
    }

    #[test]
    fn decode_slug_windows() {
        // 与 Node 版一致：保留原盘符大小写，分隔符还原为反斜杠
        assert_eq!(decode_slug("c--Users-me"), Some("c:\\Users\\me".into()));
        assert_eq!(decode_slug("d--code-xiaoyu"), Some("d:\\code\\xiaoyu".into()));
        assert_eq!(decode_slug(""), None);
    }
}
