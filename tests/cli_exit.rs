//! 命令行退出码约定的端到端回归：起真实的二进制，看它退几。

// Integration tests panic on purpose; the workspace lints deny that only in production code.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "assertions in tests"
)]

use std::{fs, process::Command};

/// An empty data dir, so the command sees no saved session and never touches the network.
fn empty_data_dir(tag: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "pace-forge-exit-{tag}-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>(),
    ));
    fs::create_dir(&directory).unwrap();
    directory
}

fn run(args: &[&str], data_dir: &std::path::Path) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_pace-forge"))
        .args(args)
        .env("PACE_FORGE_DATA_DIR", data_dir)
        .output()
        .unwrap();
    (
        output.status.code().expect("terminated by signal"),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Missing login is the `auth` category (4), not the catch-all 1, so scripts know to re-login.
#[test]
fn not_logged_in_exits_with_the_auth_code() {
    let (code, stderr) = run(&["semester"], &empty_data_dir("auth"));
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("未登录"), "{stderr}");
}

/// A usage error keeps the conventional 2.
#[test]
fn usage_error_exits_with_two() {
    let (code, stderr) = run(&["no-such-subcommand"], &empty_data_dir("usage"));
    assert_eq!(code, 2, "{stderr}");
}
