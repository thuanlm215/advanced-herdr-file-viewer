mod common;

use common::TempDir;
use herdr_file_viewer::context::LaunchContext;
use herdr_file_viewer::herdr::HerdrCli;
use herdr_file_viewer::resume::{
    RuntimeEnv, ShellKind, load_record, load_record_for_process, register, restore_with,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct FakeHerdr {
    panes: String,
    idle: bool,
    fail_process_info: bool,
    calls: Mutex<Vec<Vec<String>>>,
}

impl FakeHerdr {
    fn new(panes: &str, idle: bool) -> Self {
        Self {
            panes: panes.to_string(),
            idle,
            fail_process_info: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl HerdrCli for FakeHerdr {
    fn run_json(&self, args: &[&str]) -> io::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(args.iter().map(|s| (*s).to_string()).collect());
        match args {
            ["pane", "list"] => Ok(self.panes.clone()),
            ["pane", "process-info", "--pane", _] if self.fail_process_info => {
                Err(io::Error::other("host unavailable"))
            }
            ["pane", "process-info", "--pane", _] => {
                let process_info = if self.idle {
                    // Exact idle-shell shape observed from Herdr 0.8.2: the shell remains the
                    // sole foreground process after session restore and must still be resumable.
                    serde_json::json!({
                        "foreground_process_group_id": 4242,
                        "foreground_processes": [{
                            "argv": ["/bin/bash"],
                            "name": "bash",
                            "pid": 4242
                        }],
                        "shell_pid": 4242
                    })
                } else {
                    serde_json::json!({
                        // A directly launched plugin process is itself Herdr's `shell_pid`; its
                        // executable identity must keep it from looking like an idle shell.
                        "foreground_process_group_id": 4242,
                        "foreground_processes": [{
                            "argv": ["./target/release/advanced-herdr-file-viewer"],
                            "name": "advanced-herdr-file-viewer",
                            "pid": 4242
                        }],
                        "shell_pid": 4242
                    })
                };
                Ok(serde_json::json!({
                    "result": { "process_info": process_info }
                })
                .to_string())
            }
            ["pane", "run", _, _] => Ok("{}".into()),
            _ => Err(io::Error::other("unexpected argv")),
        }
    }
}

fn arm(root: &Path, socket: &str, pane: &str) -> herdr_file_viewer::resume::Registration {
    register(
        RuntimeEnv {
            state_dir: root.join("state"),
            socket_path: socket.into(),
            pane_id: pane.into(),
        },
        LaunchContext {
            cwd: root.join("repo"),
            exact_root: true,
            base_branch: Some("main".into()),
            workspace_id: Some("w1".into()),
        },
        root.join("config/config.toml"),
    )
    .unwrap()
}

fn pane_list(ids: &[&str]) -> String {
    let panes = ids
        .iter()
        .map(|id| format!(r#"{{"pane_id":"{id}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"result":{{"panes":[{panes}]}}}}"#)
}

#[test]
fn restored_pane_with_only_its_shell_in_foreground_is_relaunched_in_place_once() {
    let temp = TempDir::new();
    let registration = arm(temp.path(), "/sessions/default.sock", "w1:p2");
    let record_path = registration.path().to_path_buf();
    let host = FakeHerdr::new(&pane_list(&["w1:p2"]), true);

    let summary = restore_with(
        &host,
        &temp.path().join("state"),
        "/sessions/default.sock",
        Path::new("/plugin path/advanced-herdr-file-viewer"),
        ShellKind::Posix,
    );

    assert_eq!(summary.relaunched, 1);
    assert_eq!(summary.already_running, 0);
    let calls = host.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], ["pane", "list"]);
    assert_eq!(calls[1], ["pane", "process-info", "--pane", "w1:p2"]);
    assert_eq!(calls[2][0..3], ["pane", "run", "w1:p2"]);
    assert_eq!(
        calls[2][3],
        format!(
            "'/plugin path/advanced-herdr-file-viewer' --resume-record '{}'",
            record_path.display()
        )
    );
}

#[test]
fn foreground_process_is_never_overwritten_or_duplicated() {
    let temp = TempDir::new();
    let _registration = arm(temp.path(), "/socket", "w1:p2");
    let host = FakeHerdr::new(&pane_list(&["w1:p2"]), false);

    let summary = restore_with(
        &host,
        &temp.path().join("state"),
        "/socket",
        Path::new("/viewer"),
        ShellKind::Posix,
    );

    assert_eq!(summary.already_running, 1);
    assert_eq!(summary.relaunched, 0);
    assert!(
        !host
            .calls()
            .iter()
            .any(|c| c.get(1).map(String::as_str) == Some("run"))
    );
}

#[test]
fn missing_pane_prunes_its_stale_record() {
    let temp = TempDir::new();
    let registration = arm(temp.path(), "/socket", "w1:p2");
    let record_path = registration.path().to_path_buf();
    let host = FakeHerdr::new(&pane_list(&[]), true);

    let summary = restore_with(
        &host,
        &temp.path().join("state"),
        "/socket",
        Path::new("/viewer"),
        ShellKind::Posix,
    );

    assert_eq!(summary.stale_removed, 1);
    assert!(!record_path.exists());
    assert_eq!(
        host.calls(),
        vec![vec![String::from("pane"), String::from("list")]]
    );
}

#[test]
fn another_session_record_is_ignored_without_a_host_call() {
    let temp = TempDir::new();
    let _registration = arm(temp.path(), "/sessions/other.sock", "w1:p2");
    let host = FakeHerdr::new(&pane_list(&["w1:p2"]), true);

    let summary = restore_with(
        &host,
        &temp.path().join("state"),
        "/sessions/current.sock",
        Path::new("/viewer"),
        ShellKind::Posix,
    );

    assert_eq!(summary, Default::default());
    assert!(host.calls().is_empty());
}

#[test]
fn record_can_only_resume_in_its_own_live_pane() {
    let temp = TempDir::new();
    let registration = arm(temp.path(), "/socket", "w1:p2");
    let path = registration.path();
    let record = load_record(path).unwrap();
    assert_eq!(record.launch_context().cwd, temp.path().join("repo"));
    assert!(record.launch_context().exact_root);

    let ok = |key: &str| match key {
        "HERDR_SOCKET_PATH" => Some("/socket".into()),
        "HERDR_PANE_ID" => Some("w1:p2".into()),
        _ => None,
    };
    assert!(load_record_for_process(path, ok).is_ok());

    let wrong = |key: &str| match key {
        "HERDR_SOCKET_PATH" => Some("/socket".into()),
        "HERDR_PANE_ID" => Some("w1:p9".into()),
        _ => None,
    };
    assert!(load_record_for_process(path, wrong).is_err());
}

#[test]
fn malformed_records_and_process_info_fail_closed() {
    let temp = TempDir::new();
    let state = temp.path().join("state");
    std::fs::create_dir_all(state.join("pane-resume-v1")).unwrap();
    std::fs::write(state.join("pane-resume-v1/bad.json"), "not json").unwrap();
    let _registration = arm(temp.path(), "/socket", "w1:p2");
    let mut host = FakeHerdr::new(&pane_list(&["w1:p2"]), true);
    host.fail_process_info = true;

    let summary = restore_with(
        &host,
        &state,
        "/socket",
        Path::new("/viewer"),
        ShellKind::Posix,
    );
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.relaunched, 0);
}

#[test]
fn explicit_disarm_removes_only_the_registration_record() {
    let temp = TempDir::new();
    let registration = arm(temp.path(), "/socket", "w1:p2");
    let path: PathBuf = registration.path().into();
    assert!(path.exists());
    registration.disarm();
    assert!(!path.exists());
}
