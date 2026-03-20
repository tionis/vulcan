use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_mentions_global_flags_and_core_commands() {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    command.arg("--help").assert().success().stdout(
        predicate::str::contains("--vault <VAULT>")
            .and(predicate::str::contains("--output <OUTPUT>"))
            .and(predicate::str::contains("--verbose"))
            .and(predicate::str::contains("scan"))
            .and(predicate::str::contains("doctor")),
    );
}

#[test]
fn scan_stub_returns_clear_error_message() {
    let mut command = Command::cargo_bin("vulcan").expect("binary should build");

    command
        .arg("scan")
        .assert()
        .failure()
        .stderr(predicate::str::contains("scan is not implemented yet"));
}
