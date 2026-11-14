#[test]
fn version_flag_prints_llmobs() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_llmobs"))
        .arg("--version")
        .output()
        .expect("run llmobs --version");
    assert!(
        output.status.success(),
        "llmobs --version failed: {output:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("version is utf-8");
    assert!(
        stdout.starts_with("llmobs "),
        "expected version to start with \"llmobs \", got {stdout:?}"
    );
}

#[test]
fn version_command_reports_the_embedded_ui() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_llmobs"))
        .arg("version")
        .output()
        .expect("run llmobs version");
    assert!(output.status.success(), "llmobs version failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).expect("version is utf-8");
    let ui = if llmobs_ui::embedded() {
        "ui: embedded"
    } else {
        "ui: not built"
    };
    assert_eq!(stdout.lines().nth(1), Some(ui), "{stdout:?}");
}
