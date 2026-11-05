//! Argument-binding error texts of user-defined functions (`Starlark.fastcall`), as printed by Bazel.
use zel_starlark::EvalContext;

fn err_of(src: &str) -> String {
    let mut ctx = EvalContext::default();
    ctx.file_name = "t.bzl".into();
    ctx.eval_str(src).unwrap_err().0
}

fn msg(e: &str) -> &str {
    e.split_once(": ").map(|(_, m)| m).unwrap_or(e)
}

#[test]
fn missing_positional_and_keyword_only() {
    assert_eq!(msg(&err_of("def f(a): pass\nf()")), "f() missing 1 required positional argument: a");
    assert_eq!(msg(&err_of("def f(a, b): pass\nf()")), "f() missing 2 required positional arguments: a, b");
    assert_eq!(msg(&err_of("def f(*, a): pass\nf()")), "f() missing 1 required keyword-only argument: a");
    assert_eq!(msg(&err_of("def f(*, a, b): pass\nf()")), "f() missing 2 required keyword-only arguments: a, b");
}

#[test]
fn unexpected_and_duplicate_keywords() {
    assert_eq!(msg(&err_of("def f(a): pass\nf(1, b=2)")), "f() got unexpected keyword argument: b");
    assert_eq!(msg(&err_of("def f(a): pass\nf(1, b=2, c=3)")), "f() got unexpected keyword arguments: b, c");
    assert_eq!(msg(&err_of("def f(a): pass\nf(1, a=2)")), "f() got multiple values for parameter 'a'");
}

#[test]
fn too_many_positionals() {
    assert_eq!(msg(&err_of("def f(a): pass\nf(1, 2)")), "f() accepts no more than 1 positional argument but got 2");
    assert_eq!(msg(&err_of("def f(a, b=1): pass\nf(1, 2, 3)")), "f() accepts no more than 2 positional arguments but got 3");
    assert_eq!(msg(&err_of("def f(): pass\nf(1)")), "f() does not accept positional arguments, but got 1");
    assert_eq!(msg(&err_of("def f(*, a): pass\nf(1)")), "f() does not accept positional arguments, but got 1");
}

#[test]
fn star_arguments() {
    assert!(err_of("def f(*a): pass\nf(*1)").contains("argument after * must be an iterable, not int"));
    assert!(err_of("def f(**a): pass\nf(**1)").contains("argument after ** must be a dict, not int"));
    assert!(err_of("def f(**a): pass\nf(a=1, **{\"a\": 2})").contains("f() got multiple values for parameter 'a'"));
}

#[test]
fn non_iterable_for_and_dict_attr() {
    assert!(err_of("def f(k):\n    for x in k:\n        pass\nf(3)").contains("type 'int' is not iterable"));
    assert!(err_of("d = {}\nd.name").contains("'dict' value has no field or method 'name'"));
    assert!(err_of("x = None\nx.foo").contains("'NoneType' value has no field or method 'foo'"));
}

#[test]
fn valid_calls_still_bind() {
    let mut ctx = EvalContext::default();
    ctx.eval_str("def f(a, b=2, *args, c, **kw): return (a, b, args, c, kw)\nr = f(1, c=3, z=4)\ns = f(1, 5, 6, c=0)").unwrap();
}
