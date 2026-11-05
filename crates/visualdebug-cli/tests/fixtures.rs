//! Golden tests: trace each fixture with the real binary, snapshot the summary.
//! A fixture is skipped (with a note) when its runtime isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).output().is_ok_and(|o| o.status.success())
}

/// Run `visualdebug run <args> -- <cmd>` in `fixture`, then summarize. Returns None if skipped.
fn trace(fixture: &str, name: &str, run_args: &[&str], cmd: &[&str]) -> Option<String> {
    let dir = root().join("fixtures").join(fixture);
    let bin = env!("CARGO_BIN_EXE_visualdebug");
    let name = format!("golden-{name}");
    let out = Command::new(bin)
        .current_dir(&dir)
        .args(["run", "--name", &name])
        .args(run_args)
        .arg("--")
        .args(cmd)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("steps from"), "run failed in {fixture}:\n{stderr}\n{}", String::from_utf8_lossy(&out.stdout));
    let s = Command::new(bin).current_dir(&dir).args(["summarize", &name]).output().unwrap();
    assert!(s.status.success());
    Some(String::from_utf8(s.stdout).unwrap())
}

macro_rules! snap {
    ($name:expr, $text:expr) => {
        insta::with_settings!({filters => vec![
            (r"0x[0-9a-f]{6,16}", "0xADDR"),
            (r"\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d(\.\d+)?( UTC)?( [+-]\d\d:?\d\d)?", "[TIME]"),
            (r"\(\d+\.\dms\)", "(N.Nms)"),
            (r"(?m)…$", "…"),
        ]}, { insta::assert_snapshot!($name, $text) });
    };
}

#[test]
fn ruby_plain_program() {
    if !have("ruby", "-v") {
        return eprintln!("skip: ruby not installed");
    }
    if let Some(s) = trace("ruby-plain", "program", &[], &["ruby", "lib/main.rb"]) {
        snap!("ruby_plain_program", s);
    }
}

#[test]
fn ruby_plain_minitest() {
    if !have("ruby", "-v") {
        return eprintln!("skip: ruby not installed");
    }
    if let Some(s) = trace("ruby-plain", "minitest", &[], &["ruby", "test/inventory_test.rb"]) {
        snap!("ruby_plain_minitest", s);
    }
}

#[test]
fn rails_request_spec() {
    let dir = root().join("fixtures/rails-app");
    let ok = Command::new("bundle").arg("check").current_dir(&dir).output().is_ok_and(|o| o.status.success());
    if !ok {
        return eprintln!("skip: rails fixture bundle not installed (cd fixtures/rails-app && bundle install)");
    }
    let focus = ["--focus", "app/services/pricer.rb", "--focus", "app/controllers/orders_controller.rb"];
    if let Some(s) = trace("rails-app", "request", &focus, &["bundle", "exec", "rspec", "spec/requests/orders_spec.rb"]) {
        snap!("rails_request_spec", s);
    }
    if let Some(s) = trace("rails-app", "start-at", &["--start-at", "Pricer#call"], &["bundle", "exec", "rspec", "spec/services/pricer_spec.rb"]) {
        snap!("rails_pricer_start_at", s);
    }
}
