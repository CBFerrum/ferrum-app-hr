//! Salary formulas and conditions: CEL (the engine behind every Ferrum rule) over the period's
//! numbers. The metadata `Expr` only binds `doc`/`user`, while formulas name their inputs directly
//! (`base * 0.4`), so they are compiled here with their own variables.
//!
//! CEL refuses to mix integers and doubles, which would make `base * 12 / 100` an error; integer
//! literals are therefore read as decimals. Results are rounded to 6 places before they become
//! exact decimals, which removes binary floating-point noise from amounts of any realistic size.

use cel::{Context, Program, Value};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

/// Numbers every formula may use besides component abbreviations.
pub const NUMBERS: &[&str] = &["base", "variable", "gross", "payment_days", "working_days", "lwp_days", "absent_days"];
/// Names (titles) of the employee's organization, for conditions such as `grade == 'G2'`.
pub const TEXTS: &[&str] = &["designation", "department", "branch", "grade", "employment_type"];
const RESERVED: &[&str] = &[
    "true",
    "false",
    "null",
    "in",
    "as",
    "break",
    "const",
    "continue",
    "else",
    "for",
    "function",
    "if",
    "import",
    "let",
    "loop",
    "package",
    "namespace",
    "return",
    "var",
    "void",
    "while",
    "max",
    "min",
    "size",
    "int",
    "double",
    "string",
];

/// An abbreviation must work as a formula variable.
pub fn check_abbr(abbr: &str) -> Result<(), String> {
    let mut chars = abbr.chars();
    let ok_start = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !ok_start || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("must start with a letter or _ and contain only letters, digits and _".into());
    }
    if RESERVED.contains(&abbr) || NUMBERS.contains(&abbr) || TEXTS.contains(&abbr) {
        return Err(format!("'{abbr}' is reserved in formulas"));
    }
    Ok(())
}

/// Integer literals become doubles (`12` → `12.0`), leaving identifiers, strings, decimals,
/// exponents, hex and unsigned literals alone.
pub fn decimalize(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            // Copy the string literal verbatim (escapes included).
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == c {
                    break;
                }
            }
            continue;
        }
        let starts_word = i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '.');
        if c.is_ascii_digit() && starts_word {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let next = chars.get(i).copied();
            let integer = match next {
                Some('.') => !chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()),
                Some('e' | 'E' | 'x' | 'X' | 'u' | 'U') => false,
                Some(n) => !(n.is_ascii_alphanumeric() || n == '_'),
                None => true,
            };
            out.extend(&chars[start..i]);
            if integer {
                out.push_str(".0");
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The values a formula sees.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    pub numbers: BTreeMap<String, Decimal>,
    pub texts: BTreeMap<String, String>,
}

impl Vars {
    pub fn set(&mut self, name: &str, v: Decimal) {
        self.numbers.insert(name.to_string(), v);
    }

    /// Placeholder values for test evaluations at save time.
    pub fn sample(abbrs: &[String]) -> Vars {
        let mut v = Vars::default();
        for n in NUMBERS.iter().map(|s| s.to_string()).chain(abbrs.iter().cloned()) {
            v.numbers.insert(n, Decimal::ONE);
        }
        for t in TEXTS {
            v.texts.insert(t.to_string(), String::new());
        }
        v
    }
}

pub struct Formula {
    program: Program,
}

impl Formula {
    pub fn compile(src: &str) -> Result<Formula, String> {
        if src.trim().is_empty() {
            return Err("the formula is empty".into());
        }
        if src.len() > 4096 {
            return Err("the formula is longer than 4096 characters".into());
        }
        let program = Program::compile(&decimalize(src)).map_err(|e| e.to_string())?;
        Ok(Formula { program })
    }

    /// Variables the formula reads.
    pub fn variables(&self) -> Vec<String> {
        self.program.references().variables().into_iter().map(str::to_string).collect()
    }

    /// Names the formula reads that are neither known numbers/texts nor in `known`.
    pub fn unknown(&self, known: &[String]) -> Vec<String> {
        self.variables().into_iter().filter(|v| !NUMBERS.contains(&v.as_str()) && !TEXTS.contains(&v.as_str()) && !known.contains(v)).collect()
    }

