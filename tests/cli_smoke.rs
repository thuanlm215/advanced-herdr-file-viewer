//! main wiring smoke test over a real pty (AC-17 launch behavior, AC-20 close).
//! Spawns the *built* binary in a temp dir, asserts it draws the file tree, then presses
//! the close key and asserts a clean exit(0). External renderers (glow/delta/bat) need not
//! be installed — the Content Renderer falls back to plain text, and the tree draws either
//! way.
//!
//! Unix-only: drives the viewer over a real pty via `expectrl`'s unix process backend
//! (`WaitStatus`). `expectrl`'s Windows backend (`conpty`) exposes a materially different API
//! (a raw `u32` exit code, no signal/stop semantics) — porting this pty-driven e2e suite is
//! out of this feature's scope (not named in the windows-support plan); Windows coverage for
//! what these tests exercise is the unit/integration suite plus AC-15..AC-17's reviewer-checked
//! criteria.
#![cfg(unix)]

mod common;

use common::TempDir;
use expectrl::process::unix::WaitStatus;
use expectrl::{Eof, Expect, Session};
use std::process::Command;
use std::time::Duration;

#[test]
fn viewer_draws_a_filename_then_exits_zero_on_close() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("hello.txt"), "hi there\n").unwrap();
    let state_dir = dir.path().join("plugin-state");
    let config_dir = dir.path().join("plugin-config");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_advanced-herdr-file-viewer"));
    cmd.current_dir(dir.path());
    // Hermetic: disable the `git ls-remote` update check (AC-27/hermetic tests) so the smoke
    // test performs no network I/O. See `src/update/mod.rs` DISABLE_ENV — any value disables it.
    cmd.env("HERDR_FILE_VIEWER_NO_UPDATE_CHECK", "1");
    // Mirror a managed plugin pane so this journey also proves that an explicit `q` disarms the
    // tiny restart record. No live herdr is contacted: all values are inert injected identity.
    cmd.env("HERDR_ENV", "1")
        .env("HERDR_PLUGIN_ID", "advanced-herdr-file-viewer")
        .env("HERDR_PLUGIN_ENTRYPOINT_ID", "file-viewer")
        .env("HERDR_PLUGIN_STATE_DIR", &state_dir)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env("HERDR_SOCKET_PATH", dir.path().join("herdr.sock"))
        .env("HERDR_PANE_ID", "w1:p2");

    let mut p = Session::spawn(cmd).expect("spawn the viewer in a pty");
    p.set_expect_timeout(Some(Duration::from_secs(10)));

    // The tree column lists the file in the launch directory (AC-3 display / AC-17 launch).
    p.expect("hello.txt")
        .expect("viewer should draw the file tree");
    let record_dir = state_dir.join("pane-resume-v1");
    assert_eq!(
        std::fs::read_dir(&record_dir).unwrap().count(),
        1,
        "a successfully initialized managed viewer arms one resume record"
    );

    // The close key returns control and exits the process (AC-20).
    p.send("q").expect("send the close key");
    p.expect(Eof)
        .expect("process should terminate after the close key");

    match p.get_process().wait().expect("reap the viewer process") {
        WaitStatus::Exited(_, code) => assert_eq!(code, 0, "AC-20: clean exit on close"),
        other => panic!("expected a clean exit, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_dir(record_dir).unwrap().count(),
        0,
        "an explicit close disarms restart instead of resurrecting the viewer later"
    );
}
