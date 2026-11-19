use std::collections::BTreeMap;
use std::sync::Arc;
use zel_starlark::{BigInt, EvalContext, StarlarkError, StructValue, Value};

fn eval(src: &str) -> EvalContext {
    let mut ctx = EvalContext::default();
    ctx.eval_str(src)
        .unwrap_or_else(|e| panic!("eval failed: {}", e.0));
    ctx
}

fn eval_err(src: &str) -> String {
    let mut ctx = EvalContext::default();
    ctx.eval_str(src).unwrap_err().0
}

#[test]
fn int_float_exact_comparisons_and_hash() {
    let ctx = eval(
        r#"
c1 = (1 < 1.5)
c2 = (1.5 > 1)
c3 = (1 < 1.9)
c4 = (-1 > -1.5)
c5 = ((1 << 100) < float("+inf"))
c6 = (-(1 << 100) > float("-inf"))
c7 = ((1 << 100) < float("NaN"))
c8 = (float("+inf") < float("NaN"))
c9 = ([1] < [1.5])
c10 = ((1,) < (1.5,))
c11 = ([None] < [None, 1])
c12 = ((None,) < (None, 1))
c13 = (({},) < ({}, 1))
c14 = ((set(),) < (set(), 1))
big_f = 1e200 * 1e100
d_nan = {float("NaN"): "ok"}
nan_val = d_nan[float("NaN")]
"#,
    );
    for k in [
        "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12", "c13", "c14",
    ] {
        assert_eq!(ctx.globals.get(k), Some(&Value::Bool(true)), "{k}");
    }
    assert_eq!(
        ctx.globals.get("nan_val"),
        Some(&Value::String("ok".into()))
    );

    // Non-BMP UTF-16 ordering: U+10000 (surrogate pair 0xD800 0xDC00) < U+E000
    let mut ctx2 = EvalContext::default();
    ctx2.globals
        .insert("sup".into(), Value::String("\u{10000}".into()));
    ctx2.globals
        .insert("pua".into(), Value::String("\u{e000}".into()));
    ctx2.eval_str("lt = (sup < pua)\nt_lt = ((sup,) < (pua,))")
        .unwrap();
    assert_eq!(ctx2.globals.get("lt"), Some(&Value::Bool(true)));
    assert_eq!(ctx2.globals.get("t_lt"), Some(&Value::Bool(true)));

    // Very large exact power-of-2 float hashing must not panic
    let mut ctx3 = EvalContext::default();
    let big_pow2 = 2.0f64.powi(600);
    ctx3.globals.insert("bf".into(), Value::Float(big_pow2));
    ctx3.eval_str("d = {bf: 42}; got = d[bf]").unwrap();
    assert_eq!(
        ctx3.globals.get("got"),
        Some(&Value::Int(BigInt::from_i64(42)))
    );
}

#[test]
fn hashability_rules() {
    assert!(eval_err("d = {range(5): 1}").contains("unhashable type: 'range'"));
    assert!(eval_err("d = {len: 1}").contains("unhashable type: 'builtin_function_or_method'"));
    assert!(eval_err("d = {struct(a = [1]): 1}").contains("unhashable type: 'struct'"));

    let ctx = eval(
        r#"
ds = depset([1, 2])
d = {ds: "found", struct(a = 1, b = (2, 3)): "s"}
v1 = d[ds]
v2 = d[struct(a = 1, b = (2, 3))]
"#,
    );
    assert_eq!(ctx.globals.get("v1"), Some(&Value::String("found".into())));
    assert_eq!(ctx.globals.get("v2"), Some(&Value::String("s".into())));
}

