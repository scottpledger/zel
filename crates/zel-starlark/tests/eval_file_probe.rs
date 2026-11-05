//! `ZEL_EVAL_FILE=<path.bzl> cargo test -p zel-starlark --test eval_file_probe -- --nocapture`
//! evaluates a .bzl with its load() statements removed and loaded symbols bound to None; reports
//! the first evaluation error. Debug aid for engine conformance.
use zel_starlark::EvalContext;

#[test]
fn eval_file() {
    let Ok(path) = std::env::var("ZEL_EVAL_FILE") else { return };
    let src = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    let mut names = vec![];
    let mut depth = 0i32;
    let mut in_load = false;
    for line in src.lines() {
        if !in_load && line.starts_with("load(") {
            in_load = true;
            depth = 0;
        }
        if in_load {
            for ch in line.chars() {
                if ch == '(' { depth += 1 }
                if ch == ')' { depth -= 1 }
            }
            // collect identifiers before '=' or quoted names
            let t = line.trim();
            if let Some((l, _)) = t.split_once('=') {
                let n = l.trim().trim_start_matches("load(").trim();
                if !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_') { names.push(n.to_string()); }
            } else if t.starts_with('"') || t.starts_with("load(") {
                for q in t.split('"').skip(1).step_by(2).skip(1) { names.push(q.to_string()); }
            }
            if depth <= 0 { in_load = false; }
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    let mut pre = String::new();
    for n in &names { pre.push_str(&format!("{n} = struct()\n")); }
    let mut c = EvalContext::default();
    c.file_name = path.clone();
    match c.eval_str(&format!("{pre}{out}")) {
        Ok(()) => eprintln!("EVAL OK"),
        Err(e) => eprintln!("EVAL ERR {}", e.0),
    }
}
