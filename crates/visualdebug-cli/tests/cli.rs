use assert_cmd::Command;
use predicates::prelude::*;

fn vd() -> Command {
    Command::cargo_bin("visualdebug").unwrap()
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Gemfile"), "").unwrap();
    std::fs::write(d.path().join(".gitignore"), "log/\n").unwrap();
    d
}

#[test]
fn errors_are_helpful_when_nothing_recorded() {
    let d = project();
    vd().current_dir(d.path())
        .arg("summarize")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nothing recorded yet"));
}

#[test]
fn failing_command_keeps_exit_status_and_still_writes_trace() {
    if std::process::Command::new("ruby")
        .arg("-v")
        .output()
        .is_err()
    {
        return eprintln!("skip: ruby not installed");
    }
    let d = project();
    std::fs::write(
        d.path().join("app.rb"),
        "def f(x)\n  x + 1\nend\nf(1)\nexit 3\n",
    )
    .unwrap();
    vd().current_dir(d.path())
        .args(["run", "--", "ruby", "app.rb"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("added .visualdebug/ to"))
        .stderr(predicate::str::contains("command exited 3"));
    assert!(
        std::fs::read_to_string(d.path().join(".gitignore"))
            .unwrap()
            .contains(".visualdebug/")
    );
    assert!(d.path().join(".visualdebug/traces/app.json").is_file());

    vd().current_dir(d.path())
        .args(["summarize", "app"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("→ Object#f").and(predicate::str::contains("exit 3 (FAILED)")),
        );

    let out = d.path().join("v.html");
    vd().current_dir(d.path())
        .args(["view", "app", "--no-open", "--out"])
        .arg(&out)
        .assert()
        .success();
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.contains("vd-data"));
    for net in [
        "src=\"http",
        "href=\"http",
        "fetch(",
        "XMLHttpRequest",
        "@import",
    ] {
        assert!(
            !html.contains(net),
            "viewer must not reference the network: {net}"
        );
    }

    vd().current_dir(d.path()).arg("clean").assert().success();
    assert!(!d.path().join(".visualdebug/traces").exists());
}