#[test]
fn printer_repr_str_cycles_and_percent_format() {
    let mut ctx = EvalContext::default();
    ctx.globals.insert(
        "ctrl".into(),
        Value::String("\x00\x01\x1f\n\t\x7f\u{00e9}".into()),
    );
    ctx.eval_str(
        r#"
r_ctrl = repr(ctrl)
s_inf = str(float("+inf"))
r_inf = repr(float("+inf"))
cyc_l = []
cyc_l.append(cyc_l)
r_cyc_l = repr(cyc_l)
cyc_d = {}
cyc_d["self"] = cyc_d
r_cyc_d = repr(cyc_d)
shared = [1]
dag = [shared, shared]
r_dag = repr(dag)
pct_dict = "%s" % {"a": 1}
pct_nums = "%d %o %x %X" % (15, 15, 15, 15)
"#,
    )
    .unwrap();
    assert_eq!(
        ctx.globals.get("r_ctrl"),
        Some(&Value::String(
            "\"\\x00\\x01\\x1f\\n\\t\x7f\u{00e9}\"".into()
        ))
    );
    assert_eq!(
        ctx.globals.get("s_inf"),
        Some(&Value::String("+inf".into()))
    );
    assert_eq!(
        ctx.globals.get("r_inf"),
        Some(&Value::String("+inf".into()))
    );
    assert_eq!(
        ctx.globals.get("r_cyc_l"),
        Some(&Value::String("[...]".into()))
    );
    assert_eq!(
        ctx.globals.get("r_cyc_d"),
        Some(&Value::String("{\"self\": ...}".into()))
    );
    assert_eq!(
        ctx.globals.get("r_dag"),
        Some(&Value::String("[[1], [1]]".into()))
    );
    assert_eq!(
        ctx.globals.get("pct_dict"),
        Some(&Value::String("{\"a\": 1}".into()))
    );
    assert_eq!(
        ctx.globals.get("pct_nums"),
        Some(&Value::String("15 17 f F".into()))
    );

    assert!(eval_err("\"%(a)s\" % {\"a\": 1}").contains("unsupported format character"));
    assert!(eval_err("\"%10d\" % 5").contains("unsupported format character"));
    assert!(eval_err("\"%.3g\" % 1.5").contains("unsupported format character"));
    assert!(eval_err("\"%i\" % 5").contains("unsupported format character"));
}

#[test]
fn dict_pop_setdefault_and_inplace_mutations() {
    let ctx = eval(
        r#"
d = {"a": 1}
alias_d = d
d |= {"b": 2}
p = d.pop("missing", default = 42)
sd = d.setdefault("c", default = 99)

l = [1]
alias_l = l
l += [2]
l += l

s = set([1, 2])
alias_s = s
s |= set([3])
s &= set([2, 3, 4])
s ^= set([2, 5])
s -= set([5])
"#,
    );
    assert_eq!(
        ctx.globals.get("p"),
        Some(&Value::Int(BigInt::from_i64(42)))
    );
    assert_eq!(
        ctx.globals.get("sd"),
        Some(&Value::Int(BigInt::from_i64(99)))
    );
    assert_eq!(
        ctx.globals.get("alias_d").unwrap().to_string(),
        "{\"a\": 1, \"b\": 2, \"c\": 99}"
    );
    assert_eq!(
        ctx.globals.get("alias_l").unwrap().to_string(),
        "[1, 2, 1, 2]"
    );
    assert_eq!(ctx.globals.get("alias_s").unwrap().to_string(), "set([3])");
}

#[test]
fn mutability_freezing_and_default_args() {
    // Function defaults remain mutable during the same module evaluation, then freeze at finish.
    let mut ctx = EvalContext {
        file_name: "test.bzl".into(),
        ..Default::default()
    };
    ctx.eval_file_checked(
        r#"
def f(x = []):
    x.append(1)
    return x

a = f()
b = f()
exported_dict = {"k": 1}
"#,
        None,
    )
    .unwrap();
    assert_eq!(ctx.globals.get("a").unwrap().to_string(), "[1, 1]");
    assert_eq!(ctx.globals.get("b").unwrap().to_string(), "[1, 1]");

    // Now that the module's Mutability is frozen, exported containers & defaults are frozen!
    let err = ctx
        .call_value(
            ctx.globals.get("f").unwrap().clone(),
            vec![],
            BTreeMap::new(),
        )
        .unwrap_err();
    assert!(
        err.0.contains("trying to mutate a frozen list value"),
        "{}",
        err.0
    );

    let d_val = ctx.globals.get("exported_dict").unwrap().clone();
    let Value::Dict(d_arc) = d_val else { panic!() };
    let err_d = d_arc
        .lock()
        .unwrap()
        .insert(Value::String("x".into()), Value::Int(1.into()))
        .unwrap_err();
    assert!(
        err_d.0.contains("trying to mutate a frozen dict value"),
        "{}",
        err_d.0
    );

    // `unsafe_shallow_freeze` checks Mutability flags and does not walk elements
    let mu = zel_starlark::Mutability::create_allowing_shallow_freeze();
    let inner = Arc::new(std::sync::Mutex::new(zel_starlark::StarlarkList::new_in(
        vec![Value::Int(1.into())],
        Arc::clone(&mu),
    )));
    let mut outer = zel_starlark::StarlarkDict::new_in(Arc::clone(&mu));
    outer
        .insert(Value::String("l".into()), Value::List(Arc::clone(&inner)))
        .unwrap();
    outer.unsafe_shallow_freeze().unwrap();
    assert!(outer.is_frozen());
    assert!(!inner.lock().unwrap().is_frozen());
    assert!(outer.unsafe_shallow_freeze().is_err());
}

