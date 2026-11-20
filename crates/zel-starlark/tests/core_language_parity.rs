#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeMap;
use std::sync::Arc;
use zel_starlark::{
    dotted_version_cmp, parse_dotted_version, start_cpu_profile, stop_cpu_profile, EvalContext,
    Location, StarlarkError, Value,
};

fn eval_ok(src: &str) -> EvalContext {
    let mut ctx = EvalContext::default();
    ctx.strict_names = true;
    ctx.globals.insert(
        "struct".to_string(),
        Value::BuiltinFunction("struct".into()),
    );
    ctx.eval_str(src)
        .expect("expected Starlark eval to succeed");
    ctx
}

fn eval_err(src: &str) -> String {
    let mut ctx = EvalContext::default();
    ctx.strict_names = true;
    ctx.globals.insert(
        "struct".to_string(),
        Value::BuiltinFunction("struct".into()),
    );
    ctx.eval_str(src)
        .expect_err("expected Starlark eval error")
        .0
}

#[test]
fn non_associative_comparisons_rejected_unless_parenthesized() {
    let err = eval_err("x = 1 < 2 < 3");
    assert!(err.contains("not associative"), "got: {err}");
    let ctx = eval_ok("x = (1 < 2) == True\ny = (1 < 2) and (2 < 3)");
    assert_eq!(ctx.globals.get("x"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("y"), Some(&Value::Bool(true)));
}

#[test]
fn string_concat_constant_folding_and_utf16_indexing_slicing() {
    let ctx = eval_ok(
        r#"
s = "a" + "b" + "c"
emoji = "😀"
n = len(emoji)
elems = emoji.elems()
sub = ("x" + emoji + "y")[1:3]
pos = ("a" + emoji + "z").find("z")
h = hash(emoji)
t = "a1b_c".title()
"#,
    );
    assert_eq!(ctx.globals.get("s"), Some(&Value::String("abc".into())));
    assert_eq!(
        ctx.globals.get("n"),
        Some(&Value::Int(zel_starlark::BigInt::from_i64(2)))
    );
    assert_eq!(ctx.globals.get("sub"), Some(&Value::String("😀".into())));
    assert_eq!(
        ctx.globals.get("pos"),
        Some(&Value::Int(zel_starlark::BigInt::from_i64(3)))
    );
    assert_eq!(ctx.globals.get("t"), Some(&Value::String("A1B_C".into())));
    assert!(eval_err(r#""abc".index("z")"#).contains("substring not found"));
    assert!(eval_err(r#""abc".startswith(("a", 1))"#).contains("at index 1 of sub"));
    assert!(eval_err(r#""abc".replace("a", "b", maxsplit = 1)"#).contains("unexpected keyword"));
}

#[test]
fn strings_are_not_iterable_in_loops_or_collection_builtins() {
    assert!(eval_err("x = list('abc')").contains("want 'iterable'"));
    assert!(eval_err("x = tuple('abc')").contains("want 'iterable'"));
    assert!(eval_err("x = set('abc')").contains("want 'iterable'"));
    assert!(eval_err("for c in 'abc':\n    pass").contains("not iterable"));
    assert!(eval_err("x = reversed({1: 2})").contains("want 'sequence'"));
}

#[test]
fn nested_unpacking_targets_and_toplevel_redeclaration() {
    let ctx = eval_ok(
        r#"
arr = [0, 0]
d = {}
[arr[0], d["k"]] = [10, 20]
(single,) = [99]
for arr[1] in [42]:
    pass
"#,
    );
    assert_eq!(
        ctx.globals.get("single"),
        Some(&Value::Int(zel_starlark::BigInt::from_i64(99)))
    );
    let mut bzl = EvalContext::default();
    bzl.file_name = "pkg/defs.bzl".to_string();
    let err = bzl.eval_str("a, a = 1, 2").unwrap_err().0;
    assert!(err.contains("redeclared at top level"), "got: {err}");
}

#[test]
fn private_symbol_load_rejected_outside_builtins() {
    let mut bzl = EvalContext::default();
    bzl.file_name = "pkg/defs.bzl".to_string();
    bzl.static_names = true;
    let errs = bzl
        .eval_file_checked(r#"load("//pkg:helper.bzl", "_secret")"#, None)
        .unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("symbol '_secret' is private")),
        "got: {errs:?}"
    );
}

#[test]
fn range_sequence_equality_slicing_and_lazy_all_any() {
    let ctx = eval_ok(
        r#"
eq1 = range(0, 4, 2) == range(0, 3, 2)
eq2 = range(5, 0) == range(10, 10)
sl = range(0, 10, 2)[1:4]
t = type(sl)
sl_list = list(sl)
a = any(range(1000000000))
b = all(range(1, 100))
c = all(range(0, 1000000000))
"#,
    );
    assert_eq!(ctx.globals.get("eq1"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("eq2"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("t"), Some(&Value::String("range".into())));
    assert_eq!(ctx.globals.get("a"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("b"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("c"), Some(&Value::Bool(false)));
}

#[test]
fn dict_list_set_parity_and_unhashable_key_errors() {
    let ctx = eval_ok(
        r#"
d = dict(set([("a", 1), ("b", 2)]), c = 3)
first = d.popitem()
s = set([10, 20, 30])
sp = s.pop()
def fn_key():
    return 1
hs = {fn_key: "ok"}[fn_key]
g = getattr(struct(a = 1), "missing", None)
s_sorted = sorted([3, 1, 2], lambda x: -x, False)
"#,
    );
    assert_eq!(
        ctx.globals.get("sp"),
        Some(&Value::Int(zel_starlark::BigInt::from_i64(10)))
    );
    assert_eq!(ctx.globals.get("hs"), Some(&Value::String("ok".into())));
    assert_eq!(ctx.globals.get("g"), Some(&Value::None));
    assert!(eval_err("x = {}[[1]]").contains("unhashable"));
    assert!(eval_err("x = [1] in {}").contains("unhashable"));
    assert!(eval_err("x = [1] in set()").contains("unhashable"));
    assert!(eval_err("{}.popitem()").contains("popitem: empty dictionary"));
}

#[test]
fn percent_formatting_twos_complement_and_float_specifiers() {
    let ctx = eval_ok(
        r#"
hx = "%x %X %o" % (-1, -16, -8)
fl = "%f %e %E %g" % (3.5, 0.1, 1200.0, 2.5)
"#,
    );
    assert_eq!(
        ctx.globals.get("hx"),
        Some(&Value::String("ffffffff FFFFFFF0 37777777770".into()))
    );
    assert_eq!(
        ctx.globals.get("fl"),
        Some(&Value::String(
            "3.500000 1.000000e-01 1.200000E+03 2.5".into()
        ))
    );
    assert!(eval_err("x = 1.0 / 0.0").contains("division by zero"));
    assert!(eval_err("x = 1 % 0.0").contains("modulo by zero"));
    assert!(eval_err("x = int(float('inf'))").contains("can't convert float +inf to int"));
    assert!(eval_err("x = '%d' % float('inf')").contains("can't convert float +inf to int"));
    assert!(eval_err("x = int(' 10 ')").contains("invalid base-10 literal"));
    assert!(eval_err("x = int('010', 0)").contains("cannot infer base"));
}

#[test]
fn call_evaluation_order_and_kwargs_insertion_order() {
    let ctx = eval_ok(
        r#"
order = []
def rec(tag, val):
    order.append(tag)
    return val

def target(a, b = 0, **kw):
    return list(kw.keys())

keys = target(
    rec("pos", 1),
    z = rec("kw_z", 2),
    *rec("star", []),
    **rec("dstar", {"m": 3, "c": 4}),
)
"#,
    );
    assert_eq!(
        ctx.globals.get("order").unwrap().repr(),
        r#"["pos", "kw_z", "star", "dstar"]"#
    );
    assert_eq!(
        ctx.globals.get("keys").unwrap().repr(),
        r#"["z", "m", "c"]"#
    );
}

#[test]
fn json_validation_and_dotted_version_semantics() {
    let ctx = eval_ok(
        r#"
fallback = json.decode("{bad", None)
ind = json.indent('{"a":1}', prefix = "", indent = "  ")
"#,
    );
    assert_eq!(ctx.globals.get("fallback"), Some(&Value::None));
    assert!(eval_err("json.indent('{bad')").contains("JSON"));

    assert!(parse_dotted_version("1.2beta3").is_ok());
    assert!(parse_dotted_version("").is_err());
    assert!(parse_dotted_version(".1").is_err());
    assert!(parse_dotted_version("1..2").is_err());
    assert_eq!(
        dotted_version_cmp("1.2", "1.2.0"),
        std::cmp::Ordering::Equal
    );
    assert_eq!(
        dotted_version_cmp("10.10.simulator", "10.10"),
        std::cmp::Ordering::Equal
    );
    assert_eq!(
        dotted_version_cmp("1.2beta2", "1.2beta10"),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        dotted_version_cmp("1.2alpha", "1.2"),
        std::cmp::Ordering::Less
    );
    let dv1 = Value::Host("dotted_version".into(), "1.2".into());
    let dv2 = Value::Host("dotted_version".into(), "1.2.0".into());
    assert_eq!(dv1, dv2);
}

#[test]
fn step_limit_doc_comments_raw_bytes_and_cpu_profiler() {
    let mut ctx = EvalContext::default();
    ctx.thread.step_limit = 20;
    let err = ctx
        .eval_str("x = 0\nfor i in range(100):\n    x += i\n")
        .unwrap_err()
        .0;
    assert!(err.contains("too many steps"), "got: {err}");

    let mut doc_ctx = EvalContext::default();
    doc_ctx.file_name = "pkg/doc.bzl".to_string();
    doc_ctx
        .eval_str("# Leading module doc\n\n#: Doc for MY_CONST\n#: second line\nMY_CONST = 1\n")
        .unwrap();
    let docs = doc_ctx.module_env.doc_comments.lock().unwrap();
    let my_doc = docs
        .iter()
        .find(|(k, _)| k == "MY_CONST")
        .expect("doc comment for MY_CONST");
    assert_eq!(my_doc.1.text(), "Doc for MY_CONST\nsecond line");

    let mut byte_ctx = EvalContext::default();
    byte_ctx.file_name = "pkg/latin1.bzl".to_string();
    byte_ctx
        .eval_bytes_checked(b"RAW = \"\xff\"\nN = len(RAW)\n", None)
        .unwrap();
    assert_eq!(
        byte_ctx.globals.get("N"),
        Some(&Value::Int(zel_starlark::BigInt::from_i64(1)))
    );

    assert_eq!(Location::builtin().to_string(), "<builtin>");
    assert_eq!(Location::from_file("pkg/BUILD").to_string(), "pkg/BUILD");

    let tmp = std::env::temp_dir().join("zel_starlark_profile_test.pprof.gz");
    assert!(start_cpu_profile(&tmp, std::time::Duration::ZERO));
    let _ = eval_ok("acc = 0\nfor i in range(20):\n    acc += i\n");
    stop_cpu_profile().unwrap();
    let gz = std::fs::read(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(gz.len() > 20 && gz[0] == 0x1f && gz[1] == 0x8b);

    let mut hook_ctx = EvalContext::default();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_cl = Arc::clone(&seen);
    hook_ctx.post_assign_hook = Some(Arc::new(
        move |_ctx, name: &str, _val: &Value| -> Result<(), StarlarkError> {
            seen_cl.lock().unwrap().push(name.to_string());
            Ok(())
        },
    ));
    hook_ctx
        .eval_str("A = 1\nif True:\n    B = 2\nC = 3\n")
        .unwrap();
    assert_eq!(
        &*seen.lock().unwrap(),
        &vec!["A".to_string(), "C".to_string()]
    );
    let _ = BTreeMap::<String, Value>::new();
}
