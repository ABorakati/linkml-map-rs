#![no_main]
//! `Decimal::parse_python` and everything done with a parsed decimal.

use fuzz_support::{UNGUARDED, sci_exponent};
use libfuzzer_sys::fuzz_target;
use linkml_map_core::decimal::Decimal;

fuzz_target!(|text: &str| {
    let Ok(d) = Decimal::parse_python(text) else {
        return;
    };
    let sci = d.to_sci_string();
    // parse -> str -> parse is the identity.
    let again = Decimal::parse_python(&sci).expect("str(Decimal) parses");
    assert_eq!(d, again);
    assert_eq!(sci, again.to_sci_string());
    let huge = sci_exponent(&sci).is_some_and(|e| e.unsigned_abs() > 1 << 24);
    // Exponents near i32::MAX overflow normalize/cmp (reported) and the fixed-point
    // spelling would allocate the full expansion.
    if huge && !UNGUARDED {
        return;
    }
    let _ = d.to_f64();
    let _ = d.normalize();
    let _ = d.cmp(&Decimal::from_i64(1));
    let _ = d.cmp_f64(1.5);
    let fixed = d.to_fixed_string();
    assert_eq!(d, Decimal::parse_python(&fixed).expect("fixed spelling parses"));
});
