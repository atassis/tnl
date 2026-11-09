#![cfg(unix)]

use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct OwnedNode(Child);

impl Drop for OwnedNode {
    fn drop(&mut self) {
        // Node and its fixture children have a dedicated process group.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn run_node(args: &[&str], timeout: Duration) -> (Output, bool) {
    let logs = tempfile::tempdir().expect("fixture log directory");
    let stdout_path = logs.path().join("stdout");
    let stderr_path = logs.path().join("stderr");
    // Files avoid a pipe-buffer deadlock while the parent enforces the deadline.
    let long_tmp = logs.path().join("browser-parent-directory-".repeat(5));
    fs::create_dir(&long_tmp).unwrap();
    let mut command = Command::new("node");
    command
        .args(args)
        .env("TMPDIR", long_tmp)
        .stdout(fs::File::create(&stdout_path).unwrap())
        .stderr(fs::File::create(&stderr_path).unwrap())
        .process_group(0);
    let mut child = OwnedNode(
        command
            .spawn()
            .expect("start explicitly configured node fixture"),
    );
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = 'wait: loop {
        if let Some(status) = child.0.try_wait().expect("poll owned fixture") {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let result = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.0.id())])
                .status()
                .expect("kill timed-out fixture process group");
            assert!(result.success(), "could not kill owned fixture group");
            let reap_deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = child.0.try_wait().expect("reap timed-out fixture") {
                    break 'wait status;
                }
                assert!(
                    Instant::now() < reap_deadline,
                    "owned fixture did not exit after SIGKILL"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: fs::read(stdout_path).unwrap(),
        stderr: fs::read(stderr_path).unwrap(),
    };
    (output, timed_out)
}

#[test]
#[ignore = "requires explicit candidate CLI/daemon, Brave and node"]
fn inspector_refresh_preserves_auth_but_never_replay_confirmation() {
    assert!(
        std::env::var_os("TNL_TEST_CLIENT_BIN").is_some(),
        "explicit candidate CLI required"
    );
    assert!(
        std::env::var_os("TNL_TEST_DAEMON_BIN").is_some(),
        "explicit candidate daemon required"
    );
    assert!(
        std::env::var_os("TNL_TEST_BROWSER_BIN").is_some(),
        "explicit browser required"
    );

    let (_, timed_out) = run_node(
        &["-e", "setInterval(() => {}, 1000)"],
        Duration::from_millis(100),
    );
    assert!(
        timed_out,
        "outer watchdog did not stop a deliberately hung fixture"
    );
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cleanup_tests = fixtures.join("browser_process.test.mjs");
    let (cleanup, expired) = run_node(
        &["--test", cleanup_tests.to_str().unwrap()],
        Duration::from_secs(10),
    );
    assert!(
        !expired && cleanup.status.success(),
        "cleanup regression failed: {}",
        String::from_utf8_lossy(&cleanup.stderr)
    );

    let scenario = fixtures.join("inspector_refresh.mjs");
    let (output, expired) = run_node(&[scenario.to_str().unwrap()], Duration::from_secs(60));
    assert!(
        !expired && output.status.success(),
        "browser fixture failed (deadline exceeded: {expired}):\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

fn run_browser_scenario(name: &str) {
    for variable in [
        "TNL_TEST_CLIENT_BIN",
        "TNL_TEST_DAEMON_BIN",
        "TNL_TEST_BROWSER_BIN",
    ] {
        assert!(
            std::env::var_os(variable).is_some(),
            "explicit {variable} required"
        );
    }
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let (output, expired) = run_node(&[scenario.to_str().unwrap()], Duration::from_secs(60));
    assert!(
        !expired && output.status.success(),
        "{name} failed (deadline exceeded: {expired}):\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
#[ignore = "requires explicit candidate CLI/daemon, Brave and node"]
fn inspector_production_theme_layout_keyboard_and_native_csp() {
    run_browser_scenario("inspector_appearance.mjs");
}

#[test]
#[ignore = "requires explicit candidate CLI/daemon, Brave and node"]
fn inspector_same_port_restart_revokes_old_browser_authority() {
    run_browser_scenario("inspector_restart.mjs");
}
