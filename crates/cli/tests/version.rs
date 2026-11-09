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
