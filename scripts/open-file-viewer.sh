#!/usr/bin/env bash
# Idempotent launcher for the file viewer — used by both the `open-file-viewer` action and a
# herdr keybinding (a `[[keys.command]]` with `type = "shell"`). "Launch-or-focus, toggle on
# repeat", scoped to the current workspace:
#   - no Files pane and one pane in this workspace -> open a focused right split at configured width
#   - no Files pane in a multi-pane workspace      -> open a normal focused split (existing behavior)
#   - a Files pane exists but isn't focused  -> focus it
#   - the focused pane IS the Files pane     -> close it ("hide"; herdr has no hide-without-close,
#                                               and reopening just re-walks the tree — cheap)
#
# herdr actions/keybindings run a command (no declarative "open this pane" field), so this shells
# out to the herdr CLI via $HERDR_BIN_PATH (herdr injects it; fall back to `herdr` on PATH).
#
# The OPEN/FOCUS/CLOSE decision is computed in-process by the viewer binary itself
# (`advanced-herdr-file-viewer --launch-decision`, fed the `pane list` JSON on stdin) — so it is unit-
# tested and the pane id it returns is already validated as flag-safe (option-injection guard).
# Any failure (binary missing, parse error, no focused pane) degrades to OPEN, preserving the
# original always-open behavior. herdr has no focus-by-id, so a focus is a `zoom <id> --on/--off`
# cycle: `--on` focuses (and maximizes) the pane, `--off` un-maximizes while keeping it focused.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
viewer_bin="$script_dir/../target/release/advanced-herdr-file-viewer"

# Launcher protocol: `<terminal-ratio> <resize-direction|none> <resize-amount>`. Keep the current
# one-third behavior as a safe fallback if the binary/config helper is unavailable or malformed.
viewer_layout="0.666667 right 0.166667"
if [ -x "$viewer_bin" ]; then
  candidate_layout="$("$viewer_bin" --viewer-pane-layout 2>/dev/null || true)"
  if [[ "$candidate_layout" =~ ^0\.[0-9]{6}\ (right|left|none)\ 0\.[0-9]{6}$ ]]; then
    viewer_layout="$candidate_layout"
  fi
fi
read -r _terminal_ratio resize_direction resize_amount <<< "$viewer_layout"

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin advanced-herdr-file-viewer \
    --entrypoint file-viewer \
    --placement split \
    --direction right \
    --focus
}

open_sized() {
  target_pane="$1"
  open_args=(plugin pane open
    --plugin advanced-herdr-file-viewer
    --entrypoint file-viewer
    --placement split
    --target-pane "$target_pane"
    --direction right
    --focus)
  if [ -n "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
    open_args+=(--env "HERDR_PLUGIN_CONFIG_DIR=$HERDR_PLUGIN_CONFIG_DIR")
  fi
  "$herdr_bin" "${open_args[@]}" || {
    open_pane
    return
  }
  # Herdr 0.7.5 opens plugin splits at 1:1 and exposes no ratio flag. Resize the original terminal
  # from that midpoint using the validated config-derived direction/amount. At exactly 1/2 no
  # resize is needed. Failure keeps the valid viewer at the host default.
  if [ "$resize_direction" != "none" ]; then
    "$herdr_bin" pane resize \
      --pane "$target_pane" \
      --direction "$resize_direction" \
      --amount "$resize_amount" >/dev/null 2>&1 || true
  fi
}

decision="OPEN"
if [ -x "$viewer_bin" ]; then
  panes="$("$herdr_bin" pane list 2>/dev/null || true)"
  if [ -n "$panes" ]; then
    decision="$(printf '%s' "$panes" | "$viewer_bin" --launch-decision 2>/dev/null || echo OPEN)"
  fi
fi

case "$decision" in
  "OPEN_THIRD "*)
    open_sized "${decision#OPEN_THIRD }"
    ;;
  "FOCUS "*)
    pid="${decision#FOCUS }"
    "$herdr_bin" pane zoom "$pid" --on >/dev/null 2>&1 || true
    exec "$herdr_bin" pane zoom "$pid" --off
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_pane
    ;;
esac
