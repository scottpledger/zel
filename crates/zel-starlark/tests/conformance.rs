// Copyright 2026 The Bazel Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use zel_starlark::{BigInt, EvalContext, Value};

#[test]
fn test_all_any_enumerate_zip() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
a1 = all([1, True, "hello"])
a2 = all([1, False, "hello"])
o1 = any([0, False, ""])
o2 = any([0, False, "yes"])
en = enumerate(["a", "b"], start = 1)
z = zip([1, 2], ["x", "y", "z"])
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("a1"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("a2"), Some(&Value::Bool(false)));
    assert_eq!(ctx.globals.get("o1"), Some(&Value::Bool(false)));
    assert_eq!(ctx.globals.get("o2"), Some(&Value::Bool(true)));

    let en_val = ctx.globals.get("en").unwrap().to_string();
    assert_eq!(en_val, "[(1, \"a\"), (2, \"b\")]");

    let z_val = ctx.globals.get("z").unwrap().to_string();
    assert_eq!(z_val, "[(1, \"x\"), (2, \"y\")]");
}

#[test]
fn test_min_max_sorted_reversed() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
m1 = min(5, 2, 8, 1, 9)
m2 = max([10, 4, 99, 3])
s1 = sorted([5, 2, 8, 1], reverse = True)
r1 = reversed([1, 2, 3])
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("m1"), Some(&Value::Int(BigInt::from_i64(1))));
    assert_eq!(ctx.globals.get("m2"), Some(&Value::Int(BigInt::from_i64(99))));
    assert_eq!(ctx.globals.get("s1").unwrap().to_string(), "[8, 5, 2, 1]");
    assert_eq!(ctx.globals.get("r1").unwrap().to_string(), "[3, 2, 1]");
}

#[test]
fn test_hash_dir_getattr_hasattr_type() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
h = hash("hello")
s = struct(x = 10, y = "abc")
has_x = hasattr(s, "x")
has_z = hasattr(s, "z")
gx = getattr(s, "x")
gz = getattr(s, "z", default = 42)
t_int = type(123)
t_float = type(1.5)
t_range = type(range(5))
t_set = type(set([1]))
"#,
    )
    .unwrap();

    // Java String.hashCode() for "hello": 99162322
    assert_eq!(ctx.globals.get("h"), Some(&Value::Int(BigInt::from_i64(99162322))));
    assert_eq!(ctx.globals.get("has_x"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("has_z"), Some(&Value::Bool(false)));
    assert_eq!(ctx.globals.get("gx"), Some(&Value::Int(BigInt::from_i64(10))));
    assert_eq!(ctx.globals.get("gz"), Some(&Value::Int(BigInt::from_i64(42))));
    assert_eq!(ctx.globals.get("t_int"), Some(&Value::String("int".to_string())));
    assert_eq!(ctx.globals.get("t_float"), Some(&Value::String("float".to_string())));
    assert_eq!(ctx.globals.get("t_range"), Some(&Value::String("range".to_string())));
    assert_eq!(ctx.globals.get("t_set"), Some(&Value::String("set".to_string())));
}

#[test]
fn test_list_mutation_and_methods() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
l = [1, 2, 3]
l.append(4)
l.extend([5, 6])
l.insert(0, 0)
idx = l.index(3)
popped = l.pop()
l.remove(2)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("l").unwrap().to_string(), "[0, 1, 3, 4, 5]");
    assert_eq!(ctx.globals.get("idx"), Some(&Value::Int(BigInt::from_i64(3))));
    assert_eq!(ctx.globals.get("popped"), Some(&Value::Int(BigInt::from_i64(6))));
}

#[test]
fn test_dict_any_hashable_key_and_methods() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
d = {1: "int_one", "1": "str_one", (1, 2): "tuple_key", 3.0: "float_key"}
val_int = d[1]
val_str = d["1"]
val_tuple = d[(1, 2)]
val_float = d[3] # 3.0 and 3 match the same key!

