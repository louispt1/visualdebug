//! The language probes, embedded in the binary and written to a versioned cache dir at runtime.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct ProbeFile {
    pub path: &'static str,
    pub body: &'static str,
}

pub const PROBES: &[ProbeFile] = &[
    ProbeFile {
        path: "ruby/visualdebug_probe.rb",
        body: include_str!("../probes/ruby/visualdebug_probe.rb"),
    },
    ProbeFile {
        path: "python/sitecustomize.py",
        body: include_str!("../probes/python/sitecustomize.py"),
    },
    ProbeFile {
        path: "python/_visualdebug_probe.py",
        body: include_str!("../probes/python/_visualdebug_probe.py"),
    },
    ProbeFile {
        path: "node/visualdebug_probe.cjs",
        body: include_str!("../probes/node/visualdebug_probe.cjs"),
    },
    ProbeFile {
        path: "node/visualdebug_worker.cjs",
        body: include_str!("../probes/node/visualdebug_worker.cjs"),
    },
];

/// `<cache>/probes/<version>-<content hash>/`, populated if missing. The hash keeps dev builds from
/// running stale probes.
pub fn install() -> Result<PathBuf> {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in PROBES {
        for b in p.path.bytes().chain(p.body.bytes()) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    let base = std::env::var_os("VISUALDEBUG_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            directories::ProjectDirs::from("", "", "visualdebug")
                .map(|d| d.cache_dir().to_path_buf())
        })
        .context("no cache directory available; set VISUALDEBUG_CACHE_DIR")?;
    let dir = base
        .join("probes")
        .join(format!("{}-{:08x}", env!("CARGO_PKG_VERSION"), h as u32));
    if !dir.join(".complete").exists() {
        for p in PROBES {
            let dest = dir.join(p.path);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::write(&dest, p.body).with_context(|| format!("writing {}", dest.display()))?;
        }
        std::fs::write(dir.join(".complete"), "")?;
    }
    Ok(dir)
}

/// Environment that loads every probe into any Ruby, Python or Node process the command starts.
pub fn env(dir: &Path, config: &Path) -> Vec<(String, String)> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let ruby = dir.join("ruby/visualdebug_probe.rb");
    let py = dir.join("python");
    let node = dir.join("node/visualdebug_probe.cjs");
    let mut rubyopt = format!("-r{}", ruby.display());
    if let Some(v) = get("RUBYOPT") {
        rubyopt = format!("{rubyopt} {v}");
    }
    let mut pythonpath = py.display().to_string();
    if let Some(v) = get("PYTHONPATH") {
        pythonpath = format!("{pythonpath}{}{v}", if cfg!(windows) { ";" } else { ":" });
    }
    let mut node_options = format!("--require \"{}\"", node.display());
    if let Some(v) = get("NODE_OPTIONS") {
        node_options = format!("{v} {node_options}");
    }
    vec![
        ("VISUALDEBUG_CONFIG".into(), config.display().to_string()),
        ("RUBYOPT".into(), rubyopt),
        ("PYTHONPATH".into(), pythonpath),
        ("NODE_OPTIONS".into(), node_options),
    ]
}