#[test]
fn unbound_local_free_global_and_attr_assign() {
    assert!(eval_err(
        r#"
x = 1
def f():
    y = x
    x = 2
f()
"#
    )
    .contains("local variable 'x' is referenced before assignment."));

    assert!(eval_err(
        r#"
def outer():
    x = 1
    def inner():
        if False:
            x = 2
        return x
    return inner()
outer()
"#
    )
    .contains("local variable 'x' is referenced before assignment."));

    assert!(eval_err(
        r#"
def outer():
    if False:
        x = 1
    def inner():
        return x
    return inner()
outer()
"#
    )
    .contains("free variable 'x' is referenced before assignment."));

    assert!(eval_err(
        r#"
def f():
    return g
f()
g = 1
"#
    )
    .contains("global variable 'g' is referenced before assignment."));

    // x.f += y reads x.f and evaluates binary op before failing field assignment
    assert!(eval_err("s = struct(a = 1)\ns.b += 1")
        .contains("'struct' value has no field or method 'b'"));
    assert!(eval_err("s = struct(a = '')\ns.a -= 1")
        .contains("unsupported binary operation: string - int"));
    assert!(eval_err("s = struct(a = 1)\ns.a += 1").contains("cannot set .a field of struct value"));
}

#[test]
fn min_max_sorted_key_and_getattr_dir_struct_output_group() {
    let mut ctx = EvalContext::default();
    ctx.globals.insert(
        "P".into(),
        Value::Provider {
            name: "MyInfo".into(),
        },
    );
    ctx.globals.insert(
        "OutputGroupInfo".into(),
        Value::Provider {
            name: "OutputGroupInfo".into(),
        },
    );
    ctx.globals
        .insert("f1".into(), Value::Host("file".into(), "0".into()));
    ctx.eval_str(
        r#"
mn1 = min("six", "three", "four", key = len)
mn2 = min([2, -2, -1, 1], key = abs)
mx1 = max([1, -1, -2, 2], key = abs)
sr1 = sorted(["two", "three", "four"], key = len)
sr2 = sorted(["two", "three", "four"], key = len, reverse = True)
d = {"items": 99}
d_items = getattr(d, "items")()
s_up = getattr("abc", "upper")()
l = [1, 2]
getattr(l, "append")(3)
p = P(b = 1, a = 2)
p_dir = dir(p)
f_struct = struct
st = f_struct(a = 10)
def shadowed():
    struct = lambda **kw: kw["x"] + 1
    return struct(x = 5)
sh_res = shadowed()
og = OutputGroupInfo(bin = [f1])
og_type = type(og.bin)
og_len = len(og.bin.to_list())
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("mn1"), Some(&Value::String("six".into())));
    assert_eq!(ctx.globals.get("mn2"), Some(&Value::Int((-1).into())));
    assert_eq!(ctx.globals.get("mx1"), Some(&Value::Int((-2).into())));
    assert_eq!(
        ctx.globals.get("sr1").unwrap().to_string(),
        "[\"two\", \"four\", \"three\"]"
    );
    assert_eq!(
        ctx.globals.get("sr2").unwrap().to_string(),
        "[\"three\", \"four\", \"two\"]"
    );
    assert_eq!(
        ctx.globals.get("d_items").unwrap().to_string(),
        "[(\"items\", 99)]"
    );
    assert_eq!(ctx.globals.get("s_up"), Some(&Value::String("ABC".into())));
    assert_eq!(ctx.globals.get("l").unwrap().to_string(), "[1, 2, 3]");
    assert_eq!(
        ctx.globals.get("p_dir").unwrap().to_string(),
        "[\"a\", \"b\"]"
    );
    assert_eq!(ctx.globals.get("sh_res"), Some(&Value::Int(6.into())));
    assert_eq!(
        ctx.globals.get("og_type"),
        Some(&Value::String("depset".into()))
    );
    assert_eq!(ctx.globals.get("og_len"), Some(&Value::Int(1.into())));

    assert!(eval_err("min()").contains("expected at least one item"));
    assert!(eval_err("max([])").contains("expected at least one item"));
    assert!(eval_err("sorted([1, 'a'])").contains("unsupported comparison: int <=> string"));
    assert!(
        eval_err("sorted([1, 2], key = lambda x: 'a' if x == 1 else 2)")
            .contains("unsupported comparison: string <=> int")
    );
    assert!(eval_err("getattr('abc', 'uppr')").contains("(did you mean 'upper'?)"));
    assert!(eval_err("struct(1)").contains("does not accept positional arguments"));
    let mut og_err_ctx = EvalContext::default();
    og_err_ctx.globals.insert(
        "OutputGroupInfo".into(),
        Value::Provider {
            name: "OutputGroupInfo".into(),
        },
    );
    assert!(og_err_ctx
        .eval_str("OutputGroupInfo(bad = [123])")
        .unwrap_err()
        .0
        .contains("want File"));
}

