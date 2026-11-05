//! Generates tables of every file under `resources/` (the embedded `@bazel_tools` repository,
//! synced from Bazel's `//tools`) and `builtins/` (`@_builtins`, from `builtins_bzl`) as
//! `include_bytes!` entries. See `scripts/sync_bazel_tools.sh`.
use std::{fs, io::Write, path::Path};

fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let mut ents: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    ents.sort();
    for p in ents {
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

fn table(out: &mut String, name: &str, root: &Path) {
    let mut files = Vec::new();
    if root.exists() {
        walk(root, root, &mut files);
    }
    out.push_str(&format!("pub static {name}: &[(&str, &[u8])] = &[\n"));
    for f in &files {
        let abs = root.join(f);
        out.push_str(&format!("    ({:?}, include_bytes!({:?})),\n", f, abs.to_string_lossy()));
    }
    out.push_str("];\n");
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed=resources");
    println!("cargo:rerun-if-changed=builtins");
    let mut out = String::new();
    table(&mut out, "FILES", &Path::new(&manifest).join("resources"));
    table(&mut out, "BUILTINS", &Path::new(&manifest).join("builtins"));
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("files.rs");
    fs::File::create(dest).unwrap().write_all(out.as_bytes()).unwrap();
}
