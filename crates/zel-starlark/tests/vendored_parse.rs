//! Parses every vendored rules_* `.bzl`/`BUILD`/`MODULE.bazel` file (lexer, parser, resolver).
use std::path::{Path, PathBuf};
use zel_starlark::{Lexer, Parser, Resolver};

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            if n.ends_with(".bzl") || n == "BUILD" || n == "BUILD.bazel" || n == "MODULE.bazel" {
                out.push(p);
            }
        }
    }
}

#[test]
fn parse_all_vendored() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/bcr");
    if !root.exists() {
        return;
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    // Bazel's own builtins (`@_builtins`), when a checkout is available.
    if let Some(home) = std::env::var_os("HOME") {
        let b = Path::new(&home).join("Development/bazel/src/main/starlark/builtins_bzl");
        if b.exists() {
            walk(&b, &mut files);
        }
    }
    let mut fails = Vec::new();
    for f in &files {
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name = f.to_string_lossy().to_string();
        // Bazel's own scanner/parser (ported in `bazel_syntax`) must accept everything Bazel
        // accepts, and report no structural resolver errors either.
        // (`@_builtins` is parsed with type syntax enabled, which the port does not support.)
        let bazel = if name.contains("builtins_bzl") {
            zel_starlark::syntax::bazel_syntax::ParseOutput::empty()
        } else {
            zel_starlark::syntax::bazel_syntax::parse(&name, &src)
        };
        if let Some(e) = bazel.errors.first() {
            fails.push(format!("bazel_syntax: {e}"));
        } else if let Some(e) = bazel.resolver_errors.first() {
            if !(name.ends_with("protobuf/33.4/conformance/BUILD") && e.error.message.contains("more than once")) {
                fails.push(format!("bazel_syntax resolver: {}", e.error));
            }
        }
        let r = (|| -> Result<(), String> {
            let toks = Lexer::new(&name, &src).lex_all().map_err(|e| e.to_string())?;
            let stmts = Parser::new(toks).parse_module().map_err(|e| e.0)?;
            let mut r = Resolver::new();
            r.allow_late_loads = !name.ends_with(".bzl");
            r.resolve_module(&stmts).map_err(|e| e.0)?;
            Ok(())
        })();
        if let Err(e) = r {
            // protobuf's conformance/BUILD loads a symbol twice; Bazel rejects it as well
            // and it is never evaluated (it is a test-only package).
            if name.ends_with("protobuf/33.4/conformance/BUILD") && e.contains("more than once") {
                continue;
            }
            fails.push(e);
        }
    }
    for f in fails.iter().take(60) {
        eprintln!("{f}");
    }
    eprintln!("parsed {} files, {} failures", files.len(), fails.len());
    assert!(fails.is_empty());
}
