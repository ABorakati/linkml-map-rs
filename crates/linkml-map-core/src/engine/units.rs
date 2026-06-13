//! Self-contained dimensional unit conversion.
//!
//! This is the Rust analogue of `linkml_map.functions.unit_conversion`, which
//! in Python delegates to `pint` + `ucumvert` (UCUM). Pulling those in would
//! mean a heavyweight optional dependency and a UCUM grammar; for the clinical
//! / SI units that dominate OMOP Measurement mapping a fixed dimensional factor
//! table is both sufficient and dependency-free.
//!
//! # Model
//! Every known unit is mapped to a [`UnitSpec`]: a dimension tag plus an affine
//! transform to a canonical base unit of that dimension:
//!
//! ```text
//! base_value = value * factor + offset
//! ```
//!
//! Conversion between two units of the *same* dimension is therefore
//!
//! ```text
//! to_value = (value * from.factor + from.offset - to.offset) / to.factor
//! ```
//!
//! `factor`/`offset` are exact for linear units (offset 0) and carry the
//! freezing-point offset for temperature.
//!
//! # Deliberate non-goals (return `None`)
//! - **Unknown / unparseable units** — the caller leaves the value unchanged.
//! - **Cross-dimension conversion** (e.g. `mg` → `mL`).
//! - **Molar ↔ mass conversion** (`mmol/L` ↔ `mg/dL`). These require an
//!   analyte-specific molecular weight that is not present in the unit token,
//!   so — like Python `pint` without a substance context — we return `None`.
//!   `mol`-family and `g`-family ratios are each internally consistent
//!   (`mmol/L` → `umol/L`, `mg/dL` → `g/L`) but never bridged across.
//!
//! UCUM-style tokens are accepted: bracketed units (`mm[Hg]`, `m[H2O]`),
//! `Cel`/`degF`/`K` temperature spellings, and simple `num/den` ratios
//! (`mg/dL`, `g/m2`).  Pure unit *annotations* in `{...}` (e.g. `{Cre}`) are
//! stripped, matching the Python behaviour where they carry no dimension.

/// A physical dimension. Conversion is only defined within one dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    Length,
    Mass,
    Volume,
    Temperature,
    Time,
    Pressure,
    AmountOfSubstance,
    /// A ratio `numerator_dimension / denominator_dimension`.
    Ratio(RatioDim),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RatioDim {
    num: ScalarDim,
    den: ScalarDim,
}

/// The non-ratio dimensions usable as numerator/denominator of a ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarDim {
    Length,
    Mass,
    Volume,
    AmountOfSubstance,
}

/// Affine spec for one unit: `base = value * factor + offset`.
#[derive(Debug, Clone, Copy)]
struct UnitSpec {
    dimension: Dimension,
    factor: f64,
    offset: f64,
}

impl UnitSpec {
    const fn linear(dimension: Dimension, factor: f64) -> Self {
        Self {
            dimension,
            factor,
            offset: 0.0,
        }
    }
    const fn affine(dimension: Dimension, factor: f64, offset: f64) -> Self {
        Self {
            dimension,
            factor,
            offset,
        }
    }
}

/// Strip a leading/trailing UCUM annotation in braces, e.g. `mmol{Cre}` →
/// `mmol`, and surrounding whitespace.
fn strip_annotation(tok: &str) -> &str {
    let tok = tok.trim();
    // Drop a trailing `{...}` annotation if present.
    if let Some(open) = tok.find('{') {
        if tok.ends_with('}') {
            return tok[..open].trim();
        }
    }
    tok
}

