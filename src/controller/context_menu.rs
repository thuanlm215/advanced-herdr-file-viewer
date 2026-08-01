//! In-memory state and draw model for the Context Menu popup modal (`Space` or right-click).
//!
//! Read-only w.r.t. files and git (AC-N1, AC-N2): the menu lists actions available on the selected
//! tree node. The compact menu intentionally exposes only the four most-used actions; the hidden
//! editor/app/reveal actions remain available through their global keybindings.

use super::{Drag, Effects, Modal};
use crate::intent::Intent;
use crate::presenter::{ContextMenuItemRowView, ContextMenuView};
use crate::tree::NodeKind;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

#[cfg(windows)]
fn viewer_pane_command() -> std::io::Result<String> {
    let executable = std::env::current_exe()?;
    let executable = executable.to_string_lossy();
    // Windows cannot run the manifest's relative pane command, so its launcher keeps the
    // absolute-path `pane run` workaround documented in scripts/open-file-viewer.ps1.
    Ok(format!(r#"& \"{}\""#, executable.replace('"', "`\"")))
}

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
                    intent: Intent::OpenPaneHere,
                    label: "Open pane here",
                    shortcut: "G",
                },
                ContextMenuItem {
                    intent: Intent::CopyAbsPath,
                    label: "Copy absolute path",
                    shortcut: "Y",
                },
                ContextMenuItem {
                    intent: Intent::CopyRepoPath,
                    label: "Copy relative path",
                    shortcut: "y",
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
                    intent: Intent::OpenWorkspace,
                    label: "Open workspace here",
                    shortcut: "s",
                },
                ContextMenuItem {
                    intent: Intent::OpenPaneHere,
                    label: "Open pane here",
                    shortcut: "G",
                },
                ContextMenuItem {
                    intent: Intent::CopyAbsPath,
                    label: "Copy absolute path",
                    shortcut: "Y",
                },
                ContextMenuItem {
                    intent: Intent::CopyRepoPath,
                    label: "Copy relative path",
                    shortcut: "y",
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

    /// The one-based number shown beside each menu row. Only 1–9 are addressable; an absent
    /// number deliberately does nothing so a stray digit cannot close the modal.
    pub fn intent_for_number(&self, number: char) -> Option<Intent> {
        let idx = number.to_digit(10)?.checked_sub(1)? as usize;
        self.items.get(idx).map(|item| item.intent)
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
        let dir_str = dir.to_string_lossy().to_string();
        let workspace_label = dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&dir_str)
            .to_string();
        let display = crate::text_layout::sanitize_control(&dir_str);
        if let Some(herdr) = self.herdr.as_ref() {
            // Verified against herdr 0.7.5:
            // `workspace create --cwd <PATH> --label <FOLDER> --focus` returns a workspace id and
            // `pane list --workspace <ID>` identifies its initial terminal.
            // Target every following command by those ids; never use `--current` while the host
            // is still applying the workspace focus change.
            match herdr.run_json(&[
                "workspace",
                "create",
                "--cwd",
                &dir_str,
                "--label",
                &workspace_label,
                "--focus",
            ]) {
                Ok(create_reply) => {
                    if !self.open_workspace_with_viewer {
                        self.action_notice = Some(format!(
                            "Opened workspace in {display}; terminal stays focused"
                        ));
                        return Effects::redraw();
                    }
                    let Some(workspace_id) = crate::launch::created_workspace_id(&create_reply)
                    else {
                        self.action_notice = Some(format!(
                            "Opened workspace in {display}, but herdr did not return its workspace ID"
                        ));
                        return Effects::redraw();
                    };
                    let initial_pane = herdr
                        .run_json(&["pane", "list", "--workspace", &workspace_id])
                        .ok()
                        .and_then(|reply| crate::launch::sole_pane_id(&reply));
                    let Some(initial_pane) = initial_pane else {
                        self.action_notice = Some(format!(
                            "Opened workspace in {display}, but could not identify its initial terminal"
                        ));
                        return Effects::redraw();
                    };

                    #[cfg(not(windows))]
                    {
                        // `plugin pane open` launches the manifest entry directly, so there is no
                        // shell-readiness race. Herdr 0.7.5 rejects combining `--workspace` with
                        // a split's `--target-pane`; it also resolves the manifest's relative
                        // executable against `--cwd`, so passing the selected directory makes the
                        // binary disappear. The focused target pane supplies the viewer's launch
                        // context/root while Herdr keeps the process cwd at the plugin root.
                        // After Herdr's fixed 1:1 split, resize the known original terminal in the
                        // direction/amount derived from the validated config ratio. Live Herdr
                        // 0.7.5 verification: right grows the original terminal (viewer < 1/2);
                        // left shrinks it (viewer > 1/2); exactly 1/2 needs no resize.
                        let exact_root_env = format!("{}={dir_str}", crate::host::EXACT_ROOT_ENV);
                        let mut open_args = vec![
                            "plugin".to_string(),
                            "pane".to_string(),
                            "open".to_string(),
                            "--plugin".to_string(),
                            "advanced-herdr-file-viewer".to_string(),
                            "--entrypoint".to_string(),
                            "file-viewer".to_string(),
                            "--placement".to_string(),
                            "split".to_string(),
                            "--target-pane".to_string(),
                            initial_pane.clone(),
                            "--direction".to_string(),
                            "right".to_string(),
                            "--env".to_string(),
                            exact_root_env,
                            "--no-focus".to_string(),
                        ];
                        if let Ok(config_dir) = std::env::var("HERDR_PLUGIN_CONFIG_DIR") {
                            open_args.extend([
                                "--env".to_string(),
                                format!("HERDR_PLUGIN_CONFIG_DIR={config_dir}"),
                            ]);
                        }
                        let open_refs = open_args.iter().map(String::as_str).collect::<Vec<_>>();
                        let opened = herdr.run(&open_refs);
                        self.action_notice = Some(match opened {
                            Ok(_) => {
                                if let Some((direction, amount)) =
                                    self.viewer_pane_ratio.terminal_resize()
                                {
                                    match herdr.run(&[
                                        "pane",
                                        "resize",
                                        "--pane",
                                        &initial_pane,
                                        "--direction",
                                        direction,
                                        "--amount",
                                        &amount,
                                    ]) {
                                        Ok(_) => format!(
                                            "Opened workspace and File Viewer in {display}; terminal stays focused"
                                        ),
                                        Err(e) => format!(
                                            "Opened workspace and File Viewer in {display}, but failed to set viewer ratio {}: {e}",
                                            self.viewer_pane_ratio.viewer_decimal()
                                        ),
                                    }
                                } else {
                                    format!(
                                        "Opened workspace and File Viewer in {display}; terminal stays focused"
                                    )
                                }
                            }
                            Err(e) => format!(
                                "Opened workspace in {display}, but failed to open File Viewer: {e}"
                            ),
                        });
                    }

                    #[cfg(windows)]
                    {
                        let viewer_command = match viewer_pane_command() {
                            Ok(command) => command,
                            Err(e) => {
                                self.action_notice = Some(format!(
                                    "Failed to prepare File Viewer for the new workspace: {e}"
                                ));
                                return Effects::redraw();
                            }
                        };
                        let mut split_args = vec![
                            "pane".to_string(),
                            "split".to_string(),
                            "--pane".to_string(),
                            initial_pane,
                            "--direction".to_string(),
                            "right".to_string(),
                            "--ratio".to_string(),
                            self.viewer_pane_ratio.terminal_decimal(),
                            "--cwd".to_string(),
                            dir_str.clone(),
                            "--no-focus".to_string(),
                            "--env".to_string(),
                            format!("{}={dir_str}", crate::host::EXACT_ROOT_ENV),
                        ];
                        if let Ok(config_dir) = std::env::var("HERDR_PLUGIN_CONFIG_DIR") {
                            split_args.extend([
                                "--env".to_string(),
                                format!("HERDR_PLUGIN_CONFIG_DIR={config_dir}"),
                            ]);
                        }
                        let split_refs = split_args.iter().map(String::as_str).collect::<Vec<_>>();
                        match herdr.run_json(&split_refs) {
                            Ok(reply) => match crate::launch::opened_pane_id(&reply) {
                                Some(pane_id) => {
                                    match herdr.run(&["pane", "run", &pane_id, &viewer_command]) {
                                        Ok(_) => {
                                            self.action_notice = Some(
                                                match herdr
                                                    .run(&["pane", "rename", &pane_id, "Files"])
                                                {
                                                    Ok(_) => format!(
                                                        "Opened workspace and File Viewer in {display}; terminal stays focused"
                                                    ),
                                                    Err(e) => format!(
                                                        "Opened workspace and File Viewer in {display}; terminal stays focused, but could not name the pane: {e}"
                                                    ),
                                                },
                                            );
                                        }
                                        Err(e) => {
                                            let _ = herdr.run(&["pane", "close", &pane_id]);
                                            self.action_notice = Some(format!(
                                                "Opened workspace in {display}, but failed to run File Viewer; rolled back its split: {e}"
                                            ))
                                        }
                                    }
                                }
                                None => {
                                    self.action_notice = Some(format!(
                                        "Opened workspace in {display}, but herdr did not return a valid File Viewer pane"
                                    ))
                                }
                            },
                            Err(e) => {
                                self.action_notice = Some(format!(
                                    "Opened workspace in {display}, but failed to split for File Viewer: {e}"
                                ))
                            }
                        }
                    }
                }
                Err(e) => {
                    self.action_notice = Some(format!("Failed to open workspace: {e}"));
                }
            }
        } else {
            self.action_notice = Some("Failed to open workspace: herdr is unavailable".to_string());
        }
        Effects::redraw()
    }

    pub(crate) fn open_pane_here(&mut self) -> Effects {
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
            // Verified against herdr 0.7.5:
            // `herdr pane split --current --direction down --cwd <PATH> --focus`.
            match herdr.run(&[
                "pane",
                "split",
                "--current",
                "--direction",
                "down",
                "--cwd",
                &dir_str,
                "--focus",
            ]) {
                Ok(_) => {
                    self.action_notice = Some(format!("Opened pane in {display}"));
                }
                Err(e) => {
                    self.action_notice = Some(format!("Failed to open pane: {e}"));
                }
            }
        } else {
            self.action_notice = Some("Failed to open pane: herdr is unavailable".to_string());
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
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(intent) = menu.intent_for_number(c) {
                    self.modal = Modal::None;
                    self.handle(intent)
                } else {
                    Effects::noop()
                }
            }
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
        assert_eq!(
            menu.items
                .iter()
                .map(|item| item.intent)
                .collect::<Vec<_>>(),
            vec![
                Intent::OpenWorkspace,
                Intent::OpenPaneHere,
                Intent::CopyAbsPath,
                Intent::CopyRepoPath,
            ]
        );
        assert_eq!(menu.selected_intent(), Some(Intent::OpenWorkspace));
        menu.navigate(1);
        assert_eq!(menu.selected_intent(), Some(Intent::OpenPaneHere));
        menu.navigate(1);
        assert_eq!(menu.selected_intent(), Some(Intent::CopyAbsPath));
        menu.navigate(-2);
        assert_eq!(menu.selected_intent(), Some(Intent::OpenWorkspace));
        menu.navigate(-1);
        assert_eq!(menu.selected_intent(), Some(Intent::CopyRepoPath));
    }

    #[test]
    fn file_menu_keeps_only_the_same_four_common_actions() {
        let menu = ContextMenuState::for_file((5, 5));
        assert_eq!(
            menu.items
                .iter()
                .map(|item| item.intent)
                .collect::<Vec<_>>(),
            vec![
                Intent::OpenWorkspace,
                Intent::OpenPaneHere,
                Intent::CopyAbsPath,
                Intent::CopyRepoPath,
            ]
        );
        assert_eq!(menu.selected_intent(), Some(Intent::OpenWorkspace));
    }

    #[test]
    fn one_based_number_selects_the_matching_action_without_a_zero_or_overflow_fallback() {
        let menu = ContextMenuState::for_dir((5, 5));
        assert_eq!(menu.intent_for_number('1'), Some(Intent::OpenWorkspace));
        assert_eq!(menu.intent_for_number('2'), Some(Intent::OpenPaneHere));
        assert_eq!(menu.intent_for_number('3'), Some(Intent::CopyAbsPath));
        assert_eq!(menu.intent_for_number('4'), Some(Intent::CopyRepoPath));
        assert_eq!(menu.intent_for_number('0'), None);
        assert_eq!(menu.intent_for_number('5'), None);
    }
}
