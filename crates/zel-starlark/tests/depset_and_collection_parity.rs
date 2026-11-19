use std::sync::{Arc, Mutex};
use zel_starlark::{EvalContext, StarlarkList, Value};

#[test]
fn sorted_min_max_key_and_comparison_errors() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
s1 = sorted([[0, 5], [4, 1], [1, 7]], key = max)
s2 = sorted(["two", "three", "four"], key = len)
s3 = sorted(["two", "three", "four"], key = len, reverse = True)
s4 = sorted([[2, 6, 1], [5, 2, 1], [1, 4, 2]], key = sorted)
s5 = sorted(["a", "B", "c"], key = lambda s: s.lower())
s6 = sorted(["a", "B", "c"], key = lambda s: s.lower(), reverse = True)

acc = []
def keyfn(k):
    acc.append(k)
    return k
s7 = sorted([3, 1, 4, 1, 5, 9], key = keyfn)

m1 = min("aBcDeFXyZ".elems(), key = lambda s: s.upper())
m2 = max("aBcDeFXyZ".elems(), key = lambda s: s.lower())
m3 = min("test", "xyz", key = len)
m4 = max("test", "xyz", key = len)
m5 = min([1, 5], [1, 6], [2, 4], [0, 6], key = lambda x: x[1])
m6 = min(1, -1, -2, 2, key = abs)
m7 = max(1, -1, -2, 2, key = abs)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals["s1"].to_string(), "[[4, 1], [0, 5], [1, 7]]");
    assert_eq!(
        ctx.globals["s2"].to_string(),
        "[\"two\", \"four\", \"three\"]"
    );
    assert_eq!(
        ctx.globals["s3"].to_string(),
        "[\"three\", \"four\", \"two\"]"
    );
    assert_eq!(
        ctx.globals["s4"].to_string(),
        "[[1, 4, 2], [5, 2, 1], [2, 6, 1]]"
    );
    assert_eq!(ctx.globals["s5"].to_string(), "[\"a\", \"B\", \"c\"]");
    assert_eq!(ctx.globals["s6"].to_string(), "[\"c\", \"B\", \"a\"]");
    assert_eq!(ctx.globals["s7"].to_string(), "[1, 1, 3, 4, 5, 9]");
    assert_eq!(ctx.globals["acc"].to_string(), "[3, 1, 4, 1, 5, 9]");

    assert_eq!(ctx.globals["m1"], Value::String("a".into()));
    assert_eq!(ctx.globals["m2"], Value::String("Z".into()));
    assert_eq!(ctx.globals["m3"], Value::String("xyz".into()));
    assert_eq!(ctx.globals["m4"], Value::String("test".into()));
    assert_eq!(ctx.globals["m5"].to_string(), "[2, 4]");
    assert_eq!(ctx.globals["m6"].to_string(), "1");
    assert_eq!(ctx.globals["m7"].to_string(), "-2");

    let mut err_ctx = EvalContext::default();
    let e = err_ctx.eval_str("sorted([sorted, sorted])").unwrap_err().0;
    assert!(
        e.contains(
            "unsupported comparison: builtin_function_or_method <=> builtin_function_or_method"
        ),
        "{e}"
    );

    let mut bad_key = EvalContext::default();
    let e = bad_key
        .eval_str("sorted([1, 2, 3], key = 1)")
        .unwrap_err()
        .0;
    assert!(
        e.contains("parameter 'key' got value of type 'int', want 'callable or NoneType'"),
        "{e}"
    );

    let mut oob = EvalContext::default();
    let e = oob
        .eval_str("min(0, 1, 2, 3, 4, key = lambda x: 'foo'.elems()[x])")
        .unwrap_err()
        .0;
    assert!(e.contains("index out of range"), "{e}");
}

#[test]
fn plus_equals_mutates_list_in_place_and_comprehension_is_scoped() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
def list_mut():
    l1 = [1, 2]
    l2 = l1
    l2 += [3, 4]
    return l1, l2

def dict_list_mut():
    l = [1, 2]
    d = {0: l}
    d[0] += [3, 4]
    return l, d[0]

a, b = list_mut()
c, d = dict_list_mut()

x = 1
def f():
    return x
y = [f() for x in [2]]

def nested():
    nx = 10
    def nf():
        return nx
    return [nf() for nx in [20]]
ny = nested()
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals["a"].to_string(), "[1, 2, 3, 4]");
    assert_eq!(ctx.globals["b"].to_string(), "[1, 2, 3, 4]");
    assert_eq!(ctx.globals["c"].to_string(), "[1, 2, 3, 4]");
    assert_eq!(ctx.globals["d"].to_string(), "[1, 2, 3, 4]");
    assert_eq!(ctx.globals["y"].to_string(), "[1]");
    assert_eq!(ctx.globals["ny"].to_string(), "[10]");
}

#[test]
fn depset_topological_order_and_mutability_checks() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
# Topological order conflict: rightmost branch wins
c1 = depset(["a", "b"], order = "topological")
c2 = depset(["b", "a"], order = "topological")
conflict = depset(transitive = [c1, c2], order = "topological").to_list()

# Topological nestingNoDuplicates
sub = depset(["c", "a", "e"], order = "topological")
nest = depset(["b", "d", "e"], transitive = [sub], order = "topological").to_list()

# Default parent containing topological child keeps child's pre-reverse layout
def_with_topo = depset(transitive = [c1]).to_list()
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals["conflict"].to_string(), "[\"b\", \"a\"]");
    assert_eq!(
        ctx.globals["nest"].to_string(),
        "[\"b\", \"d\", \"c\", \"a\", \"e\"]"
    );
    assert_eq!(ctx.globals["def_with_topo"].to_string(), "[\"b\", \"a\"]");

    // Mutability & frozen list checks
    let mut frozen_list = StarlarkList::new(vec![]);
    frozen_list.freeze();
    let frozen_val = Value::List(Arc::new(Mutex::new(frozen_list)));

    let mut mctx = EvalContext::default();
    mctx.globals.insert("x".into(), frozen_val);

    let e1 = mctx.eval_str("depset([[1, 2, 3]])").unwrap_err().0;
    assert!(
        e1.contains("depset elements must not be mutable values"),
        "{e1}"
    );
    let e2 = mctx.eval_str("depset([struct(a=[])])").unwrap_err().0;
    assert!(
        e2.contains("depset elements must not be mutable values"),
        "{e2}"
    );
    let e3 = mctx.eval_str("depset([{}])").unwrap_err().0;
    assert!(
        e3.contains("depset elements must not be mutable values"),
        "{e3}"
    );
    let e4 = mctx.eval_str("depset([struct(a={})])").unwrap_err().0;
    assert!(
        e4.contains("depset elements must not be mutable values"),
        "{e4}"
    );
    let e5 = mctx.eval_str("depset([x])").unwrap_err().0;
    assert!(
        e5.contains("depsets cannot contain items of type 'list'"),
        "{e5}"
    );

    // Tuple of frozen list is allowed
    mctx.eval_str("ok = depset([(x,)]).to_list()").unwrap();
    assert_eq!(mctx.globals["ok"].to_string(), "[([],)]");
}
