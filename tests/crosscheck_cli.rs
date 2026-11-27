//! `pace-forge crosscheck` 的命令行回归：本地日切、时区提示和原始行号。

// Integration tests panic on purpose; the workspace lints deny that only in production code.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "assertions in tests"
)]

use std::{fs, process::Command};

#[test]
fn crosscheck_uses_local_calendar_days_and_reports_invalid_timezone() {
    let directory = std::env::temp_dir().join(format!(
        "pace-forge-crosscheck-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>(),
    ));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("records.json");
    // UTC+08:00 places these records at 23:55 and 00:05 on adjacent days.
    fs::write(
        &path,
        r#"[
          {"startTime":1795190100000,"totalDis":1000.9,"totalTime":480},
          {"startTime":1795190700000,"totalDis":1000.9,"totalTime":480}
        ]"#,
    )
    .unwrap();
    let cases = [
        ("28800", "跨 2 天", "UTC+08:00"),
        ("0", "跨 1 天", "UTC+00:00"),
    ];
    let outputs = cases.map(|(offset, _, _)| {
        Command::new(env!("CARGO_BIN_EXE_pace-forge"))
            .arg("crosscheck")
            .arg(&path)
            .env("PACE_TZ_OFFSET_SEC", offset)
            .output()
    });

    // 非法时区偏移：旧实现静默回落默认，现在配置加载直接报错退出（fail-fast）
    let invalid_offsets = ["invalid", "72001"];
    let invalid_outputs = invalid_offsets.map(|offset| {
        Command::new(env!("CARGO_BIN_EXE_pace-forge"))
            .arg("crosscheck")
            .arg(&path)
            .env("PACE_TZ_OFFSET_SEC", offset)
            .output()
            .expect("spawn crosscheck")
    });

    fs::remove_dir_all(directory).unwrap();

    for ((offset, span, timezone), output) in cases.into_iter().zip(outputs) {
        let output = output.unwrap();
        assert!(output.status.success(), "offset {offset}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(span), "offset {offset}: {text}");
        assert!(text.contains(timezone), "offset {offset}: {text}");
        assert!(text.contains("8'00\"/km"), "offset {offset}: {text}");
    }

    for (offset, output) in invalid_offsets.into_iter().zip(invalid_outputs) {
        assert!(
            !output.status.success(),
            "offset {offset} 应当拒绝启动而不是回落默认"
        );
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("配置加载失败"), "offset {offset}: {err}");
        assert!(err.contains("PACE_TZ_OFFSET_SEC"), "offset {offset}: {err}");
    }
}

#[test]
fn crosscheck_reports_original_rows_without_discarding_valid_distance() {
    let directory = std::env::temp_dir().join(format!(
        "pace-forge-crosscheck-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>(),
    ));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("records.json");
    fs::write(
        &path,
        r#"{"list":[
            {"startTime":"NaN","totalDis":1000},
            {"startTime":1795190100000,"totalDis":1000,"points":[[0,0],[0,0.001]]},
            {"startTime":1795190100000,"totalDis":-1},
            {"startTime":1795190700000,"totalDis":2000,"points":[[0,0],[0,0.001]]},
            {"startTime":1795190700000,"totalDis":3000,"points":[[91,0]]}
        ]}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pace-forge"))
        .arg("crosscheck")
        .arg(&path)
        .env("PACE_TZ_OFFSET_SEC", "0")
        .output();
    fs::remove_dir_all(directory).unwrap();

    let output = output.unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("[记录] 3 条"), "{stdout}");
    assert!(stdout.contains("[里程] 均值 2000m"), "{stdout}");
    assert!(stdout.contains("[配速] 无可用记录"), "{stdout}");
    assert!(stdout.contains("[重合] 第 2 条 vs 第 4 条"), "{stdout}");
    assert!(stderr.contains("第 1 条：开始时间"), "{stderr}");
    assert!(stderr.contains("第 3 条：里程"), "{stderr}");
    assert!(stderr.contains("第 5 条：轨迹第 1 点"), "{stderr}");
}