d.setdefault("new_key", "default_val")
d_pop = d.pop(1)
items = d.items()
keys = d.keys()
values = d.values()
d2 = d | {"extra": 99}
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("val_int"), Some(&Value::String("int_one".to_string())));
    assert_eq!(ctx.globals.get("val_str"), Some(&Value::String("str_one".to_string())));
    assert_eq!(ctx.globals.get("val_tuple"), Some(&Value::String("tuple_key".to_string())));
    assert_eq!(ctx.globals.get("val_float"), Some(&Value::String("float_key".to_string())));
    assert_eq!(ctx.globals.get("d_pop"), Some(&Value::String("int_one".to_string())));
    assert!(ctx.globals.get("d2").unwrap().to_string().contains("extra"));
}

#[test]
fn test_set_algebra_and_methods() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
s1 = set([1, 2, 3])
s2 = set([3, 4, 5])
union_set = s1 | s2
inter_set = s1 & s2
diff_set = s1 - s2
sym_diff = s1 ^ s2
is_sub = set([1, 2]).issubset(s1)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("union_set").unwrap().to_string(), "set([1, 2, 3, 4, 5])");
    assert_eq!(ctx.globals.get("inter_set").unwrap().to_string(), "set([3])");
    assert_eq!(ctx.globals.get("diff_set").unwrap().to_string(), "set([1, 2])");
    assert_eq!(ctx.globals.get("sym_diff").unwrap().to_string(), "set([1, 2, 4, 5])");
    assert_eq!(ctx.globals.get("is_sub"), Some(&Value::Bool(true)));
}

#[test]
fn test_lazy_range() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
r = range(10)
r_len = len(r)
r_idx = r[3]
r_neg = r[-1]
r_list = list(r)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("r_len"), Some(&Value::Int(BigInt::from_i64(10))));
    assert_eq!(ctx.globals.get("r_idx"), Some(&Value::Int(BigInt::from_i64(3))));
    assert_eq!(ctx.globals.get("r_neg"), Some(&Value::Int(BigInt::from_i64(9))));
    assert_eq!(ctx.globals.get("r_list").unwrap().to_string(), "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]");

    // Step 0 must error
    assert!(ctx.eval_str("range(0, 10, 0)").is_err());
}

#[test]
fn test_struct_operations() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
s1 = struct(a = 1, b = 2)
s2 = struct(c = 3)
s3 = s1 + s2
r = repr(s3)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("r"), Some(&Value::String("struct(a = 1, b = 2, c = 3)".to_string())));

    // Common field addition must error
    assert!(ctx.eval_str("s = struct(a = 1) + struct(a = 2)").is_err());

    // Nonexistent field must error with available attributes
    let err = ctx.eval_str("s = struct(a = 1).nonexistent").unwrap_err();
    assert!(err.to_string().contains("Available attributes: a"));
}

#[test]
fn test_recursion_prohibition() {
    let mut ctx = EvalContext::default();
    let res = ctx.eval_str(
        r#"
def countdown(n):
    if n <= 0:
        return 0
    return countdown(n - 1)

countdown(10)
"#,
    );
    assert!(res.is_err());
    assert!(res.unwrap_err().to_string().contains("called recursively"));
}

#[test]
fn test_freezing_and_mutability() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
l = [1, 2, 3]
def f():
    for x in l:
        l.append(x)
"#,
    )
    .unwrap();

    // Mutating during iteration must fail!
    let err = ctx.eval_str("f()").unwrap_err();
    assert!(err.to_string().contains("temporarily immutable due to active for-loop iteration"));
}

#[test]
fn test_depset_orders_and_invariants() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
d_post = depset(["a"], order = "postorder", transitive = [depset(["b"])])
l_post = d_post.to_list()

d_pre = depset(["a"], order = "preorder", transitive = [depset(["b"])])
l_pre = d_pre.to_list()

d_topo = depset(["a"], order = "topological", transitive = [depset(["b"])])
l_topo = d_topo.to_list()
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("l_post").unwrap().to_string(), "[\"b\", \"a\"]");
    assert_eq!(ctx.globals.get("l_pre").unwrap().to_string(), "[\"a\", \"b\"]");
    assert_eq!(ctx.globals.get("l_topo").unwrap().to_string(), "[\"a\", \"b\"]");

    // Direct iteration over depset must fail
    assert!(ctx.eval_str("for x in depset([1]): pass").is_err());

    // Heterogeneous depset elements must fail
    assert!(ctx.eval_str("d = depset([1, 'a'])").is_err());
}

