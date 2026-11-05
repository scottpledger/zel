//! Reprs that Bazel prints in a specific way (round 7B).
use zel_starlark::{EvalContext, Value};

fn run(src: &str) -> Vec<String> {
    let mut ctx = EvalContext::default();
    ctx.captured_prints = Some(Vec::new());
    ctx.eval_str(src).unwrap_or_else(|e| panic!("{}", e.0));
    ctx.captured_prints.take().unwrap().into_iter().map(|(_, m)| m).collect()
}

#[test]
fn struct_repr_quotes_strings() {
    assert_eq!(run("print(struct(a='x'))"), vec!["struct(a = \"x\")"]);
    assert_eq!(
        run("print(struct(b=[1, 'y'], a={'k': 'v'}, c=struct(d=None)))"),
        vec!["struct(a = {\"k\": \"v\"}, b = [1, \"y\"], c = struct(d = None))"]
    );
    assert_eq!(run("print(repr(struct(a='h\\u00e9\"q')))"), vec!["struct(a = \"h\u{e9}\\\"q\")"]);
}

#[test]
fn float_repr_matches_python_style() {
    let cases = [
        (1.0, "1.0"),
        (1.5, "1.5"),
        (1e20, "1e+20"),
        (1e-5, "1e-05"),
        (0.1 + 0.2, "0.30000000000000004"),
        (1e15, "1000000000000000.0"),
        (1e16, "1e+16"),
        (123456789012345680.0, "1.2345678901234568e+17"),
        (0.0001, "0.0001"),
        (0.00012345, "0.00012345"),
        (-2.5e-7, "-2.5e-07"),
        (1.5e300, "1.5e+300"),
        (100.0, "100.0"),
        (-0.0, "-0.0"),
    ];
    for (v, want) in cases {
        assert_eq!(zel_starlark::format_float(v), want, "{v:e}");
    }
}

#[test]
fn opaque_struct_prints_as_unknown_object() {
    let s = zel_starlark::StructValue::new_opaque(Default::default(), "com.example.Thing");
    assert_eq!(Value::Struct(std::sync::Arc::new(s)).repr(), "<unknown object com.example.Thing>");
}

#[test]
fn static_names_reject_undefined_before_execution() {
    let mut ctx = EvalContext::default();
    ctx.file_name = "e.bzl".into();
    ctx.static_names = true;
    ctx.captured_prints = Some(Vec::new());
    let errs = ctx.eval_file_checked("print('ran')\nx = zzz\n", None).unwrap_err();
    assert!(ctx.static_failure);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].ends_with("2:5: name 'zzz' is not defined"), "{errs:?}");
    assert!(ctx.captured_prints.unwrap().is_empty(), "the file must not run when the static check fails");
}

#[test]
fn static_names_accept_declared_universe() {
    let mut ctx = EvalContext::default();
    ctx.file_name = "e.bzl".into();
    ctx.static_names = true;
    ctx.static_universe = vec!["cc_common".into()];
    ctx.captured_prints = Some(Vec::new());
    let r = ctx.eval_file_checked("def f():\n    return cc_common\nprint('ok')\n", None);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn static_names_did_you_mean() {
    let mut ctx = EvalContext::default();
    ctx.file_name = "e.bzl".into();
    ctx.static_names = true;
    let errs = ctx.eval_file_checked("x = lenn([1])\n", None).unwrap_err();
    assert!(errs[0].contains("name 'lenn' is not defined (did you mean 'len'?)"), "{errs:?}");
}
