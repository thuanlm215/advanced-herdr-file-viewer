# advanced-herdr-file-viewer

[![CI](https://github.com/thuanlm215/advanced-herdr-file-viewer/actions/workflows/ci.yml/badge.svg)](https://github.com/thuanlm215/advanced-herdr-file-viewer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust 1.96+](https://img.shields.io/badge/rust-1.96%2B-orange.svg)
![herdr 0.7.5+](https://img.shields.io/badge/herdr-0.7.5%2B-8a2be2)
![platforms: linux • macOS • Windows (preview)](https://img.shields.io/badge/platforms-linux%20%E2%80%A2%20macOS%20%E2%80%A2%20Windows%20(preview)-informational)

A git-aware, read-only file viewer for [herdr](https://herdr.dev). Fork of
[smarzban/herdr-file-viewer](https://github.com/smarzban/herdr-file-viewer): the same tree +
diff / markdown / syntax / **inline image preview** views, plus **workspace search**, **context
actions that open Herdr workspaces and panes**, **file icons**, and **independent tree scrolling**.

![advanced-herdr-file-viewer open in a herdr split beside your work: the directory tree on the left, syntax-highlighted content on the right](assets/File-viewer.png)

*Markdown rendered in the content pane, git status in the tree:*

![advanced-herdr-file-viewer rendering a markdown file: colored headings and styled inline code on the right, the git-status tree on the left](assets/Markdown-view.png)

*Full-screen:*

![advanced-herdr-file-viewer running full-screen](assets/File-Viewer-FS.png)

## Why you'd want it

- **The right view, automatically.** Images preview inline (Kitty/Ghostty); a changed file shows
  its diff; a README renders; code is highlighted. Press `v` only when you want something else.
- **Search the workspace, not just the open file.** `f` fuzzy-finds files; `F` searches text with
  ripgrep. `Tab` switches scope between the whole workspace and the selected folder.
- **Act from the tree.** Right-click or `Space` for a numbered context menu. `s` opens a Herdr
  workspace in the selected directory (viewer in the right fifth); `G` opens a terminal pane
  rooted there.
- **Git in the tree.** `M`/`A`/`D`/`?` on every row, changed-only filter (`c`), working-tree
  status mode (`d`), baseline toggle (`b`). Not a separate git client.
- **Icons you can actually scan.** Unicode by default (Jenkins, Docker, YAML, Terraform, …);
  optional Nerd Font glyphs.
- **Keyboard-first**, mouse-optional. Read-only; text rendering is delegated to `glow` / `delta` /
  `bat`; images use the Kitty graphics protocol (or Unicode halfblocks). See [SECURITY.md](SECURITY.md).

## Highlights

Full tables: [keys](docs/keys.md) · [usage](docs/usage.md).

| Key | Does |
| --- | --- |
| `F` | Full-text search (ripgrep); `Tab` toggles workspace ↔ folder scope |
| `f` | Fuzzy-find a file; same `Tab` scope |
| `s` | Open a Herdr workspace here (viewer in the configured fifth) |
| `G` | Open a focused terminal pane in the selected directory |
| `Space` | Context menu (`1`–`4`); also right-click a tree row |
| `p` | Pin the **viewer pane** so `q`/`Esc` cannot close it |
| `C` | Collapse the whole tree |
| `v` | Cycle the view (diff ⇄ rendered ⇄ syntax) |
| `b` | Flip the diff baseline: merge-base ⇄ `HEAD` |
| `W` | Switch to another git worktree, in place |
| `L` | Copy a `path:line` reference (or the selected lines) |
| `Z` | Full-screen the current file |
| `e` / `O` / `R` | Hand off: editor / OS default app / file manager |
| `?` | Help overlay: keys, what's new, settings, about |

`p` pins the pane, not a second file preview.

## Quick start

```bash
# 1. Install the plugin (prebuilt binary on tagged releases; otherwise builds from source):
herdr plugin install thuanlm215/advanced-herdr-file-viewer

# 2. (recommended) install the renderers:
brew install glow git-delta bat     # macOS, or use your package manager
#   Linux / cross-platform: run scripts/install-renderers.sh from the plugin dir
```

Bind a key in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "advanced-herdr-file-viewer.open-file-viewer"
description = "open file viewer in split"

[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "advanced-herdr-file-viewer.open-file-viewer-tab"
description = "open file viewer in tab"
```

Run `herdr server reload-config`, then press your key. Use `plugin_action` (not a shell `invoke`)
so Herdr injects workspace context.

More: [install](docs/install.md), [summoning](docs/summoning.md), [renderers](docs/renderers.md),
[keys](docs/keys.md).

## Configuration

Copy [`config.example.toml`](config.example.toml) to `config.toml` in the directory printed by
`herdr plugin config-dir advanced-herdr-file-viewer`. Useful keys: `file_icons`,
`open_workspace_with_viewer`, `viewer_pane_ratio`, tree layout, `[keys]`.

Full reference: **[docs/configuration.md](docs/configuration.md)**. Effective values are also in
the `?` overlay **Settings** section.

## Windows

Native Windows is a **preview** (use the `-windows` action ids). WSL needs no extra setup. See
[docs/windows.md](docs/windows.md).

## Documentation

- **[Documentation index](docs/README.md)**
- **[Install & updating](docs/install.md)**
- **[Summoning the viewer](docs/summoning.md)**
- **[Usage guide](docs/usage.md)**
- **[Keys & mouse](docs/keys.md)**
- **[Configuration](docs/configuration.md)**
- **[External renderers](docs/renderers.md)**
- **[Windows (preview)](docs/windows.md)**
- **[Architecture](ARCHITECTURE.md)**
- **[Security](SECURITY.md)**

## Contributing

Bug reports and feature requests: [issues](https://github.com/thuanlm215/advanced-herdr-file-viewer/issues).
How to build and test: [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE) © thuanlm215
