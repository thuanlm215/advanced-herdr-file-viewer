//! In-memory state and draw model for the Context Menu popup modal (`m` or right-click).
//!
//! Read-only w.r.t. files and git (AC-N1, AC-N2): the menu lists actions available on the selected
//! tree node (e.g. open workspace, reveal in file manager, copy path). Selecting an action fires its
//! corresponding [`Intent`] and closes the overlay.

use super::{Drag, Effects, Modal};
use crate::intent::Intent;
use crate::presenter::{ContextMenuItemRowView, ContextMenuView};
use crate::tree::NodeKind;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

/// One item in the context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextMenuItem {
    pub intent: Intent,
    pub label: &'static str,
    pub shortcut: &'static str,
}

/// The session state of the open context menu modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextMenuState {
    pub items: Vec<ContextMenuItem>,
    pub cursor: usize,
    pub anchor: (u16, u16),
}

impl ContextMenuState {
    /// Build the context menu for a directory node.
    pub fn for_dir(anchor: (u16, u16)) -> Self {
        Self {
            items: vec![
                ContextMenuItem {
                    intent: Intent::OpenWorkspace,
                    label: "Open workspace here",
                    shortcut: "s",
                },
                ContextMenuItem {
                    intent: Intent::RevealInFileManager,
                    label: "Reveal in file manager",
                    shortcut: "R",
                },
                ContextMenuItem {
                    intent: Intent::CopyRepoPath,
                    label: "Copy relative path",
                    shortcut: "y",
                },
                ContextMenuItem {
                    intent: Intent::CopyAbsPath,
                    label: "Copy absolute path",
                    shortcut: "Y",
                },
            ],
            cursor: 0,
            anchor,
        }
    }

    /// Build the context menu for a file node.
    pub fn for_file(anchor: (u16, u16)) -> Self {
        Self {
            items: vec![
                ContextMenuItem {
                    intent: Intent::OpenInEditor,
                    label: "Open in editor",
                    shortcut: "e",
                },
                ContextMenuItem {
                    intent: Intent::OpenWithApp,
                    label: "Open with app",
                    shortcut: "O",
                },
                ContextMenuItem {
                    intent: Intent::RevealInFileManager,
                    label: "Reveal in file manager",
                    shortcut: "R",
                },
                ContextMenuItem {
                    intent: Intent::OpenWorkspace,
                    label: "Open workspace here",
                    shortcut: "s",
                },
                ContextMenuItem {
                    intent: Intent::CopyRepoPath,
                    label: "Copy relative path",
                    shortcut: "y",
                },
                ContextMenuItem {
                    intent: Intent::CopyAbsPath,
                    label: "Copy absolute path",
                    shortcut: "Y",
                },
            ],
            cursor: 0,
            anchor,
        }
    }

    /// Move the menu cursor up (`delta < 0`) or down (`delta > 0`), wrapping around edges.
    pub fn navigate(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let next = (self.cursor as isize + delta).rem_euclid(len);
        self.cursor = next as usize;
    }

    /// Get the currently selected item's intent.
    pub fn selected_intent(&self) -> Option<Intent> {
        self.items.get(self.cursor).map(|i| i.intent)
    }

