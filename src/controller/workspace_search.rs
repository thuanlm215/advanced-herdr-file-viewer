//! Ripgrep-backed full-text search modal (`F`).

use super::*;
use std::sync::atomic::Ordering;

impl Controller {
    pub fn set_workspace_searcher(&mut self, searcher: Box<dyn WorkspaceSearcher>) {
        self.workspace_searcher = Some(Arc::from(searcher));
    }

    pub fn workspace_search_open(&self) -> bool {
        self.modal.workspace_search().is_some()
    }

    pub(super) fn workspace_search_view(&self) -> Option<WorkspaceSearchView> {
        let state = self.modal.workspace_search()?;
        Some(WorkspaceSearchView {
            query: state.input.query().to_string(),
            scope_label: state.scope_label().to_string(),
            workspace: state.workspace,
            matches: state.matches.clone(),
            cursor: state.cursor,
            pending: state.pending,
            limited: state.limited,
            error: state.error.clone(),
        })
    }

    pub(super) fn open_text_search(&mut self) -> Effects {
        let Some(searcher) = self.workspace_searcher.as_ref() else {
            self.action_notice =
                Some("Full-text search requires ripgrep (`rg`) on PATH".to_string());
            return Effects::redraw();
        };
        if !searcher.available() {
            self.action_notice =
                Some("Full-text search requires ripgrep (`rg`) on PATH".to_string());
            return Effects::redraw();
        }
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        let label = node
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&node.path)
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        let label = if node.kind == NodeKind::Dir {
            format!("{label}/")
        } else {
            label
        };
        self.modal = Modal::WorkspaceSearch(WorkspaceSearchState::new(node.path, label));
        self.action_notice = None;
        self.last_click = None;
        Effects::redraw()
    }

    fn cancel_workspace_search(&mut self) {
        if let Some(cancel) = self.workspace_search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.workspace_search_rx = None;
    }

    fn dispatch_workspace_search(&mut self) {
        self.cancel_workspace_search();
        let Some(state) = self.modal.workspace_search_mut() else {
            return;
        };
        state.matches.clear();
        state.cursor = 0;
        state.limited = false;
        state.error = None;
        let query = state.input.query().to_string();
        if query.is_empty() {
            state.pending = false;
            return;
        }
        state.pending = true;
        let scope = state.scope(&self.root).to_path_buf();
        let Some(searcher) = self.workspace_searcher.clone() else {
            state.pending = false;
            state.error = Some("ripgrep (`rg`) is unavailable".to_string());
            return;
        };
        self.workspace_search_seq = self.workspace_search_seq.wrapping_add(1);
        let seq = self.workspace_search_seq;
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.workspace_search_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = mpsc::channel();
        self.workspace_search_rx = Some(rx);
        std::thread::spawn(move || {
            let result = searcher.search(&root, &scope, &query, cancel);
            let _ = tx.send((seq, result));
        });
    }

    pub fn handle_workspace_search_key(&mut self, key: KeyEvent) -> Effects {
        if self.modal.workspace_search().is_none() {
            return Effects::noop();
        }
        match key.code {
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.insert(c);
                }
                self.dispatch_workspace_search();
                Effects::redraw()
            }
            KeyCode::Backspace => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.backspace();
                }
                self.dispatch_workspace_search();
                Effects::redraw()
            }
            KeyCode::Delete => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.delete();
                }
                self.dispatch_workspace_search();
                Effects::redraw()
            }
            KeyCode::Left => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.move_left();
                }
                Effects::redraw()
            }
            KeyCode::Right => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.move_right();
                }
                Effects::redraw()
            }
            KeyCode::Home => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.move_home();
                }
                Effects::redraw()
            }
            KeyCode::End => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.input.move_end();
                }
                Effects::redraw()
            }
            KeyCode::Up => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.move_selection(-1);
                }
                Effects::redraw()
            }
            KeyCode::Down => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.move_selection(1);
                }
                Effects::redraw()
            }
            KeyCode::Tab => self.toggle_workspace_search_scope(),
            KeyCode::Enter => self.confirm_workspace_search(),
            KeyCode::Esc => {
                self.cancel_workspace_search();
                self.modal = Modal::None;
                self.last_click = None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    fn toggle_workspace_search_scope(&mut self) -> Effects {
        let Some(state) = self.modal.workspace_search_mut() else {
            return Effects::noop();
        };
        state.toggle_scope();
        self.dispatch_workspace_search();
        Effects::redraw()
    }

    fn confirm_workspace_search(&mut self) -> Effects {
        let Some(hit) = self
            .modal
            .workspace_search()
            .and_then(|state| state.matches.get(state.cursor))
            .cloned()
        else {
            return Effects::noop();
        };
        self.cancel_workspace_search();
        self.modal = Modal::None;
        self.last_click = None;
        let abs = self.root.join(&hit.path);
        if !self.tree.reveal(&abs) {
            self.action_notice = Some(format!("Could not open {}", hit.path));
            return Effects::redraw();
        }
        self.tree_follow_selection = true;
        let tree_changed_only = self.tree.changed_only();
        if !tree_changed_only {
            self.changed_only = false;
            self.status_mode = false;
        } else {
            self.changed_only = !self.status_mode;
        }
        self.hide_hidden = self.tree.hide_hidden();
        self.show_ignored = self.tree.show_ignored();
        self.overrides.insert(abs, ViewMode::SyntaxContent);
        self.dispatch_render();
        self.pending_goto = Some((self.latest_seq, hit.line));
        if self.content_width == 0 {
            self.zoomed = true;
            self.focus = Focus::Content;
        }
        Effects::redraw()
    }

    pub(super) fn handle_workspace_search_mouse(&mut self, ev: MouseEvent) -> Effects {
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        let pos = Position {
            x: ev.column,
            y: ev.row,
        };
        match ev.kind {
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .geom
                    .workspace_search_scope_button
                    .is_some_and(|rect| rect.contains(pos)) =>
            {
                self.toggle_workspace_search_scope()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(rows) = self.geom.workspace_search_rows else {
                    return Effects::noop();
                };
                if !rows.contains(pos) {
                    return Effects::noop();
                }
                let idx =
                    self.geom.workspace_search_scroll as usize + (ev.row - rows.y) as usize / 2;
                if let Some(state) = self.modal.workspace_search_mut()
                    && idx < state.matches.len()
                {
                    state.set_cursor(idx);
                    return Effects::redraw();
                }
                Effects::noop()
            }
            MouseEventKind::ScrollDown => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.move_selection(self.wheel_step);
                }
                Effects::redraw()
            }
            MouseEventKind::ScrollUp => {
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.move_selection(-self.wheel_step);
                }
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    pub(super) fn poll_workspace_search(&mut self) -> bool {
        let Some(rx) = &self.workspace_search_rx else {
            return false;
        };
        let received = match rx.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.workspace_search_rx = None;
                if let Some(state) = self.modal.workspace_search_mut() {
                    state.pending = false;
                    state.error = Some("ripgrep search failed".to_string());
                    return true;
                }
                return false;
            }
            Err(mpsc::TryRecvError::Empty) => None,
        };
        let Some((seq, result)) = received else {
            return false;
        };
        self.workspace_search_rx = None;
        self.workspace_search_cancel = None;
        if seq != self.workspace_search_seq {
            return false;
        }
        let Some(state) = self.modal.workspace_search_mut() else {
            return false;
        };
        state.pending = false;
        match result {
            Ok(output) => {
                state.matches = output.matches;
                state.limited = output.limited;
                state.cursor = 0;
                state.error = None;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return false,
            Err(_) => {
                state.matches.clear();
                state.limited = false;
                state.error = Some("ripgrep search failed".to_string());
            }
        }
        true
    }
}
