//! Python's `complex`, following CPython 3.13's `complexobject.c`.
//!
//! The arithmetic is [`Complex64`] with CPython's own algorithms (Smith's
//! division, binary exponentiation for small integer powers, the polar form
//! otherwise) rather than `num-complex`'s, so the last digit of a result
//! matches.

pub use num_complex::Complex64;

use super::NumError;
use crate::pyfmt::py_float_repr;

/// `a * b` (`_Py_c_prod`).
pub fn prod(a: Complex64, b: Complex64) -> Complex64 {
    Complex64::new(a.re * b.re - a.im * b.im, a.re * b.im + a.im * b.re)
}

/// `a / b` (`_Py_c_quot`).
///
/// # Errors
/// `ZeroDivisionError` for a zero divisor.
pub fn quot(a: Complex64, b: Complex64) -> Result<Complex64, NumError> {
    let abs_re = b.re.abs();
    let abs_im = b.im.abs();
    if abs_re >= abs_im {
        if abs_re == 0.0 {
            return Err(NumError::ZeroDivision("division by zero"));
        }
        let ratio = b.im / b.re;
        let denom = b.re + b.im * ratio;
        Ok(Complex64::new(
            (a.re + a.im * ratio) / denom,
            (a.im - a.re * ratio) / denom,
        ))
    } else if abs_im >= abs_re {
        let ratio = b.re / b.im;
        let denom = b.re * ratio + b.im;
        Ok(Complex64::new(
            (a.re * ratio + a.im) / denom,
            (a.im * ratio - a.re) / denom,
        ))
    } else {
        // A NaN component.
        Ok(Complex64::new(f64::NAN, f64::NAN))
    }
}

/// `abs(z)`: `hypot`, with the `OverflowError` CPython raises when the result
/// overflows although the parts are finite.
///
/// # Errors
/// `OverflowError` ("absolute value too large").
pub fn abs(z: Complex64) -> Result<f64, NumError> {
    let r = z.re.hypot(z.im);
    if r.is_infinite() && z.re.is_finite() && z.im.is_finite() {
        return Err(NumError::Overflow("absolute value too large"));
    }
    Ok(r)
}

/// `z ** w` (`complex_pow`).
///
/// # Errors
/// `ZeroDivisionError` for zero to a negative or complex power, `OverflowError`
/// when a component of the result is infinite.
pub fn pow(z: Complex64, w: Complex64) -> Result<Complex64, NumError> {
    let (result, edom) = if w.im == 0.0 && w.re == w.re.floor() && w.re.abs() <= 100.0 {
        powi(z, w.re as i64)
    } else {
        c_pow(z, w)
    };
    if edom {
        return Err(NumError::ZeroDivision(
            "zero to a negative or complex power",
        ));
    }
    // `_Py_ADJUST_ERANGE2`: an infinite component is an overflow.
    if result.re.is_infinite() || result.im.is_infinite() {
        return Err(NumError::Overflow("complex exponentiation"));
    }
    Ok(result)
}

/// `_Py_c_pow`: the second element is true for `EDOM` (zero to a negative or
/// complex power).
fn c_pow(a: Complex64, b: Complex64) -> (Complex64, bool) {
    if b.re == 0.0 && b.im == 0.0 {
        return (Complex64::new(1.0, 0.0), false);
    }
    if a.re == 0.0 && a.im == 0.0 {
        return (Complex64::new(0.0, 0.0), b.im != 0.0 || b.re < 0.0);
    }
    let vabs = a.re.hypot(a.im);
    let mut len = vabs.powf(b.re);
    let at = a.im.atan2(a.re);
    let mut phase = at * b.re;
    if b.im != 0.0 {
        len /= (at * b.im).exp();
        phase += b.im * vabs.ln();
    }
    (Complex64::new(len * phase.cos(), len * phase.sin()), false)
}

/// `c_powu`: binary exponentiation by squaring.
fn powu(x: Complex64, n: u64) -> Complex64 {
    let mut r = Complex64::new(1.0, 0.0);
    let mut p = x;
    let mut mask = 1u64;
    while mask > 0 && n >= mask {
        if n & mask != 0 {
            r = prod(r, p);
        }
        mask <<= 1;
        p = prod(p, p);
    }
    r
}

/// `c_powi`: an integer power, a quotient `1 / x**n` for a negative one (whose
/// division by zero is the `EDOM` of the second element).
fn powi(x: Complex64, n: i64) -> (Complex64, bool) {
    if n > 0 {
        (powu(x, n as u64), false)
    } else {
        match quot(Complex64::new(1.0, 0.0), powu(x, n.unsigned_abs())) {
            Ok(z) => (z, false),
            Err(_) => (Complex64::new(0.0, 0.0), true),
        }
    }
}

/// `repr(z)` and `str(z)`.
pub fn repr(z: Complex64) -> String {
    let part = |x: f64| {
        let text = py_float_repr(x);
        match text.strip_suffix(".0") {
            Some(stripped) => stripped.to_owned(),
            None => text,
        }
    };
    let signed = |x: f64| {
        let text = part(x);
        if text.starts_with('-') {
            text
        } else if x.is_nan() && x.is_sign_negative() {
            format!("-{text}")
        } else {
            format!("+{text}")
        }
    };
    if z.re == 0.0 && z.re.is_sign_positive() {
        return format!("{}j", part(z.im));
    }
    format!("({}{}j)", part(z.re), signed(z.im))
}

/// `bool(z)`.
pub fn is_truthy(z: Complex64) -> bool {
    z.re != 0.0 || z.im != 0.0
}

/// `z == w`.
pub fn eq(z: Complex64, w: Complex64) -> bool {
    z.re == w.re && z.im == w.im
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(re: f64, im: f64) -> Complex64 {
        Complex64::new(re, im)
    }

    #[test]
    fn repr_follows_cpython() {
        assert_eq!(repr(c(1.0, 2.0)), "(1+2j)");
        assert_eq!(repr(c(0.0, 1.0)), "1j");
        assert_eq!(repr(c(-0.0, 1.0)), "(-0+1j)");
        assert_eq!(repr(c(1.5, -2.5)), "(1.5-2.5j)");
        assert_eq!(repr(c(1e16, 1e-5)), "(1e+16+1e-05j)");
        assert_eq!(repr(c(0.0, 0.0)), "0j");
        assert_eq!(repr(c(0.0, -0.0)), "-0j");
        assert_eq!(repr(c(f64::INFINITY, f64::NAN)), "(inf+nanj)");
        assert_eq!(repr(c(1.0, f64::NEG_INFINITY)), "(1-infj)");
    }

    #[test]
    fn power_of_a_negative_base() {
        // `(-8) ** 0.5` is `(1.7319121124709868e-16+2.8284271247461903j)`.
        let z = pow(c(-8.0, 0.0), c(0.5, 0.0)).unwrap();
        assert_eq!(z.re, 1.7319121124709868e-16);
        assert_eq!(z.im, 2.8284271247461903);
        let w = pow(c(1.0, 2.0), c(2.0, 0.0)).unwrap();
        assert_eq!((w.re, w.im), (-3.0, 4.0));
    }

    #[test]
    fn division_and_errors() {
        assert!(matches!(
            quot(c(1.0, 0.0), c(0.0, 0.0)),
            Err(NumError::ZeroDivision(_))
        ));
        assert!(matches!(
            pow(c(0.0, 0.0), c(-1.0, 0.0)),
            Err(NumError::ZeroDivision(_))
        ));
    }
}
