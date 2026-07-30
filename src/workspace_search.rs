//! Full-text search delegated to ripgrep (`rg`).
//!
//! The viewer owns the interaction and presentation, while `rg` owns filesystem traversal,
//! `.gitignore` handling, binary detection, and matching. Search is read-only and bounded to
//! [`SEARCH_LIMIT`] displayed `file:line` records.

use crate::prompt::PromptInput;
use serde_json::Value;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SEARCH_LIMIT: usize = 500;

/// One displayed ripgrep match record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMatch {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}

/// A completed, bounded search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSearchOutput {
    pub matches: Vec<WorkspaceMatch>,
    /// `true` when at least one additional match existed beyond [`SEARCH_LIMIT`].
    pub limited: bool,
}

/// The external-search seam. Tests inject a fake; production uses [`LiveWorkspaceSearcher`].
pub trait WorkspaceSearcher: Send + Sync {
    /// Whether the required `rg` executable is runnable.
    fn available(&self) -> bool;

    /// Search `scope` under `root`. Implementations should observe `cancel` promptly.
    fn search(
        &self,
        root: &Path,
        scope: &Path,
        query: &str,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<WorkspaceSearchOutput>;
}

/// The real `rg` adapter.
pub struct LiveWorkspaceSearcher {
    program: String,
}

impl Default for LiveWorkspaceSearcher {
    fn default() -> Self {
        Self {
            program: "rg".to_string(),
        }
    }
}

impl LiveWorkspaceSearcher {
    #[cfg(test)]
    fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl WorkspaceSearcher for LiveWorkspaceSearcher {
    fn available(&self) -> bool {
        Command::new(&self.program)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn search(
        &self,
        root: &Path,
        scope: &Path,
        query: &str,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<WorkspaceSearchOutput> {
        let mut child = Command::new(&self.program)
            .current_dir(root)
            .args([
                "--json",
                "--line-number",
                "--column",
                "--smart-case",
                "--fixed-strings",
                "--color=never",
                "--",
            ])
            .arg(query)
            .arg(scope)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("ripgrep stdout unavailable"))?;
        let mut matches = Vec::with_capacity(SEARCH_LIMIT);
        let mut limited = false;

        for line in BufReader::new(stdout).lines() {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "search cancelled",
                ));
            }
            let line = line?;
            let Some(record) = parse_match_record(root, &line) else {
                continue;
            };
            if matches.len() == SEARCH_LIMIT {
                limited = true;
                let _ = child.kill();
                break;
            }
            matches.push(record);
        }

        let status = child.wait()?;
        // ripgrep: 0 = matches, 1 = no matches. A killed child after the 501st record is expected.
        if !status.success() && status.code() != Some(1) && !limited {
            return Err(io::Error::other("ripgrep search failed"));
        }
        Ok(WorkspaceSearchOutput { matches, limited })
    }
}

fn parse_match_record(root: &Path, line: &str) -> Option<WorkspaceMatch> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "match" {
        return None;
    }
    let data = value.get("data")?;
    let raw_path = data.get("path")?.get("text")?.as_str()?;
    let raw = PathBuf::from(raw_path);
    let relative = if raw.is_absolute() {
        raw.strip_prefix(root).unwrap_or(&raw)
    } else {
        raw.as_path()
    };
    let path = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let line_number = data.get("line_number")?.as_u64()? as usize;
    let column = data
        .get("submatches")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("start"))
        .and_then(Value::as_u64)
        .map_or(1, |start| start as usize + 1);
    let preview = data
        .get("lines")?
        .get("text")?
        .as_str()?
        .trim_end_matches(['\r', '\n'])
        .to_string();
    Some(WorkspaceMatch {
        path,
        line: line_number,
        column,
        preview,
    })
}

/// Modal state for the top-anchored full-text search.
pub struct WorkspaceSearchState {
    pub input: PromptInput,
    pub selection_scope: PathBuf,
    pub selection_label: String,
    pub workspace: bool,
    pub matches: Vec<WorkspaceMatch>,
    pub cursor: usize,
    pub pending: bool,
    pub limited: bool,
    pub error: Option<String>,
}

impl WorkspaceSearchState {
    pub fn new(selection_scope: PathBuf, selection_label: String) -> Self {
        Self {
            input: PromptInput::new(),
            selection_scope,
            selection_label,
            workspace: false,
            matches: Vec::new(),
            cursor: 0,
            pending: false,
            limited: false,
            error: None,
        }
    }

    pub fn scope<'a>(&'a self, root: &'a Path) -> &'a Path {
        if self.workspace {
            root
        } else {
            &self.selection_scope
        }
    }

    pub fn scope_label(&self) -> &str {
        if self.workspace {
            "workspace"
        } else {
            &self.selection_label
        }
    }

    pub fn toggle_scope(&mut self) {
        self.workspace = !self.workspace;
        self.cursor = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor =
            (self.cursor as isize + delta).clamp(0, self.matches.len() as isize - 1) as usize;
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.matches.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rg_json_and_normalizes_a_relative_path() {
        let line = r#"{"type":"match","data":{"path":{"text":"docs/usage.md"},"lines":{"text":"hello world\n"},"line_number":7,"absolute_offset":12,"submatches":[{"match":{"text":"world"},"start":6,"end":11}]}}"#;
        assert_eq!(
            parse_match_record(Path::new("/repo"), line),
            Some(WorkspaceMatch {
                path: "docs/usage.md".to_string(),
                line: 7,
                column: 7,
                preview: "hello world".to_string(),
            })
        );
    }

    #[test]
    fn ignores_non_match_and_malformed_records() {
        assert_eq!(
            parse_match_record(Path::new("/repo"), r#"{"type":"begin","data":{}}"#),
            None
        );
        assert_eq!(parse_match_record(Path::new("/repo"), "not json"), None);
    }

    #[test]
    fn scope_toggle_preserves_the_original_selection() {
        let mut state = WorkspaceSearchState::new(PathBuf::from("/repo/docs"), "docs/".to_string());
        assert_eq!(state.scope(Path::new("/repo")), Path::new("/repo/docs"));
        assert_eq!(state.scope_label(), "docs/");
        state.toggle_scope();
        assert_eq!(state.scope(Path::new("/repo")), Path::new("/repo"));
        assert_eq!(state.scope_label(), "workspace");
        state.toggle_scope();
        assert_eq!(state.scope(Path::new("/repo")), Path::new("/repo/docs"));
    }

    #[test]
    fn unavailable_program_is_reported_without_panicking() {
        assert!(!LiveWorkspaceSearcher::with_program("herdr-no-such-rg").available());
    }
}
