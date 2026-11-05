use zel_starlark::EvalContext;

fn run(src: &str) -> EvalContext {
    let mut c = EvalContext::default();
    c.eval_str(src).unwrap();
    c
}

#[test]
fn closure_captures_param() {
    let c = run("calls = []\ndef mk(macro):\n    def impl(x):\n        macro()\n        return 1\n    return impl\ndef m():\n    calls.append(1)\nf = mk(m)\nr = f(0)\n");
    assert_eq!(format!("{:?}", c.globals["calls"]).contains("1"), true, "{:?}", c.globals["calls"]);
}

#[test]
fn universe_functions_are_first_class() {
    let c = run("f = len\nn = f([1, 2, 3])\ndef g(x, h = len):\n    return h(x)\nm = g('ab')\nk = sorted([3, 1, 2], key = abs)\n");
    assert_eq!(format!("{:?}", c.globals["n"]), "Int(3)");
    assert_eq!(format!("{:?}", c.globals["m"]), "Int(2)");
}

#[test]
fn params_after_star_args_are_keyword_only() {
    let c = run("def f(*a, k = 5, r = []):\n    return (len(a), k, len(r))\nx = f(1, 2)\ny = f(1, k = 9)\n");
    assert_eq!(format!("{:?}", c.globals["x"]), "Tuple([Int(2), Int(5), Int(0)])");
    assert_eq!(format!("{:?}", c.globals["y"]), "Tuple([Int(1), Int(9), Int(0)])");
}

#[test]
fn kwargs_excludes_named_params() {
    let c = run("def f(a, b = 2, **kw):\n    return sorted(kw.keys())\nx = f(a = 1, b = 3, c = 4)\n");
    assert_eq!(format!("{:?}", c.globals["x"]).contains("\"c\""), true);
    assert_eq!(format!("{:?}", c.globals["x"]).contains("\"b\""), false);
}

#[test]
fn bound_methods_are_first_class() {
    let c = run("g = {'a': 1}.get\nx = g('a')\ny = g('z', 7)\ns = 'a,b'.split\nz = s(',')\nl = [3].append\n");
    assert_eq!(format!("{:?}", c.globals["x"]), "Int(1)");
    assert_eq!(format!("{:?}", c.globals["y"]), "Int(7)");
}