#[test]
fn test_json_module() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
enc = json.encode({"b": 2, "a": 1, "nested": [True, None, 1.5]})
dec = json.decode('{"x": 10, "y": [true, null]}')
ind = json.indent('{"a":1,"b":2}', prefix = "", indent = "  ")
"#,
    )
    .unwrap();

    // Key order in JSON encode is sorted: "a" before "b"
    assert_eq!(
        ctx.globals.get("enc"),
        Some(&Value::String("{\"a\":1,\"b\":2,\"nested\":[true,null,1.5]}".to_string()))
    );
    assert_eq!(ctx.globals.get("dec").unwrap().to_string(), "{\"x\": 10, \"y\": [True, None]}");
    assert!(ctx.globals.get("ind").unwrap().to_string().contains("\n  \"a\": 1,"));
}

#[test]
fn test_proto_module() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
txt = proto.encode_text(struct(
    name = "foo",
    count = 42,
    sub = struct(active = True),
    tags = ["a", "b"]
))
"#,
    )
    .unwrap();

    let expected = r#"count: 42
name: "foo"
sub {
  active: true
}
tags: "a"
tags: "b"
"#;
    assert_eq!(ctx.globals.get("txt"), Some(&Value::String(expected.to_string())));
}

#[test]
fn test_string_format_and_methods() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
f1 = "hello {}".format("world")
f2 = "{a} and {b}".format(a = 1, b = 2)
f3 = "{{escaped}}".format()
p1 = "lawl".partition("a")
p2 = "google".rpartition("o")
rp = "foo_bar".removeprefix("foo_")
rs = "foo_bar".removesuffix("_bar")
lines = "a\nb\r\nc".splitlines()
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("f1"), Some(&Value::String("hello world".to_string())));
    assert_eq!(ctx.globals.get("f2"), Some(&Value::String("1 and 2".to_string())));
    assert_eq!(ctx.globals.get("f3"), Some(&Value::String("{escaped}".to_string())));
    assert_eq!(ctx.globals.get("p1").unwrap().to_string(), "(\"l\", \"a\", \"wl\")");
    assert_eq!(ctx.globals.get("p2").unwrap().to_string(), "(\"go\", \"o\", \"gle\")");
    assert_eq!(ctx.globals.get("rp"), Some(&Value::String("bar".to_string())));
    assert_eq!(ctx.globals.get("rs"), Some(&Value::String("foo".to_string())));
    assert_eq!(ctx.globals.get("lines").unwrap().to_string(), "[\"a\", \"b\", \"c\"]");
}

#[test]
fn test_float_ieee754_semantics() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
nan = float("NaN")
nan_eq = (nan == nan)
inf = float("+Inf")
neginf = float("-Inf")
f_int_eq = (123.0 == 123)
f_str = str(0.0)
f_div = 1 / 2
f_floordiv = 5.5 // 2
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("nan_eq"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("f_int_eq"), Some(&Value::Bool(true)));
    assert_eq!(ctx.globals.get("f_str"), Some(&Value::String("0.0".to_string())));
    assert_eq!(ctx.globals.get("f_div"), Some(&Value::Float(0.5)));
    assert_eq!(ctx.globals.get("f_floordiv"), Some(&Value::Float(2.0)));
}

#[test]
fn test_int_constructor() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
i0 = int("0")
i1 = int("42")
i2 = int("-1234")
i3 = int("0xAF", 0)
i4 = int("1111111111111", 2)
i5 = int("11", 36)
i6 = int("-0x10", 0)
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("i0"), Some(&Value::Int(BigInt::from_i64(0))));
    assert_eq!(ctx.globals.get("i1"), Some(&Value::Int(BigInt::from_i64(42))));
    assert_eq!(ctx.globals.get("i2"), Some(&Value::Int(BigInt::from_i64(-1234))));
    assert_eq!(ctx.globals.get("i3"), Some(&Value::Int(BigInt::from_i64(175))));
    assert_eq!(ctx.globals.get("i4"), Some(&Value::Int(BigInt::from_i64(8191))));
    assert_eq!(ctx.globals.get("i5"), Some(&Value::Int(BigInt::from_i64(37))));
    assert_eq!(ctx.globals.get("i6"), Some(&Value::Int(BigInt::from_i64(-16))));
}