    fn run(&self, vars: &Vars) -> Result<Value, String> {
        let mut ctx = Context::default();
        let _ = ctx.add_function("max", cel::functions::max);
        let _ = ctx.add_function("min", cel::functions::min);
        for (k, v) in &vars.numbers {
            ctx.add_variable_from_value(k.clone(), Value::Float(v.to_f64().unwrap_or_default()));
        }
        for (k, v) in &vars.texts {
            ctx.add_variable_from_value(k.clone(), Value::String(Arc::new(v.clone())));
        }
        self.program.execute(&ctx).map_err(|e| e.to_string())
    }

    pub fn amount(&self, vars: &Vars) -> Result<Decimal, String> {
        let f = match self.run(vars)? {
            Value::Float(f) => f,
            Value::Int(i) => i as f64,
            Value::UInt(u) => u as f64,
            Value::Null => 0.0,
            other => return Err(format!("the formula gave {other:?}, not a number")),
        };
        if !f.is_finite() {
            return Err("the formula did not give a finite number (division by zero?)".into());
        }
        Decimal::from_str(&format!("{f:.6}")).map_err(|_| "the amount is out of range".to_string())
    }

    pub fn holds(&self, vars: &Vars) -> Result<bool, String> {
        match self.run(vars)? {
            Value::Bool(b) => Ok(b),
            Value::Null => Ok(false),
            other => Err(format!("the condition gave {other:?}, not true or false")),
        }
    }
}

/// Annual tax on `income` under progressive slabs `(from, to (0 = no limit), percent)`.
pub fn slab_tax(income: Decimal, slabs: &[(Decimal, Decimal, Decimal)]) -> Decimal {
    let mut sorted = slabs.to_vec();
    sorted.sort_by_key(|a| a.0);
    sorted
        .iter()
        .filter(|(from, _, _)| income > *from)
        .map(|(from, to, pct)| {
            let top = if to.is_zero() { income } else { income.min(*to) };
            (top - from).max(Decimal::ZERO) * pct / Decimal::ONE_HUNDRED
        })
        .sum()
}