/// Resolve a single (non-ratio) scalar unit token to its spec.
///
/// Returns `None` for unknown tokens.
fn scalar_spec(tok: &str) -> Option<UnitSpec> {
    let t = strip_annotation(tok);
    use Dimension as D;
    let spec = match t {
        // ── Length (base: metre) ──────────────────────────────────────────
        "m" | "meter" | "metre" => UnitSpec::linear(D::Length, 1.0),
        "km" => UnitSpec::linear(D::Length, 1000.0),
        "cm" => UnitSpec::linear(D::Length, 0.01),
        "mm" => UnitSpec::linear(D::Length, 0.001),
        "um" | "µm" | "micrometer" => UnitSpec::linear(D::Length, 1e-6),
        "nm" => UnitSpec::linear(D::Length, 1e-9),
        "in" | "[in_i]" | "inch" => UnitSpec::linear(D::Length, 0.0254),
        "ft" | "[ft_i]" | "foot" => UnitSpec::linear(D::Length, 0.3048),

        // ── Mass (base: gram) ─────────────────────────────────────────────
        "g" | "gram" => UnitSpec::linear(D::Mass, 1.0),
        "kg" => UnitSpec::linear(D::Mass, 1000.0),
        "mg" => UnitSpec::linear(D::Mass, 0.001),
        "ug" | "µg" | "mcg" => UnitSpec::linear(D::Mass, 1e-6),
        "ng" => UnitSpec::linear(D::Mass, 1e-9),
        "pg" => UnitSpec::linear(D::Mass, 1e-12),

        // ── Volume (base: litre) ──────────────────────────────────────────
        "L" | "l" | "liter" | "litre" => UnitSpec::linear(D::Volume, 1.0),
        "dL" | "dl" => UnitSpec::linear(D::Volume, 0.1),
        "cL" | "cl" => UnitSpec::linear(D::Volume, 0.01),
        "mL" | "ml" => UnitSpec::linear(D::Volume, 0.001),
        "uL" | "ul" | "µL" => UnitSpec::linear(D::Volume, 1e-6),
        "nL" | "nl" => UnitSpec::linear(D::Volume, 1e-9),

        // ── Temperature (base: kelvin) ────────────────────────────────────
        // base = value*factor + offset  (offset is the value at 0 of this unit)
        "K" | "kelvin" => UnitSpec::affine(D::Temperature, 1.0, 0.0),
        "Cel" | "degC" | "celsius" | "°C" => UnitSpec::affine(D::Temperature, 1.0, 273.15),
        "degF" | "fahrenheit" | "°F" => {
            UnitSpec::affine(D::Temperature, 5.0 / 9.0, 273.15 - 32.0 * 5.0 / 9.0)
        }

        // ── Time (base: second) ───────────────────────────────────────────
        "s" | "sec" | "second" => UnitSpec::linear(D::Time, 1.0),
        "ms" => UnitSpec::linear(D::Time, 0.001),
        "min" => UnitSpec::linear(D::Time, 60.0),
        "h" | "hr" | "hour" => UnitSpec::linear(D::Time, 3600.0),
        "d" | "day" => UnitSpec::linear(D::Time, 86400.0),
        "wk" | "week" => UnitSpec::linear(D::Time, 604800.0),

        // ── Pressure (base: pascal) ───────────────────────────────────────
        "Pa" | "pascal" => UnitSpec::linear(D::Pressure, 1.0),
        "kPa" => UnitSpec::linear(D::Pressure, 1000.0),
        "hPa" => UnitSpec::linear(D::Pressure, 100.0),
        "mbar" => UnitSpec::linear(D::Pressure, 100.0),
        "bar" => UnitSpec::linear(D::Pressure, 100000.0),
        "mm[Hg]" | "mmHg" => UnitSpec::linear(D::Pressure, 133.322_387_415),
        "cm[H2O]" | "cmH2O" => UnitSpec::linear(D::Pressure, 98.0665),

        // ── Amount of substance (base: mole) ──────────────────────────────
        "mol" | "mole" => UnitSpec::linear(D::AmountOfSubstance, 1.0),
        "mmol" => UnitSpec::linear(D::AmountOfSubstance, 0.001),
        "umol" | "µmol" => UnitSpec::linear(D::AmountOfSubstance, 1e-6),
        "nmol" => UnitSpec::linear(D::AmountOfSubstance, 1e-9),
        "pmol" => UnitSpec::linear(D::AmountOfSubstance, 1e-12),

        _ => return None,
    };
    Some(spec)
}

/// Map a non-ratio dimension to its [`ScalarDim`] (for ratio composition).
/// Temperature/time/pressure/amount cannot meaningfully appear as a ratio leg
/// here (we don't model molar mass), so they map to `None`.
fn scalar_dim_of(dim: Dimension) -> Option<ScalarDim> {
    Some(match dim {
        Dimension::Length => ScalarDim::Length,
        Dimension::Mass => ScalarDim::Mass,
        Dimension::Volume => ScalarDim::Volume,
        Dimension::AmountOfSubstance => ScalarDim::AmountOfSubstance,
        _ => return None,
    })
}