    /// Project this controller state into a borrow-free Presenter draw model.
    pub fn to_view(&self) -> ContextMenuView {
        ContextMenuView {
            items: self
                .items
                .iter()
                .map(|i| ContextMenuItemRowView {
                    label: i.label.to_string(),
                    shortcut: i.shortcut.to_string(),
                })
                .collect(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }
}

impl super::Controller {
    pub(super) fn context_menu_view(&self) -> Option<crate::presenter::ContextMenuView> {
        self.modal.context_menu().map(|s| s.to_view())
    }

    pub(crate) fn context_menu_open(&self) -> bool {
        self.modal.context_menu().is_some()
    }

    pub(crate) fn close_context_menu(&mut self) -> Effects {
        if self.modal.context_menu().is_some() {
            self.modal = Modal::None;
            Effects::redraw()
        } else {
            Effects::noop()
        }
    }

    pub(crate) fn show_context_menu(&mut self) -> Effects {
        let row = (self.tree.cursor() as u16).saturating_sub(self.geom.tree_scroll) + 2;
        self.show_context_menu_at(8, row)
    }

    pub(crate) fn show_context_menu_at(&mut self, col: u16, row: u16) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        let menu = if node.kind == NodeKind::Dir {
            ContextMenuState::for_dir((col, row))
        } else {
            ContextMenuState::for_file((col, row))
        };
        self.modal = Modal::ContextMenu(menu);
        Effects::redraw()
    }

    pub(crate) fn open_workspace(&mut self) -> Effects {
        let Some(node) = self.tree.selected() else {
            return Effects::noop();
        };
        let dir = if node.kind == NodeKind::Dir {
            node.path.clone()
        } else {
            node.path.parent().unwrap_or(&node.path).to_path_buf()
        };
        let dir_str = dir.to_string_lossy();
        let display = crate::text_layout::sanitize_control(&dir_str);
        if let Some(herdr) = self.herdr.as_ref() {
            match herdr.run(&["workspace", "create", "--cwd", &dir_str, "--focus"]) {
                Ok(_) => {
                    self.action_notice = Some(format!("Opened workspace in {display}"));
                }
                Err(e) => {
                    self.action_notice = Some(format!("Failed to open workspace: {e}"));
                }
            }
        } else {
            self.action_notice = Some(format!("Opened workspace in {display}"));
        }
        Effects::redraw()
    }

    pub(crate) fn handle_context_menu_key(&mut self, key: KeyEvent) -> Effects {
        let Some(menu) = self.modal.context_menu_mut() else {
            return Effects::noop();
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                menu.navigate(-1);
                Effects::redraw()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                menu.navigate(1);
                Effects::redraw()
            }
            KeyCode::Enter => {
                let intent = menu.selected_intent();
                self.modal = Modal::None;
                if let Some(i) = intent {
                    self.handle(i)
                } else {
                    Effects::redraw()
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.close_context_menu(),
            KeyCode::Char(c) => {
                let shortcut = c.to_string();
                let found = menu
                    .items
                    .iter()
                    .find(|i| i.shortcut == shortcut)
                    .map(|i| i.intent);
                if let Some(intent) = found {
                    self.modal = Modal::None;
                    self.handle(intent)
                } else {
                    self.close_context_menu()
                }
            }
            _ => self.close_context_menu(),
        }
    }

    pub(super) fn handle_context_menu_mouse(&mut self, ev: MouseEvent) -> Effects {
        let Some(menu) = self.modal.context_menu() else {
            return Effects::noop();
        };
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Use the actual drawn rect (clamped by the presenter) so clicks
                // hit the correct row even when the menu was shifted to fit on screen.
                if let Some(rect) = self.geom.context_menu_rect {
                    // Items start after the top border (1 row) + padding (1 row) = 2 rows.
                    let items_y = rect.y + 2;
                    let items_x_end = rect.x + rect.width;
                    if col >= rect.x
                        && col < items_x_end
                        && row >= items_y
                        && row < items_y + menu.items.len() as u16
                    {
                        let idx = (row - items_y) as usize;
                        if let Some(item) = menu.items.get(idx) {
                            let intent = item.intent;
                            self.modal = Modal::None;
                            // Suppress the subsequent MouseUp so it doesn't trigger a click on
                            // the tree/content beneath the now-closed menu.
                            self.drag = Some(Drag::ModalConsumed);
                            return self.handle(intent);
                        }
                    }
                }
                // Click outside menu → close it; suppress the Up as well.
                self.modal = Modal::None;
                self.drag = Some(Drag::ModalConsumed);
                Effects::redraw()
            }
            MouseEventKind::Down(_) => {
                self.modal = Modal::None;
                self.drag = Some(Drag::ModalConsumed);
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_dir_includes_open_workspace_and_navigates_correctly() {
        let mut menu = ContextMenuState::for_dir((10, 5));
        assert_eq!(menu.selected_intent(), Some(Intent::OpenWorkspace));
        menu.navigate(1);
        assert_eq!(menu.selected_intent(), Some(Intent::RevealInFileManager));
        menu.navigate(-1);
        assert_eq!(menu.selected_intent(), Some(Intent::OpenWorkspace));
        menu.navigate(-1);
        assert_eq!(menu.selected_intent(), Some(Intent::CopyAbsPath));
    }

    #[test]
    fn for_file_includes_editor_and_app_options() {
        let menu = ContextMenuState::for_file((5, 5));
        assert_eq!(menu.selected_intent(), Some(Intent::OpenInEditor));
        assert!(
            menu.items.iter().any(|i| i.intent == Intent::OpenWorkspace),
            "file menu must include OpenWorkspace"
        );
    }
}
