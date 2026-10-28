//! Property tests for the arbitrary-precision integers: the `pynum` operators
//! agree with `i128` where `i128` is wide enough, and the evaluator keeps its
//! `Int`/`BigInt` normal form.

use std::cmp::Ordering;

use linkml_map_core::expr::eval_expr;
use linkml_map_core::pynum::{self, BigInt, Bitwise, IntRef};
use linkml_map_core::value::Value;
use proptest::prelude::*;

fn big(i: i128) -> BigInt {
    BigInt::from(i)
}

/// An integer of up to about 100 bits, so products and sums fit `i128`.
fn arb_int() -> impl Strategy<Value = i128> {
    prop_oneof![
        any::<i64>().prop_map(i128::from),
        (-(1i128 << 100)..(1i128 << 100)),
        Just(i128::from(i64::MIN)),
        Just(i128::from(i64::MAX)),
        Just(i128::from(i64::MAX) + 1),
    ]
}

fn view(n: &BigInt) -> IntRef<'_> {
    IntRef::Big(n)
}

proptest! {
    #[test]
    fn arithmetic_matches_i128(a in arb_int(), b in arb_int()) {
        let (x, y) = (big(a), big(b));
        prop_assert_eq!(pynum::add(view(&x), view(&y)), big(a + b));
        prop_assert_eq!(pynum::sub(view(&x), view(&y)), big(a - b));
        if a.unsigned_abs() < (1 << 62) && b.unsigned_abs() < (1 << 62) {
            prop_assert_eq!(pynum::mul(view(&x), view(&y)), big(a * b));
        }
        if b != 0 {
            let q = pynum::floordiv(view(&x), view(&y)).unwrap();
            let r = pynum::modulo(view(&x), view(&y)).unwrap();
            // The Python identity, and the remainder takes the divisor's sign.
            prop_assert_eq!(&q * &y + &r, x.clone());
            prop_assert!(r == BigInt::from(0) || (r < BigInt::from(0)) == (b < 0));
        }
        prop_assert_eq!(pynum::bitwise(Bitwise::And, view(&x), view(&y)), big(a & b));
        prop_assert_eq!(pynum::bitwise(Bitwise::Or, view(&x), view(&y)), big(a | b));
        prop_assert_eq!(pynum::bitwise(Bitwise::Xor, view(&x), view(&y)), big(a ^ b));
        prop_assert_eq!(pynum::invert(view(&x)), big(!a));
        prop_assert_eq!(pynum::cmp(view(&x), view(&y)), a.cmp(&b));
    }

    #[test]
    fn shifts_match_i128(a in -(1i128 << 60)..(1i128 << 60), n in 0u64..60) {
        let x = big(a);
        prop_assert_eq!(pynum::lshift(view(&x), n).unwrap(), big(a << n));
        prop_assert_eq!(pynum::rshift(view(&x), n), big(a >> n));
        prop_assert_eq!(pynum::rshift(view(&x), 200), big(if a < 0 { -1 } else { 0 }));
    }

    #[test]
    fn float_conversion_is_correctly_rounded(a in arb_int()) {
        // `i128 as f64` rounds to nearest, ties to even.
        let x = big(a);
        prop_assert_eq!(pynum::big_to_f64(&x), Some(a as f64));
    }

    #[test]
    fn integer_float_order_is_exact(a in arb_int(), f in any::<f64>()) {
        let x = big(a);
        let got = pynum::cmp_big_f64(&x, f);
        if f.is_nan() {
            prop_assert_eq!(got, None);
        } else if f.is_infinite() {
            prop_assert_eq!(got, Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater }));
        } else {
            // Compare with the float's integer part, then its fraction.
            let floor = pynum::float_to_bigint(f.floor());
            let want = x.cmp(&floor).then(if f > f.floor() { Ordering::Less } else { Ordering::Equal });
            prop_assert_eq!(got, Some(want));
        }
    }

    #[test]
    fn true_division_matches_f64_when_exact(a in -(1i64 << 53)..(1i64 << 53), b in (-(1i64 << 53)..(1i64 << 53)).prop_filter("nonzero", |b| *b != 0)) {
        let (x, y) = (big(i128::from(a)), big(i128::from(b)));
        let got = pynum::true_div(view(&x), view(&y)).unwrap();
        prop_assert_eq!(got.to_bits(), (a as f64 / b as f64).to_bits());
    }

    #[test]
    fn text_round_trips(a in arb_int()) {
        let x = big(a);
        let text = pynum::big_to_string(&x).unwrap();
        prop_assert_eq!(text.clone(), a.to_string());
        let (negative, digits) = match text.strip_prefix('-') {
            Some(d) => (true, d.to_owned()),
            None => (false, text),
        };
        prop_assert_eq!(pynum::parse_signed_digits(negative, &digits, 10).unwrap(), x);
    }

    #[test]
    fn evaluator_results_are_normalised(a in arb_int(), b in arb_int()) {
        for op in ["+", "-", "*", "//", "%"] {
            if (op == "//" || op == "%") && b == 0 {
                continue;
            }
            let src = format!("({a}) {op} ({b})");
            let got = eval_expr(&src).unwrap();
            match &got {
                Value::Int(_) => {}
                Value::BigInt(n) => prop_assert!(i64::try_from(&**n).is_err(), "{src}: {got:?} should be an Int"),
                other => prop_assert!(false, "{src}: {other:?}"),
            }
        }
    }
}