#[test]
fn string_split_rsplit_join_and_json_encoding() {
    let mut ctx = EvalContext::default();
    ctx.globals.insert(
        "P".into(),
        Value::Provider {
            name: "MyInfo".into(),
        },
    );
    ctx.host = Some(Arc::new(|_ctx, name, _pos, _kw| match name {
        "attr:file.path" => Ok(Value::String("bazel-out/k8-fastbuild/bin/a.out".into())),
        "attr:file.root" => {
            let mut m = BTreeMap::new();
            m.insert(
                "path".into(),
                Value::String("bazel-out/k8-fastbuild/bin".into()),
            );
            Ok(Value::Struct(Arc::new(StructValue::new(m))))
        }
        "attr:file.short_path" => Ok(Value::String("a.out".into())),
        other => Err(StarlarkError(format!("unknown host call {other}"))),
    }));
    ctx.globals
        .insert("f".into(), Value::Host("file".into(), "0".into()));
    ctx.eval_str(
        r#"
rs = "aaa".rsplit("aa", 1)
jd = ",".join({"a": 1, "b": 2})
jp = json.encode(P(b = 2, a = 1))
jf = json.encode(f)
"#,
    )
    .unwrap();
    assert_eq!(ctx.globals.get("rs").unwrap().to_string(), "[\"a\", \"\"]");
    assert_eq!(ctx.globals.get("jd"), Some(&Value::String("a,b".into())));
    assert_eq!(
        ctx.globals.get("jp"),
        Some(&Value::String("{\"a\":1,\"b\":2}".into()))
    );
    assert_eq!(
        ctx.globals.get("jf"),
        Some(&Value::String(
            "{\"path\":\"bazel-out/k8-fastbuild/bin/a.out\",\"root\":\"bazel-out/k8-fastbuild/bin\",\"short_path\":\"a.out\"}".into()
        ))
    );

    assert!(eval_err("\"a b\".split()").contains("missing 1 required positional argument: sep"));
    assert!(eval_err("\"a b\".split(None)").contains("want 'string'"));
    assert!(eval_err("\"a b\".rsplit()").contains("missing 1 required positional argument: sep"));
    assert!(eval_err("\",\".join(['x', 123])")
        .contains("expected string for sequence element 1, got '123' of type int"));
}

