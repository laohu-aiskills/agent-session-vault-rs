//! 受限 Provider：本机存在但没有可解析的会话正文。
//!
//! 仍然注册是为了让 `asv agents` 明确回答「这家为什么扫不到」，
//! 而不是静默漏掉让人以为是工具 bug。

use std::path::PathBuf;

use crate::model::{Message, Session};
use crate::util;

pub struct RestrictedProvider {
    pub id: String,
    pub label: String,
    pub root: PathBuf,
    pub reason: String,
}

impl RestrictedProvider {
    pub fn new(id: &str, label: &str, probe_path: &str, reason: &str) -> Self {
        RestrictedProvider {
            id: id.into(),
            label: label.into(),
            root: util::expand_home(probe_path),
            reason: reason.into(),
        }
    }

    /// (available, root, reason, installed)
    pub fn detect(&self) -> (bool, String, String, bool) {
        if !self.root.is_dir() {
            return (false, self.root.to_string_lossy().to_string(), format!("目录不存在：{}", self.root.display()), false);
        }
        (false, self.root.to_string_lossy().to_string(), self.reason.clone(), true)
    }

    pub fn discover(&self) -> Vec<Session> {
        Vec::new()
    }

    pub fn parse(&self, _session: &Session) -> (Vec<Message>, Option<String>) {
        (Vec::new(), None)
    }
}
