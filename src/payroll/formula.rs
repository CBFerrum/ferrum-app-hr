//! Salary formulas and conditions: CEL (the engine behind every Ferrum rule) over the period's
//! numbers. The metadata `Expr` only binds `doc`/`user`, while formulas name their inputs directly
//! (`base * 0.4`), so they are compiled here with their own variables.
//!
//! CEL refuses to mix integers and doubles, which would make `base * 12 / 100` an error; integer
//! literals are therefore read as decimals. Results are rounded to 6 places before they become
//! exact decimals, which removes binary floating-point noise from amounts of any realistic size.
//!
//! Besides those numbers a formula may read what statutory rules need, with no country in the code
//! ([`CONTEXT`]): `employee.<field>` and `location.<field>` (the employee's and their branch's fields, a
//! workspace's own included), `period` (`start`, `end`, `month`, `year`, `number` in the payroll year, `count`
//! of periods in it, `left` of them from this one on, `days`), `ytd.<abbr>` and `ytd_total` (`gross`,
//! `deductions`, `net`, `employer`, `taxable` and `tax`, the last two with the year's opening figures) from the
//! slips of the payroll year before this one, `previous.<abbr>` (the previous slip), `declared.<code>` and
//! `verified.<code>` (what the employee declared for the payroll year by exemption category code, and of it
//! what HR verified), and the functions of [`FUNCTIONS`] over the administrator's lookup tables.

use cel::{Context, Program, Value};
use chrono::{Datelike, NaiveDate};
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

/// What statutory formulas read besides the numbers: records' fields, the period, year-to-date amounts and
/// declarations (maps), filled only for formulas that name them.
pub const CONTEXT: &[&str] = &["employee", "location", "period", "ytd", "ytd_total", "previous", "declared", "verified"];
/// `lookup(table, column, x, key1?, key2?)`: the `rate` or `amount` of the lookup table's row in force (the
/// latest `effective_from` on or before the period's end among the rows whose keys fit) whose band
/// `[from_amount, to_amount)` holds `x` (`to_amount` 0: no limit); a row's empty key fits any, a key not given
/// fits any row; 0 when none. `banded(table, x, key1?, key2?)`: the progressive sum over those rows' bands of
/// the part of `x` in each times its rate (percent). `years_between(a, b)`, `months_between(a, b)`: whole
/// years or months from date `a` to date `b` (`YYYY-MM-DD`).
pub const FUNCTIONS: &[&str] = &["lookup", "banded", "years_between", "months_between"];

/// One row of an administrator's lookup table, as a formula reads it.
#[derive(Debug, Clone)]
pub struct LookupRow {
    pub table: String,
    pub effective_from: NaiveDate,
    pub key1: String,
    pub key2: String,
    pub from: f64,
    pub to: f64,
    pub rate: f64,
    pub amount: f64,
}

/// The lookup tables' rows and the day they are read on.
#[derive(Debug, Clone, Default)]
pub struct Tables {
    pub on: Option<NaiveDate>,
    pub rows: Vec<LookupRow>,
}

impl Tables {
    /// The rows of `table` in force whose keys fit.
    fn rows(&self, table: &str, keys: &[String]) -> Vec<&LookupRow> {
        let fits = |r: &&LookupRow| {
            r.table.eq_ignore_ascii_case(table)
                && self.on.is_none_or(|on| r.effective_from <= on)
                && [&r.key1, &r.key2]
                    .iter()
                    .zip(keys.iter().map(Some).chain(std::iter::repeat(None)))
                    .all(|(have, want)| have.is_empty() || want.is_none_or(|w| have.eq_ignore_ascii_case(w)))
        };
        let candidates: Vec<&LookupRow> = self.rows.iter().filter(fits).collect();
        let Some(latest) = candidates.iter().map(|r| r.effective_from).max() else { return vec![] };
        let mut out: Vec<&LookupRow> = candidates.into_iter().filter(|r| r.effective_from == latest).collect();
        out.sort_by(|a, b| a.from.total_cmp(&b.from));
        out
    }