/// Resolve any unit token — scalar or simple `num/den` ratio — to a spec.
fn unit_spec(tok: &str) -> Option<UnitSpec> {
    let t = strip_annotation(tok);
    if let Some((num_s, den_s)) = split_ratio(t) {
        let num = scalar_spec(num_s)?;
        let den = scalar_spec(den_s)?;
        let num_dim = scalar_dim_of(num.dimension)?;
        let den_dim = scalar_dim_of(den.dimension)?;
        // A ratio is affine-free: combined factor = num.factor / den.factor.
        // (Both legs are linear; we never build a ratio from temperature.)
        return Some(UnitSpec::linear(
            Dimension::Ratio(RatioDim {
                num: num_dim,
                den: den_dim,
            }),
            num.factor / den.factor,
        ));
    }
    scalar_spec(t)
}

/// Split a `num/den` ratio token. Only a single `/` is supported.
/// Returns `None` if there is no `/` or more than one.
fn split_ratio(tok: &str) -> Option<(&str, &str)> {
    let mut parts = tok.splitn(2, '/');
    let num = parts.next()?;
    let den = parts.next()?;
    if den.contains('/') {
        return None;
    }
    if num.is_empty() || den.is_empty() {
        return None;
    }
    Some((num, den))
}

/// Convert `value` from `from_unit` to `to_unit`.
///
/// Returns `Some(converted)` when both units are known and dimensionally
/// compatible; returns `None` for unknown units, cross-dimension conversion,
/// or molar↔mass conversion (see module docs). Identity (`from == to`) returns
/// the value unchanged.
pub fn convert(value: f64, from_unit: &str, to_unit: &str) -> Option<f64> {
    if from_unit == to_unit {
        return Some(value);
    }
    let from = unit_spec(from_unit)?;
    let to = unit_spec(to_unit)?;
    if from.dimension != to.dimension {
        return None;
    }
    // base = value*from.factor + from.offset; then invert through `to`.
    let base = value * from.factor + from.offset;
    Some((base - to.offset) / to.factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
    }

    #[test]
    fn length_cm_to_m() {
        approx(convert(100.0, "cm", "m").unwrap(), 1.0);
        approx(convert(1.0, "m", "cm").unwrap(), 100.0);
    }

    #[test]
    fn length_inch_to_cm() {
        approx(convert(1.0, "in", "cm").unwrap(), 2.54);
    }

    #[test]
    fn mass_mg_to_g() {
        approx(convert(2500.0, "mg", "g").unwrap(), 2.5);
        approx(convert(1.0, "kg", "g").unwrap(), 1000.0);
    }

    #[test]
    fn temperature_celsius_to_fahrenheit() {
        approx(convert(0.0, "Cel", "degF").unwrap(), 32.0);
        approx(convert(100.0, "Cel", "degF").unwrap(), 212.0);
        approx(convert(37.0, "Cel", "degF").unwrap(), 98.6);
    }

    #[test]
    fn temperature_celsius_to_kelvin() {
        approx(convert(0.0, "Cel", "K").unwrap(), 273.15);
    }

    #[test]
    fn pressure_mmhg_to_kpa() {
        // 760 mmHg ≈ 101.325 kPa
        approx(convert(760.0, "mm[Hg]", "kPa").unwrap(), 101.325_014);
        approx(convert(1.0, "mmHg", "Pa").unwrap(), 133.322_387);
    }

    #[test]
    fn ratio_mgdl_to_gl() {
        // 1 mg/dL = 0.01 g/L
        approx(convert(1.0, "mg/dL", "g/L").unwrap(), 0.01);
        // 100 mg/dL = 1 g/L
        approx(convert(100.0, "mg/dL", "g/L").unwrap(), 1.0);
    }

    #[test]
    fn ratio_mmol_per_l_to_umol_per_l() {
        approx(convert(1.0, "mmol/L", "umol/L").unwrap(), 1000.0);
    }

    #[test]
    fn unknown_unit_is_none() {
        assert!(convert(1.0, "smoots", "m").is_none());
        assert!(convert(1.0, "m", "frobnitz").is_none());
    }

    #[test]
    fn cross_dimension_is_none() {
        assert!(convert(1.0, "mg", "mL").is_none());
        assert!(convert(1.0, "Cel", "m").is_none());
    }

    #[test]
    fn molar_to_mass_is_none() {
        // mmol/L → mg/dL needs a molecular weight; we refuse.
        assert!(convert(5.0, "mmol/L", "mg/dL").is_none());
    }

    #[test]
    fn identity_passthrough() {
        approx(convert(42.5, "mg/dL", "mg/dL").unwrap(), 42.5);
    }

    #[test]
    fn annotation_is_stripped() {
        // nmol/mmol{Cre} dimension matches nmol/mmol
        approx(convert(1.0, "nmol/mmol{Cre}", "nmol/mmol").unwrap(), 1.0);
    }
}
