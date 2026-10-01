//! With the `embedded-dashboard` feature, compile `dashboard/dist` into the
//! binary so a release is one file whose dashboard can't drift from its API.
//!
//! Generated here rather than with a crate like `include_dir` because
//! `include_bytes!` is what makes cargo rebuild when a file changes; the
//! `rerun-if-changed` on the directory catches files added or removed.

use std::fmt::Write;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_DASHBOARD").is_none() {
        return;
    }

    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").is_file() {
        panic!("embedded-dashboard needs a built dashboard at {}. Run `bun install && bun run build` in dashboard/ first.", dist.display());
    }

    let mut files = Vec::new();
    walk(&dist, &mut files);
    files.sort();

    let mut source = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for file in &files {
        let relative = file.strip_prefix(&dist).expect("inside dist").to_str().expect("dashboard file names are UTF-8").replace('\\', "/");
        writeln!(source, "    ({relative:?}, include_bytes!({:?})),", file.display().to_string()).expect("write");
    }
    source.push_str("];\n");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("dashboard_files.rs");
    std::fs::write(out, source).expect("write dashboard_files.rs");
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dashboard/dist") {
        let path = entry.expect("read dashboard/dist").path();
        if path.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}