    fn lookup(&self, table: &str, column: &str, x: f64, keys: &[String]) -> Result<f64, String> {
        let row = self.rows(table, keys).into_iter().rev().find(|r| x >= r.from && (r.to == 0.0 || x < r.to));
        match column {
            "rate" => Ok(row.map_or(0.0, |r| r.rate)),
            "amount" => Ok(row.map_or(0.0, |r| r.amount)),
            other => Err(format!("lookup reads the column rate or amount, not {other}")),
        }
    }

    fn banded(&self, table: &str, x: f64, keys: &[String]) -> f64 {
        self.rows(table, keys).iter().filter(|r| x > r.from).map(|r| ((if r.to == 0.0 { x } else { x.min(r.to) }) - r.from).max(0.0) * r.rate / 100.0).sum()
    }
}

/// An error of a function of ours.
fn fail<E: ToString>(e: E) -> cel::ExecutionError {
    cel::ExecutionError::function_error("formula", e)
}

/// A key given to a lookup as text: a whole number without its decimals (`3`, not `3.0`).
fn key_text(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.to_string()),
        Value::Float(f) if f.fract() == 0.0 => Ok(format!("{}", *f as i64)),
        Value::Float(f) => Ok(f.to_string()),
        Value::Int(i) => Ok(i.to_string()),
        Value::UInt(u) => Ok(u.to_string()),
        Value::Null => Ok(String::new()),
        other => Err(format!("a lookup key is text or a number, not {other:?}")),
    }
}

fn num(v: &Value) -> Result<f64, String> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        Value::UInt(u) => Ok(*u as f64),
        Value::Null => Ok(0.0),
        other => Err(format!("expected a number, got {other:?}")),
    }
}

fn date_arg(v: &Value) -> Result<NaiveDate, String> {
    match v {
        Value::String(s) => NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d").map_err(|_| format!("'{s}' is not a date")),
        other => Err(format!("expected a date, got {other:?}")),
    }
}

/// JSON (numbers as decimals) as a CEL value.
pub fn to_cel(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Float(n.as_f64().unwrap_or_default()),
        serde_json::Value::String(s) => Value::String(Arc::new(s.clone())),
        serde_json::Value::Array(a) => Value::List(Arc::new(a.iter().map(to_cel).collect())),
        serde_json::Value::Object(o) => {
            let m: std::collections::HashMap<cel::objects::Key, Value> =
                o.iter().map(|(k, v)| (cel::objects::Key::String(Arc::new(k.clone())), to_cel(v))).collect();
            Value::Map(cel::objects::Map { map: Arc::new(m) })
        }
    }
}

