//! Regression tests for big-integer shifts evaluated through the interpreter
//! (`1 << 70` was reported to evaluate to `2`).

use zel_starlark::{BigInt, EvalContext, Value};

fn eval_x(src: &str) -> Value {
    let mut ctx = EvalContext::default();
    ctx.eval_str(src).unwrap();
    ctx.globals.get("x").cloned().unwrap()
}

fn as_decimal(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        other => panic!("not an int: {other:?}"),
    }
}

#[test]
fn shl_beyond_64_bits() {
    assert_eq!(as_decimal(&eval_x("x = 1 << 70")), "1180591620717411303424");
    assert_eq!(as_decimal(&eval_x("x = 1<<70")), "1180591620717411303424");
    assert_eq!(as_decimal(&eval_x("x = 3 << 65")), "110680464442257309696");
    assert_eq!(as_decimal(&eval_x("x = 1 << 100")), "1267650600228229401496703205376");
    assert_eq!(as_decimal(&eval_x("x = 1 << 64")), "18446744073709551616");
    assert_eq!(as_decimal(&eval_x("x = 1 << 63")), "9223372036854775808");
    assert_eq!(as_decimal(&eval_x("x = 1 << 32")), "4294967296");
}

#[test]
fn shl_with_variable_and_augmented_assignment() {
    assert_eq!(as_decimal(&eval_x("y = 70\nx = 1 << y")), "1180591620717411303424");
    assert_eq!(as_decimal(&eval_x("x = 1\nx <<= 70")), "1180591620717411303424");
    assert_eq!(
        as_decimal(&eval_x("def f(n):\n    return 1 << n\nx = f(70)")),
        "1180591620717411303424"
    );
}

#[test]
fn shr_roundtrip_and_negative() {
    assert_eq!(as_decimal(&eval_x("x = (1 << 70) >> 69")), "2");
    assert_eq!(as_decimal(&eval_x("x = (1 << 70) >> 70")), "1");
    assert_eq!(as_decimal(&eval_x("x = (1 << 200) >> 199")), "2");
    assert_eq!(as_decimal(&eval_x("x = -1 << 70")), "-1180591620717411303424");
    assert_eq!(as_decimal(&eval_x("x = -(1 << 70) >> 69")), "-2");
}

#[test]
fn shift_errors() {
    let mut ctx = EvalContext::default();
    assert!(ctx.eval_str("x = 1 << -1").is_err());
    let _ = BigInt::from_i64(1);
}
