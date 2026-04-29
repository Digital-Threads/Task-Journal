use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("task-journal").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("events"))
        .stdout(contains("rebuild-state"));
}