#[test]
fn syntax_unpacking_resolver_and_string_literals() {
    let ctx = eval(
        r#"
out_tuple_unpack = []
for (a,) in [(1,), (2,)]:
    out_tuple_unpack.append(a)
out_list_unpack = []
for [a] in [[10], [20]]:
    out_list_unpack.append(a)
out_paren_no_comma = []
for (a) in [(1,)]:
    out_paren_no_comma.append(a)
comp_t = [a + 1 for (a,) in [(1,), (2,)]]
comp_l = [a + 1 for [a] in [[1], [2]]]
"#,
    );
    assert_eq!(
        ctx.globals.get("out_tuple_unpack").unwrap().to_string(),
        "[1, 2]"
    );
    assert_eq!(
        ctx.globals.get("out_list_unpack").unwrap().to_string(),
        "[10, 20]"
    );
    assert_eq!(
        ctx.globals.get("out_paren_no_comma").unwrap().to_string(),
        "[(1,)]"
    );
    assert_eq!(ctx.globals.get("comp_t").unwrap().to_string(), "[2, 3]");
    assert_eq!(ctx.globals.get("comp_l").unwrap().to_string(), "[2, 3]");

    assert!(eval_err("x, y = (1,)").contains("too few values to unpack (got 1, want 2)"));
    assert!(eval_err("x, y = (1, 2, 3)").contains("too many values to unpack (got 3, want 2)"));
    assert!(eval_err("x, = 'a'")
        .contains("got 'string' in sequence assignment (want 1-element sequence)"));

    // Resolver top-level rebinding in .bzl vs BUILD
    let mut bzl = EvalContext {
        file_name: "defs.bzl".into(),
        ..Default::default()
    };
    let e1 = bzl.eval_str("x = 1\nx = 2\n").unwrap_err().0;
    assert!(
        e1.contains("'x' redeclared at top level") && e1.contains("'x' previously declared here"),
        "{e1}"
    );

    let mut bzl2 = EvalContext {
        file_name: "defs.bzl".into(),
        ..Default::default()
    };
    let e2 = bzl2.eval_str("load(':a.bzl', 'x')\nx = 1\n").unwrap_err().0;
    assert!(
        e2.contains("conflicting global declaration of 'x'")
            && e2.contains("'x' previously declared as file-local here"),
        "{e2}"
    );

    let mut bld = EvalContext {
        file_name: "BUILD".into(),
        ..Default::default()
    };
    bld.eval_str("load(':a.bzl', 'x')\nload(':b.bzl', 'x')\n")
        .unwrap();
    let e3 = bld.eval_str("load(':a.bzl', 'y', 'y')\n").unwrap_err().0;
    assert!(
        e3.contains("load statement defines 'y' more than once"),
        "{e3}"
    );

    // StringLiteral lexer rules
    assert!(eval_err(r#"s = "\x41""#).contains("invalid escape sequence"));
    assert!(eval_err(r#"s = "\u0041""#).contains("invalid escape sequence"));
    assert!(eval_err(r#"s = "\U00000041""#).contains("invalid escape sequence"));
    assert!(eval_err(r#"s = "\777""#).contains("octal escape sequence out of range"));
    assert!(eval_err(r#"s = b"abc""#).contains("syntax error"));

    let raw_ctx = eval("s1 = r\"\\\"\"\ns2 = r\"a\\\r\nb\"\n");
    assert_eq!(
        raw_ctx.globals.get("s1"),
        Some(&Value::String("\\\"".into()))
    );
    assert_eq!(
        raw_ctx.globals.get("s2"),
        Some(&Value::String("a\\\nb".into()))
    );
}

#[test]
fn label_visibility_workspace_name_workspace_root_and_sibling_layout() {
    use zel_cmdline::{PackageIdentifier, RepositoryMapping, RepositoryName};
    use zel_vfs::PathFragment;

    let make_ctx = |sibling_layout: bool, entries: Vec<(&str, &str)>, owner: RepositoryName| {
        let mut ctx = EvalContext {
            file_name: "blazel_utils.bzl".into(),
            ..Default::default()
        };
        ctx.semantics.experimental_sibling_repository_layout = sibling_layout;
        ctx.thread.semantics.experimental_sibling_repository_layout = sibling_layout;
        let ordered = entries
            .into_iter()
            .map(|(a, c)| (a.to_string(), RepositoryName::create_unvalidated(c)))
            .collect();
        let pkg = PackageIdentifier::create(owner.clone(), PathFragment::empty()).unwrap();
        *ctx.module_env.label_ctx.lock().unwrap() =
            Some((pkg, RepositoryMapping::new_ordered(ordered, owner)));
        ctx.eval_str(
            r#"
def _to_label(repo_name):
    if not repo_name.startswith("@"):
        repo_name = "@" + repo_name
    return Label(repo_name + "//:unused")

def get_canonical_repo_name(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_name

def get_repo_root(apparent_repo_name):
    return _to_label(apparent_repo_name).workspace_root
"#,
        )
        .unwrap();
        ctx
    };

    // Mapped main + external repos under default external/<canonical> layout.
    let mut default_ctx = make_ctx(
        false,
        vec![
            ("", ""),
            ("my_ws", ""),
            ("rules_foo", "rules_foo+1.2"),
            ("bazel_tools", "bazel_tools"),
            ("platforms", "platforms+0.0.10"),
        ],
        RepositoryName::main(),
    );
    default_ctx
        .eval_str(
            r#"
c_main = get_canonical_repo_name("")
c_ws = get_canonical_repo_name("my_ws")
c_foo = get_canonical_repo_name("rules_foo")
c_foo_at = get_canonical_repo_name("@rules_foo")
c_bt = get_canonical_repo_name("bazel_tools")
c_plat = get_canonical_repo_name("platforms")
r_main = get_repo_root("")
r_ws = get_repo_root("my_ws")
r_foo = get_repo_root("rules_foo")
r_plat = get_repo_root("platforms")
ga_root = getattr(Label("@rules_foo//:t"), "workspace_root")
"#,
        )
        .unwrap();
    assert_eq!(
        default_ctx.globals.get("c_main"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        default_ctx.globals.get("c_ws"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        default_ctx.globals.get("c_foo"),
        Some(&Value::String("rules_foo+1.2".into()))
    );
    assert_eq!(
        default_ctx.globals.get("c_foo_at"),
        Some(&Value::String("rules_foo+1.2".into()))
    );
    assert_eq!(
        default_ctx.globals.get("c_bt"),
        Some(&Value::String("bazel_tools".into()))
    );
    assert_eq!(
        default_ctx.globals.get("c_plat"),
        Some(&Value::String("platforms+0.0.10".into()))
    );
    assert_eq!(
        default_ctx.globals.get("r_main"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        default_ctx.globals.get("r_ws"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        default_ctx.globals.get("r_foo"),
        Some(&Value::String("external/rules_foo+1.2".into()))
    );
    assert_eq!(
        default_ctx.globals.get("r_plat"),
        Some(&Value::String("external/platforms+0.0.10".into()))
    );
    assert_eq!(
        default_ctx.globals.get("ga_root"),
        Some(&Value::String("external/rules_foo+1.2".into()))
    );

    // Same under --experimental_sibling_repository_layout: main stays "", external becomes ../<canonical>.
    let mut sibling_ctx = make_ctx(
        true,
        vec![("", ""), ("my_ws", ""), ("rules_foo", "rules_foo+1.2")],
        RepositoryName::main(),
    );
    sibling_ctx
        .eval_str(
            r#"
r_main = get_repo_root("")
r_ws = get_repo_root("my_ws")
r_foo = get_repo_root("rules_foo")
ga_root = getattr(Label("@rules_foo//:t"), "workspace_root")
"#,
        )
        .unwrap();
    assert_eq!(
        sibling_ctx.globals.get("r_main"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        sibling_ctx.globals.get("r_ws"),
        Some(&Value::String("".into()))
    );
    assert_eq!(
        sibling_ctx.globals.get("r_foo"),
        Some(&Value::String("../rules_foo+1.2".into()))
    );
    assert_eq!(
        sibling_ctx.globals.get("ga_root"),
        Some(&Value::String("../rules_foo+1.2".into()))
    );

    // Unmapped apparent repo (with did-you-mean suggestion, without suggestion, and unmapped
    // bazel_tools / platforms) fails on workspace_name, repo_name, workspace_root, and getattr.
    for (expr, expected) in [
        (
            "get_canonical_repo_name('rules_fob')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'rules_fob' requested from @@ (did you mean 'rules_foo'?)]//:unused",
        ),
        (
            "Label('@rules_fob//:unused').repo_name",
            "'repo_name' is not allowed on invalid Label @@[unknown repo 'rules_fob' requested from @@ (did you mean 'rules_foo'?)]//:unused",
        ),
        (
            "get_repo_root('nope')",
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'nope' requested from @@]//:unused",
        ),
        (
            "get_canonical_repo_name('bazel_tools')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'bazel_tools' requested from @@]//:unused",
        ),
        (
            "get_repo_root('platforms')",
            "'workspace_root' is not allowed on invalid Label @@[unknown repo 'platforms' requested from @@]//:unused",
        ),
        (
            "getattr(Label('@platforms//:unused'), 'workspace_name', 'fallback')",
            "'workspace_name' is not allowed on invalid Label @@[unknown repo 'platforms' requested from @@]//:unused",
        ),
    ] {
        let mut c = make_ctx(
            true,
            vec![("", ""), ("my_ws", ""), ("rules_foo", "rules_foo+1.2")],
            RepositoryName::main(),
        );
        let err = c.eval_str(expr).unwrap_err().0;
        assert!(err.contains(expected), "for {expr}: {err}");
    }
}

#[test]
fn depset_str_dict_fifo_float_format_and_unpacking_subscripts() {
    let ctx = eval(
        r#"
ds_topo = str(depset([3, 4, 5], transitive = [depset([2, 4, 6])], order = "topological"))
ds_def = str(depset([2, 4, 6], transitive = [depset([3, 5])]))

d_pop = {2: "bar", 3: "baz", 1: "foo"}
first_popped = d_pop.popitem()

def user_fn():
    pass
fn_in_dict = user_fn in {}

d_self = {"a": 1}
d_self.update(d_self, d=4)

d_sub = {"a": 1}
d_sub["b"], d_sub["c"] = 2, 3

fe0 = "%e" % 0
fe_neg0 = "%e" % -0.0
fe123 = "%e" % 123
fe45 = "%e" % 1.23e45
ff0 = "%f" % 0
ff_neg0 = "%f" % -0.0
ff45 = "%f" % 1.23e45
ff_tiny = "%f" % -1.23e-45

mod_fi = -100.0 % 7
mod_if = 100 % 7.0

jd_str = json.decode('{"invalid": "json"', "default value")
jd_none = json.decode('{"invalid": "json"', None)
ix = int(x = "2a", base = 16)
"#,
    );
    assert_eq!(
        ctx.globals.get("ds_topo"),
        Some(&Value::String(
            "depset([3, 5, 6, 4, 2], order = \"topological\")".into()
        ))
    );
    assert_eq!(
        ctx.globals.get("ds_def"),
        Some(&Value::String("depset([3, 5, 2, 4, 6])".into()))
    );
    assert_eq!(
        ctx.globals.get("first_popped").unwrap().to_string(),
        "(2, \"bar\")"
    );
    assert_eq!(ctx.globals.get("fn_in_dict"), Some(&Value::Bool(false)));
    assert_eq!(
        ctx.globals.get("d_self").unwrap().to_string(),
        "{\"a\": 1, \"d\": 4}"
    );
    assert_eq!(
        ctx.globals.get("d_sub").unwrap().to_string(),
        "{\"a\": 1, \"b\": 2, \"c\": 3}"
    );

    assert_eq!(
        ctx.globals.get("fe0"),
        Some(&Value::String("0.000000e+00".into()))
    );
    assert_eq!(
        ctx.globals.get("fe_neg0"),
        Some(&Value::String("-0.000000e+00".into()))
    );
    assert_eq!(
        ctx.globals.get("fe123"),
        Some(&Value::String("1.230000e+02".into()))
    );
    assert_eq!(
        ctx.globals.get("fe45"),
        Some(&Value::String("1.230000e+45".into()))
    );
    assert_eq!(
        ctx.globals.get("ff0"),
        Some(&Value::String("0.000000".into()))
    );
    assert_eq!(
        ctx.globals.get("ff_neg0"),
        Some(&Value::String("-0.000000".into()))
    );
    assert_eq!(
        ctx.globals.get("ff45"),
        Some(&Value::String(
            "1230000000000000000000000000000000000000000000.000000".into()
        ))
    );
    assert_eq!(
        ctx.globals.get("ff_tiny"),
        Some(&Value::String("-0.000000".into()))
    );
    assert_eq!(ctx.globals.get("mod_fi"), Some(&Value::Float(5.0)));
    assert_eq!(ctx.globals.get("mod_if"), Some(&Value::Float(2.0)));
    assert_eq!(
        ctx.globals.get("jd_str"),
        Some(&Value::String("default value".into()))
    );
    assert_eq!(ctx.globals.get("jd_none"), Some(&Value::None));
    assert_eq!(ctx.globals.get("ix").unwrap().to_string(), "42");

    assert!(eval_err("{}.pop([], 1)").contains("unhashable type: 'list'"));
    assert!(eval_err("{} in {}").contains("unhashable type: 'dict'"));
    assert!(eval_err("len in {}").contains("unhashable type: 'builtin_function_or_method'"));
    assert!(eval_err("{1: 2}.get(len)").contains("unhashable type: 'builtin_function_or_method'"));
    assert!(eval_err("1 / 0.0").contains("floating-point division by zero"));
    assert!(eval_err("1 % 0.0").contains("floating-point modulo by zero"));
    assert!(eval_err("int()").contains("missing 1 required positional argument: x"));
}

#[test]
fn provider_instance_add_display_and_select_type_checks() {
    let mut ctx = EvalContext::default();
    let mut f1 = std::collections::BTreeMap::new();
    f1.insert("a".to_string(), Value::Int(1.into()));
    let mut f2 = std::collections::BTreeMap::new();
    f2.insert("b".to_string(), Value::String("foo".into()));
    let p1 = Value::ProviderInstance {
        provider_name: "%anonymous#1%struct".into(),
        fields: f1.clone(),
    };
    let p2 = Value::ProviderInstance {
        provider_name: "%anonymous#1%struct".into(),
        fields: f2,
    };
    let p_other = Value::ProviderInstance {
        provider_name: "%anonymous#2%other".into(),
        fields: f1,
    };
    let merged = p1.add(&p2).unwrap();
    assert_eq!(merged.to_string(), "struct(a = 1, b = \"foo\")");
    assert_eq!(merged.repr(), "struct(a = 1, b = \"foo\")");
    assert!(p1
        .add(&p1)
        .unwrap_err()
        .0
        .contains("cannot add struct instances with common field 'a'"));
    assert!(p1.add(&p_other).unwrap_err().0.contains(
        "Cannot use '+' operator on instances of different providers (<no name> and <no name>)"
    ));

    ctx.eval_str(
        r#"
sl = select({"//a": ["x"]})
sd = select({"//a": {"k": "v"}})
si = select({"//a": 1})
"#,
    )
    .unwrap();
    assert!(ctx
        .eval_str("sl + 1")
        .unwrap_err()
        .0
        .contains("Cannot combine incompatible types (select of list, int)"));
    assert!(ctx
        .eval_str("sl + si")
        .unwrap_err()
        .0
        .contains("Cannot combine incompatible types (select of list, select of int)"));
    assert!(ctx
        .eval_str("sl + sd")
        .unwrap_err()
        .0
        .contains("Cannot combine incompatible types (select of list, select of dict)"));
    assert!(ctx
        .eval_str("sl + {}")
        .unwrap_err()
        .0
        .contains("unsupported binary operation: select + dict"));
    assert!(ctx
        .eval_str("{} + sl")
        .unwrap_err()
        .0
        .contains("unsupported binary operation: dict + select"));
    assert!(ctx
        .eval_str("sl | {}")
        .unwrap_err()
        .0
        .contains("Cannot combine incompatible types (select of list, dict)"));
    assert!(ctx
        .eval_str("{} | sl")
        .unwrap_err()
        .0
        .contains("Cannot combine incompatible types (dict, select of list)"));
}