#[test]
fn test_string_slice_index() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
s = "0123456"
s1 = s[0:-1]
s2 = s[2:4]
s3 = s[-2:-1]
s4 = s[::1]
s5 = s[1::1]
s6 = s[:2:1]
s7 = s[::-1]
s8 = s[4::-1]
s9 = s[:0:-1]
s10 = s[::3]
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("s1"), Some(&Value::String("012345".to_string())));
    assert_eq!(ctx.globals.get("s2"), Some(&Value::String("23".to_string())));
    assert_eq!(ctx.globals.get("s3"), Some(&Value::String("5".to_string())));
    assert_eq!(ctx.globals.get("s4"), Some(&Value::String("0123456".to_string())));
    assert_eq!(ctx.globals.get("s5"), Some(&Value::String("123456".to_string())));
    assert_eq!(ctx.globals.get("s6"), Some(&Value::String("01".to_string())));
    assert_eq!(ctx.globals.get("s7"), Some(&Value::String("6543210".to_string())));
    assert_eq!(ctx.globals.get("s8"), Some(&Value::String("43210".to_string())));
    assert_eq!(ctx.globals.get("s9"), Some(&Value::String("654321".to_string())));
    assert_eq!(ctx.globals.get("s10"), Some(&Value::String("036".to_string())));
}

#[test]
fn test_comprehensions_multiple_clauses() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
matrix = [[1, 2], [3, 4]]
flattened = [x for row in matrix for x in row]
filtered = [x for x in [1, 2, 3, 4, 5, 6] if x > 2 if x % 2 == 0]
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("flattened").unwrap().to_string(), "[1, 2, 3, 4]");
    assert_eq!(ctx.globals.get("filtered").unwrap().to_string(), "[4, 6]");
}

#[test]
fn test_starlark_semantics_flags() {
    let ctx = EvalContext::default();
    assert_eq!(ctx.semantics.nested_set_depth_limit, 3500);
    assert_eq!(ctx.semantics.incompatible_always_check_depset_elements, true);
    assert_eq!(ctx.semantics.incompatible_stop_exporting_build_file_path, true);
    assert_eq!(ctx.semantics.incompatible_no_implicit_file_export, true);
    assert_eq!(ctx.semantics.incompatible_fail_on_unknown_attributes, true);
}

#[test]
fn test_semicolon_separated_statements() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
a = 1; b = 2; c = 3
d = 4;
;; e = 5;; f = 6;
def foo():
    x = 10; y = 20
    return x + y
res = foo()
if True: g = 7; h = 8
"#,
    )
    .unwrap();

    assert_eq!(ctx.globals.get("a"), Some(&Value::Int(BigInt::from(1))));
    assert_eq!(ctx.globals.get("b"), Some(&Value::Int(BigInt::from(2))));
    assert_eq!(ctx.globals.get("c"), Some(&Value::Int(BigInt::from(3))));
    assert_eq!(ctx.globals.get("d"), Some(&Value::Int(BigInt::from(4))));
    assert_eq!(ctx.globals.get("e"), Some(&Value::Int(BigInt::from(5))));
    assert_eq!(ctx.globals.get("f"), Some(&Value::Int(BigInt::from(6))));
    assert_eq!(ctx.globals.get("res"), Some(&Value::Int(BigInt::from(30))));
    assert_eq!(ctx.globals.get("g"), Some(&Value::Int(BigInt::from(7))));
    assert_eq!(ctx.globals.get("h"), Some(&Value::Int(BigInt::from(8))));

    let toks = zel_starlark::syntax::lexer::Lexer::new("test.bzl", "load(\":foo.bzl\", \"foo\"); print(\"bar\"); x = 1\n").lex_all().unwrap();
    let stmts = zel_starlark::syntax::parser::Parser::new(toks).parse_module().unwrap();
    assert_eq!(stmts.len(), 3);
}

