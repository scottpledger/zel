//! Prints Bazel-style scanner/parser errors for each file argument (used by
//! `scripts/query_harness/syntax/`).
fn main() {
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap();
        for e in zel_starlark::syntax::bazel_syntax::parse_errors("BUILD", &src) {
            println!("{e}");
        }
    }
}