/// An abbreviation must work as a formula variable.
pub fn check_abbr(abbr: &str) -> Result<(), String> {
    let mut chars = abbr.chars();
    let ok_start = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !ok_start || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("must start with a letter or _ and contain only letters, digits and _".into());
    }
    if RESERVED.contains(&abbr) || NUMBERS.contains(&abbr) || TEXTS.contains(&abbr) || CONTEXT.contains(&abbr) || FUNCTIONS.contains(&abbr) {
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
    /// The context maps ([`CONTEXT`]) a formula reads, when loaded.
    pub maps: BTreeMap<String, serde_json::Value>,
    pub tables: Arc<Tables>,
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

    /// Names the formula reads that are neither known numbers/texts/context nor in `known`.
    pub fn unknown(&self, known: &[String]) -> Vec<String> {
        self.variables()
            .into_iter()
            .filter(|v| !NUMBERS.contains(&v.as_str()) && !TEXTS.contains(&v.as_str()) && !CONTEXT.contains(&v.as_str()) && !known.contains(v))
            .collect()
    }

    /// Whether it reads the context or calls its functions (which a slip then loads); `known` (abbreviations)
    /// shadow context names a component took before they were reserved.
    pub fn uses_context(&self, known: &[String]) -> bool {
        let refs = self.program.references();
        self.variables().iter().any(|v| CONTEXT.contains(&v.as_str()) && !known.contains(v)) || FUNCTIONS.iter().any(|f| refs.has_function(f))
    }

    fn run(&self, vars: &Vars) -> Result<Value, String> {
        let mut ctx = Context::default();
        let _ = ctx.add_function("max", cel::functions::max);
        let _ = ctx.add_function("min", cel::functions::min);
        let _ = ctx.add_function("years_between", |a: Value, b: Value| -> std::result::Result<Value, cel::ExecutionError> {
            let (a, b) = (date_arg(&a).map_err(fail)?, date_arg(&b).map_err(fail)?);
            Ok(Value::Float(b.years_since(a).map_or_else(|| -(a.years_since(b).unwrap_or(0) as f64), |y| y as f64)))
        });
        let _ = ctx.add_function("months_between", |a: Value, b: Value| -> std::result::Result<Value, cel::ExecutionError> {
            let (a, b) = (date_arg(&a).map_err(fail)?, date_arg(&b).map_err(fail)?);
            let mut m = (b.year() - a.year()) * 12 + b.month() as i32 - a.month() as i32;
            if m > 0 && b.day() < a.day() {
                m -= 1;
            } else if m < 0 && b.day() > a.day() {
                m += 1;
            }
            Ok(Value::Float(m as f64))
        });
        let tables = vars.tables.clone();
        let _ = ctx.add_function(
            "lookup",
            move |cel::extractors::Arguments(args): cel::extractors::Arguments| -> std::result::Result<Value, cel::ExecutionError> {
                let err = fail::<String>;
                if !(3..=5).contains(&args.len()) {
                    return Err(fail("lookup takes a table, a column, a value and up to two keys"));
                }
                let (Value::String(table), Value::String(column)) = (&args[0], &args[1]) else { return Err(fail("lookup's table and column are texts")) };
                let keys = args[3..].iter().map(key_text).collect::<std::result::Result<Vec<_>, _>>().map_err(err)?;
                tables.lookup(table, column, num(&args[2]).map_err(err)?, &keys).map(Value::Float).map_err(err)
            },
        );
        let tables = vars.tables.clone();
        let _ = ctx.add_function(
            "banded",
            move |cel::extractors::Arguments(args): cel::extractors::Arguments| -> std::result::Result<Value, cel::ExecutionError> {
                let err = fail::<String>;
                if !(2..=4).contains(&args.len()) {
                    return Err(fail("banded takes a table, a value and up to two keys"));
                }
                let Value::String(table) = &args[0] else { return Err(fail("banded's table is a text")) };
                let keys = args[2..].iter().map(key_text).collect::<std::result::Result<Vec<_>, _>>().map_err(err)?;
                Ok(Value::Float(tables.banded(table, num(&args[1]).map_err(err)?, &keys)))
            },
        );
        for (k, v) in &vars.maps {
            ctx.add_variable_from_value(k.clone(), to_cel(v));
        }
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

    fn row(table: &str, from: &str, keys: (&str, &str), band: (f64, f64), rate: f64, amount: f64) -> LookupRow {
        LookupRow {
            table: table.into(),
            effective_from: from.parse().unwrap(),
            key1: keys.0.into(),
            key2: keys.1.into(),
            from: band.0,
            to: band.1,
            rate,
            amount,
        }
    }

    /// Statutory rules as configuration only, with made-up names and numbers: each is a formula over the context
    /// and the administrator's lookup tables, nothing of it in code.
    #[test]
    fn statutory_rules_are_formulas_over_tables() {
        let tables = Tables {
            on: Some("2026-05-31".parse().unwrap()),
            rows: vec![
                // A fund's wage ceiling, changed for the year from April 2026.
                row("fund_ceiling", "2025-04-01", ("", ""), (0.0, 0.0), 0.0, 5000.0),
                row("fund_ceiling", "2026-04-01", ("", ""), (0.0, 0.0), 0.0, 6000.0),
                // A wage tax by bands of annual income.
                row("wage_tax", "2026-01-01", ("", ""), (0.0, 30000.0), 0.0, 0.0),
                row("wage_tax", "2026-01-01", ("", ""), (30000.0, 60000.0), 10.0, 0.0),
                row("wage_tax", "2026-01-01", ("", ""), (60000.0, 0.0), 20.0, 0.0),
                // A levy by region and month, by pay band; nothing below 1000.
                row("levy", "2026-01-01", ("North", ""), (1000.0, 0.0), 0.0, 20.0),
                row("levy", "2026-01-01", ("North", "5"), (1000.0, 0.0), 0.0, 30.0),
                row("levy", "2026-01-01", ("South", ""), (0.0, 0.0), 0.0, 0.0),
            ],
        };
        let mut v = vars(&[("base", "8000"), ("gross", "8000"), ("BS", "4000"), ("HA", "1500")]);
        v.tables = Arc::new(tables);
        v.maps.insert("employee".into(), serde_json::json!({"date_of_joining": "2021-08-15", "x_band": "B2"}));
        v.maps.insert("location".into(), serde_json::json!({"region": "North"}));
        v.maps.insert(
            "period".into(),
            serde_json::json!({"start": "2026-05-01", "end": "2026-05-31", "month": 5, "year": 2026, "number": 2, "count": 12, "left": 11}),
        );
        v.maps.insert("ytd".into(), serde_json::json!({"WT": 300, "BS": 4000}));
        v.maps.insert("ytd_total".into(), serde_json::json!({"gross": 8000, "taxable": 8000, "tax": 300}));
        v.maps.insert("declared".into(), serde_json::json!({"SAVE": 5000, "RENT": 2000}));
        v.maps.insert("verified".into(), serde_json::json!({"SAVE": 3000, "RENT": 2000}));
        let amount = |src: &str| Formula::compile(src).unwrap().amount(&v).unwrap();

        // A contribution of 10% of basic up to the year's ceiling, and the employer's 12% on the same base.
        assert_eq!(amount("min(BS, lookup('fund_ceiling', 'amount', 0)) * 0.10"), n("400"));
        assert_eq!(amount("min(base, lookup('fund_ceiling', 'amount', 0)) * 0.12"), n("720"), "6000, this year's ceiling");
        let mut april_2025 = v.clone();
        april_2025.tables = Arc::new(Tables { on: Some("2025-06-30".parse().unwrap()), ..(*v.tables).clone() });
        assert_eq!(Formula::compile("min(base, lookup('fund_ceiling', 'amount', 0)) * 0.12").unwrap().amount(&april_2025).unwrap(), n("600"), "last year's");

        // Tax on the projected year (what was paid so far, this month's pay for the months left) less verified
        // savings, by bands, less what was already deducted, spread over the months left.
        // Projected: 8000 + 8000 * 11 - 3000 = 93000; bands: 3000 + 6600 = 9600; less 300, over 11.
        assert_eq!(amount("max(banded('wage_tax', ytd_total.taxable + gross * period.left - verified.SAVE) - ytd.WT, 0) / period.left"), n("845.454545"));

        // A fixed amount by region and month: May's own row for North, else its row for any month.
        assert_eq!(amount("lookup('levy', 'amount', gross, location.region, period.month)"), n("30"));
        let mut june = v.clone();
        june.maps.insert("period".into(), serde_json::json!({"month": 6}));
        assert_eq!(Formula::compile("lookup('levy', 'amount', gross, location.region, period.month)").unwrap().amount(&june).unwrap(), n("20"));
        assert_eq!(amount("lookup('levy', 'amount', 900, location.region, period.month)"), n("0"), "below the band");

        // An exemption as the least of three: rent declared less a tenth of basic, 40% of basic, the allowance.
        assert_eq!(amount("max(min(declared.RENT - BS * 0.1, BS * 0.4, HA), 0)"), n("1500"));
        // Tenure from the employee's own field, and a field a workspace added.
        assert!(Formula::compile("years_between(employee.date_of_joining, period.end) >= 4 && employee.x_band == 'B2'").unwrap().holds(&v).unwrap());
        assert_eq!(amount("months_between(employee.date_of_joining, period.end)"), n("57"));

        // Formulas that read none of it need none of it; reading it is seen before it runs.
        assert!(!Formula::compile("base * 0.4").unwrap().uses_context(&[]));
        assert!(Formula::compile("lookup('levy', 'amount', gross)").unwrap().uses_context(&[]));
        assert!(Formula::compile("ytd.BS + 1").unwrap().uses_context(&[]));
        assert!(!Formula::compile("period * 2").unwrap().uses_context(&["period".to_string()]), "an older component named period keeps its name");
        assert!(check_abbr("ytd").is_err() && check_abbr("lookup").is_err());
    }
}
