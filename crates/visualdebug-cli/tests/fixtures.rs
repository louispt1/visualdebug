//! Golden tests: trace each fixture with the real binary, snapshot the summary.
//! A fixture is skipped (with a note) when its runtime isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
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
    assert!(
        stderr.contains("steps from"),
        "run failed in {fixture}:\n{stderr}\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let s = Command::new(bin)
        .current_dir(&dir)
        .args(["summarize", &name])
        .output()
        .unwrap();
    assert!(s.status.success());
    Some(String::from_utf8(s.stdout).unwrap())
}

macro_rules! snap {
    ($name:expr, $text:expr) => {
        insta::with_settings!({filters => vec![
            (r"0x[0-9a-f]{6,16}", "0xADDR"),
            (r"\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d(\.\d+)?( UTC)?( [+-]\d\d:?\d\d)?", "[TIME]"),
            (r"\(\d+\.\dms\)", "(N.Nms)"),
            (r"(?m)^command: (\S*/)?python[0-9.]*", "command: python"),
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
    if let Some(s) = trace(
        "ruby-plain",
        "minitest",
        &[],
        &["ruby", "test/inventory_test.rb"],
    ) {
        snap!("ruby_plain_minitest", s);
    }
}

#[test]
fn rails_request_spec() {
    let dir = root().join("fixtures/rails-app");
    let ok = Command::new("bundle")
        .arg("check")
        .current_dir(&dir)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return eprintln!(
            "skip: rails fixture bundle not installed (cd fixtures/rails-app && bundle install)"
        );
    }
    let focus = [
        "--focus",
        "app/services/pricer.rb",
        "--focus",
        "app/controllers/orders_controller.rb",
    ];
    if let Some(s) = trace(
        "rails-app",
        "request",
        &focus,
        &["bundle", "exec", "rspec", "spec/requests/orders_spec.rb"],
    ) {
        snap!("rails_request_spec", s);
    }
    if let Some(s) = trace(
        "rails-app",
        "start-at",
        &["--start-at", "Pricer#call"],
        &["bundle", "exec", "rspec", "spec/services/pricer_spec.rb"],
    ) {
        snap!("rails_pricer_start_at", s);
    }
}

fn python() -> String {
    std::env::var("VISUALDEBUG_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// Python < 3.12 uses the settrace backend, which legitimately sees less (no C calls): own snapshots.
fn py_snap(py: &str, name: &str) -> String {
    let out = Command::new(py)
        .args(["-c", "import sys; print(sys.version_info >= (3, 12))"])
        .output()
        .unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() == "True" {
        name.to_string()
    } else {
        format!("{name}_settrace")
    }
}

#[test]
fn python_plain_program() {
    let py = python();
    if !have(&py, "-V") {
        return eprintln!("skip: {py} not installed");
    }
    if let Some(s) = trace("python-plain", "program", &[], &[&py, "shop.py"]) {
        snap!(py_snap(&py, "python_plain_program"), s);
    }
}

#[test]
fn python_pytest_auto_test() {
    let py = python();
    let ok = Command::new(&py)
        .args(["-m", "pytest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return eprintln!("skip: pytest not available for {py} (set VISUALDEBUG_TEST_PYTHON)");
    }
    if let Some(s) = trace(
        "python-pytest",
        "pytest",
        &[],
        &[&py, "-m", "pytest", "-q", "tests/test_pricing.py"],
    ) {
        snap!(py_snap(&py, "python_pytest_auto_test"), s);
    }
    if let Some(s) = trace(
        "python-pytest",
        "pytest-start-at",
        &["--start-at", "pricing.raise_if_negative"],
        &[
            &py,
            "-m",
            "pytest",
            "-q",
            "tests/test_pricing.py::test_guest",
        ],
    ) {
        snap!(py_snap(&py, "python_pytest_start_at"), s);
    }
}

/// V8 versions differ by a pause or two (e.g. loop heads), so exact snapshots are kept per Node major
/// version; on a version without one, the summary must still contain `must`.
fn node_check(name: &str, text: String, must: &[&str]) {
    let out = Command::new("node")
        .arg("-p")
        .arg("process.versions.node.split('.')[0]")
        .output()
        .unwrap();
    let major = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let snap_name = format!("{name}_node{major}");
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/snapshots/fixtures__{snap_name}.snap"));
    for m in must {
        assert!(text.contains(m), "{name}: summary lacks {m:?}:\n{text}");
    }
    if file.exists() || std::env::var("VISUALDEBUG_NEW_NODE_SNAPSHOTS").is_ok() {
        snap!(snap_name, text);
    } else {
        eprintln!("note: no snapshot for Node {major}; checked structure only");
    }
}

fn npm_installed(fixture: &str) -> bool {
    root()
        .join("fixtures")
        .join(fixture)
        .join("node_modules")
        .is_dir()
}

#[test]
fn node_plain() {
    if !have("node", "-v") {
        return eprintln!("skip: node not installed");
    }
    if let Some(s) = trace(
        "node-plain",
        "plain",
        &["--focus", "src/pricing.js"],
        &["node", "src/main.js"],
    ) {
        node_check(
            "node_plain",
            s,
            &[
                "→ priceOrder",
                "[REDACTED]",
                "loop ×2",
                "← priceOrder returned 108",
            ],
        );
    }
}

#[test]
fn node_jest() {
    if !have("node", "-v") || !npm_installed("node-jest") {
        return eprintln!("skip: node or fixtures/node-jest/node_modules missing (npm ci)");
    }
    if let Some(s) = trace(
        "node-jest",
        "jest",
        &["--focus", "src/pricing.js"],
        &["npx", "jest"],
    ) {
        node_check(
            "node_jest",
            s,
            &["→ priceOrder", "loop ×2", "← priceOrder returned 108"],
        );
    }
}

#[test]
fn node_vitest_ts() {
    if !have("node", "-v") || !npm_installed("node-vitest-ts") {
        return eprintln!("skip: node or fixtures/node-vitest-ts/node_modules missing (npm ci)");
    }
    if let Some(s) = trace(
        "node-vitest-ts",
        "vitest",
        &["--start-at", "priceOrder"],
        &["npx", "vitest", "run"],
    ) {
        node_check(
            "node_vitest_ts",
            s,
            &[
                "pricing.ts:19",
                "→ priceOrder",
                "pricing.ts:25",
                "← priceOrder returned",
            ],
        );
    }
}