/// The year's tax on `income`: [`slab_tax`], less a rebate of up to `rebate.1` while the income
/// is at most `rebate.0` (0: no rebate), then each charge `(percent, min, max (0 = no limit))`
/// whose bounds hold the income, in order, as a percent of the tax so far (a surcharge, then the
/// cess on tax and surcharge).
pub fn annual_tax(income: Decimal, slabs: &[(Decimal, Decimal, Decimal)], rebate: (Decimal, Decimal), charges: &[(Decimal, Decimal, Decimal)]) -> Decimal {
    let mut tax = slab_tax(income, slabs);
    if !rebate.0.is_zero() && income <= rebate.0 {
        tax -= tax.min(rebate.1);
    }
    for (pct, min, max) in charges {
        if income >= *min && (max.is_zero() || income <= *max) {
            tax += tax * pct / Decimal::ONE_HUNDRED;
        }
    }
    tax
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn vars(pairs: &[(&str, &str)]) -> Vars {
        let mut v = Vars::default();
        for (k, x) in pairs {
            v.set(k, n(x));
        }
        v
    }

    #[test]
    fn integer_literals_become_decimals() {
        assert_eq!(decimalize("base * 12 / 100"), "base * 12.0 / 100.0");
        assert_eq!(decimalize("gross > 21000 ? 0 : gross * 0.0075"), "gross > 21000.0 ? 0.0 : gross * 0.0075");
        assert_eq!(decimalize("BS2 + x_1 * 1e3 + 0x1F + 7u + 2.5"), "BS2 + x_1 * 1e3 + 0x1F + 7u + 2.5");
        assert_eq!(decimalize("grade == 'G 2' ? 5 : 0"), "grade == 'G 2' ? 5.0 : 0.0");
        assert_eq!(decimalize("'it''s' + \"a\\\"5\""), "'it''s' + \"a\\\"5\"");
    }

    #[test]
    fn formulas_compute_exact_amounts() {
        let v = vars(&[("base", "50000"), ("gross", "21000"), ("BS", "25000")]);
        assert_eq!(Formula::compile("base * 0.4").unwrap().amount(&v).unwrap(), n("20000"));
        assert_eq!(Formula::compile("base * 12 / 100").unwrap().amount(&v).unwrap(), n("6000"));
        assert_eq!(Formula::compile("gross > 21000 ? 0 : gross * 0.0075").unwrap().amount(&v).unwrap(), n("157.5"));
        assert_eq!(Formula::compile("min(BS, 15000) * 0.12").unwrap().amount(&v).unwrap(), n("1800"));
        assert_eq!(Formula::compile("max(base - BS * 3, 0)").unwrap().amount(&v).unwrap(), Decimal::ZERO);
        assert_eq!(Formula::compile("33333.33 * 3").unwrap().amount(&v).unwrap(), n("99999.99"));
        assert!(Formula::compile("base / 0").unwrap().amount(&v).is_err());
        let mut t = v.clone();
        t.texts.insert("grade".into(), "G2".into());
        assert!(Formula::compile("grade == 'G2' && base > 1000").unwrap().holds(&t).unwrap());
    }

    #[test]
    fn unknown_names_are_found_at_save() {
        let f = Formula::compile("BS * 0.4 + HRA_X + payment_days").unwrap();
        assert_eq!(f.unknown(&["BS".to_string()]), vec!["HRA_X".to_string()]);
        assert!(Formula::compile("base *").is_err());
        assert!(check_abbr("BS").is_ok());
        assert!(check_abbr("base").is_err() && check_abbr("2X").is_err() && check_abbr("in").is_err() && check_abbr("a-b").is_err());
    }

    /// India's new regime for FY 2025-26, as the Income Tax Department publishes it
    /// (tests/fixtures/india-new-regime-fy2025-26.json): the slabs' printed amounts at each bound,
    /// the section 87A rebate up to 12 lakh, and the 4% cess.
    #[test]
    fn india_new_regime_fy2025_26() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/india-new-regime-fy2025-26.json")).unwrap();
        let num = |v: &serde_json::Value| Decimal::from_str(&v.to_string()).unwrap();
        let slabs: Vec<_> = fixture["slabs"].as_array().unwrap().iter().map(|s| (num(&s["from"]), num(&s["to"]), num(&s["percent"]))).collect();
        let rebate = (num(&fixture["rebate"]["limit"]), num(&fixture["rebate"]["max"]));
        let charges: Vec<_> = fixture["charges"].as_array().unwrap().iter().map(|c| (num(&c["percent"]), Decimal::ZERO, Decimal::ZERO)).collect();
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 8);
        for c in cases {
            let income = num(&c["income"]);
            assert_eq!(slab_tax(income, &slabs), num(&c["slab_tax"]), "the slabs at {income}");
            assert_eq!(annual_tax(income, &slabs, rebate, &charges), num(&c["tax"]), "the tax at {income}");
        }
    }

    #[test]
    fn charges_in_order() {
        let slabs = [(n("0"), n("0"), n("10"))];
        let (surcharge, cess) = ((n("10"), n("5000000"), n("10000000")), (n("4"), n("0"), n("0")));
        assert_eq!(annual_tax(n("6000000"), &slabs, (n("0"), n("0")), &[surcharge, cess]), n("686400"), "cess on tax and surcharge");
        assert_eq!(annual_tax(n("4000000"), &slabs, (n("0"), n("0")), &[surcharge, cess]), n("416000"), "no surcharge below its bound");
        assert_eq!(annual_tax(n("20000000"), &slabs, (n("0"), n("0")), &[surcharge, cess]), n("2080000"), "nor above it");
        assert_eq!(annual_tax(n("100000"), &slabs, (n("0"), n("50000")), &[]), n("10000"), "no rebate without a limit");
        assert_eq!(annual_tax(n("800000"), &slabs, (n("1000000"), n("50000")), &[]), n("30000"), "the rebate takes no more than its most");
    }

    #[test]
    fn progressive_slabs() {
        let slabs = [(n("0"), n("300000"), n("0")), (n("300000"), n("700000"), n("5")), (n("700000"), n("0"), n("10"))];
        assert_eq!(slab_tax(n("250000"), &slabs), Decimal::ZERO);
        assert_eq!(slab_tax(n("500000"), &slabs), n("10000"));
        assert_eq!(slab_tax(n("1000000"), &slabs), n("50000"));
    }
}
