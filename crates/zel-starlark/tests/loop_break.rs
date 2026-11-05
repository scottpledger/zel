use zel_starlark::EvalContext;

#[test]
fn break_leaves_the_loop_and_range_is_lazy() {
    let mut ctx = EvalContext::default();
    ctx.eval_str(
        r#"
def f():
    seen = []
    for i in range(2147483647):
        seen.append(i)
        if i == 2:
            break
    return seen

def g():
    out = []
    for i in [1, 2, 3, 4]:
        if i == 2:
            continue
        if i == 4:
            break
        out.append(i)
    return out

a = f()
b = g()
"#,
    )
    .unwrap();
    assert_eq!(ctx.globals.get("a").unwrap().repr(), "[0, 1, 2]");
    assert_eq!(ctx.globals.get("b").unwrap().repr(), "[1, 3]");
}
