//! Salary slips, recomputed on every save while draft:
//!
//! * working days are the period's days (less holidays when the company excludes them);
//!   payment days are the working days the employee was employed (joining/relieving), less
//!   approved leave without pay and absences marked in attendance (half days count half);
//! * structure rows are evaluated earnings first, then deductions, then employer contributions, in
//!   order. Formulas see the full-period amounts of the rows above (`BS`, `HRA`, … and `gross`),
//!   and each row is then prorated by payment days when its component says so (ERPNext's
//!   semantics). Employer contributions are never part of gross or net pay;
//! * additional salary for the period is added (or replaces the structure amount), and arrears for
//!   periods already paid when a salary assignment took effect in them;
//! * income tax is the year's tax on projected taxable salary (with the opening figures of the year:
//!   what the system moved from and a previous employer paid), less tax already deducted, spread
//!   over the periods left in the payroll period (additional salary marked Deduct Full Tax adds the
//!   tax it brings, taken in the period it is paid). In the period an employee leaves it is
//!   the year's actual tax less what was deducted: nothing is spread over months they will not work;
//! * an off-cycle slip is for one day and pays only off-cycle additional salary of that day and
//!   arrears, with the tax they bring;
//! * a slip of a payroll run takes back what an earlier run slip, corrected, paid too much.

use super::formula::{Formula, LookupRow, Tables, Vars, annual_tax};
use super::{FLAGS, assignment_on, row_formula, run};
use crate::calendar::{employee_holidays, working_days};
use crate::{lookup, opt_date, stamp_company};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use ferrum_app_erp::ledger;
use ferrum_domain::util::{Company, b, company, d, elevated, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Doc, DocHook, Error, FieldError, HookCtx, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

pub(crate) struct SlipHook;

/// The three tables of a structure and a slip, in the order they are computed.
pub(crate) const TABLES: [&str; 3] = ["earnings", "deductions", "employer_contributions"];

/// The table a component's rows go in.
pub(crate) fn table_of(comp: &Doc) -> &'static str {
    match s(comp, "component_type") {
        Some("Deduction") => "deductions",
        Some("Employer Contribution") => "employer_contributions",
        _ => "earnings",
    }
}

/// Payroll sub-periods from `from` to the end of the payroll period, this one included.
pub fn periods_left(frequency: &str, from: NaiveDate, period_end: NaiveDate) -> i64 {
    let n = match frequency {
        "Weekly" | "Fortnightly" => {
            let len = if frequency == "Weekly" { 7 } else { 14 };
            ((period_end - from).num_days() + 1 + len - 1) / len
        }
        _ => (period_end.year() * 12 + period_end.month() as i32 - from.year() * 12 - from.month() as i32 + 1) as i64,
    };
    n.max(1)
}

/// Pay periods from the one holding `first` to the one holding `day` (0: the same one).
pub(crate) fn periods_between(frequency: &str, first: NaiveDate, day: NaiveDate) -> i64 {
    match frequency {
        "Weekly" | "Fortnightly" => (day - first).num_days().div_euclid(if frequency == "Weekly" { 7 } else { 14 }),
        _ => (day.year() * 12 + day.month() as i32 - first.year() * 12 - first.month() as i32) as i64,
    }
}

fn round_amount(v: Decimal, whole: bool, precision: u32) -> Decimal {
    if whole {
        let mut r = v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        r.rescale(precision);
        r
    } else {
        round(v, precision)
    }
}

/// One computed row of a slip.
struct Row {
    component: Doc,
    amount: Decimal,
    default_amount: Decimal,
    additional: Decimal,
    /// The part of the additional salary whose full tax is taken in the period it is paid.
    full_tax: Decimal,
    arrears: Decimal,
}

impl Row {
    fn flag(&self, f: &str) -> bool {
        b(&self.component, f)
    }

    fn new(component: Doc, amount: Decimal) -> Row {
        Row { component, amount, default_amount: Decimal::ZERO, additional: Decimal::ZERO, full_tax: Decimal::ZERO, arrears: Decimal::ZERO }
    }
}

/// Additional salary of a period by component: (amount, whether it replaces the structure's amount,
/// the part of it whose full tax is taken in the period).
type Extra = BTreeMap<Uuid, (Decimal, bool, Decimal)>;

/// Each table's rows, in order.
type Rows = HashMap<&'static str, Vec<Row>>;

/// A period's days as a slip pays them.
#[derive(Clone, Copy)]
struct Days {
    working: Decimal,
    payment: Decimal,
    lwp: Decimal,
    absent: Decimal,
}

/// Approved absences in attendance within `[from, to]`: absent days count 1, half days without
/// a leave type count ½ (leave days are handled through leave without pay).
async fn absent_days(hc: &mut HookCtx<'_>, employee: Uuid, from: NaiveDate, to: NaiveDate) -> Result<Decimal> {
    Ok(sqlx::query_scalar(
        "select coalesce(sum(case when status = 'Absent' then 1 when status = 'Half Day' and leave_type is null then 0.5 else 0 end), 0)::numeric \
         from e_attendance where tenant_id = $1 and employee = $2 and docstatus = 1 and attendance_date between $3 and $4",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(from)
    .bind(to)
    .fetch_one(&mut **hc.tx)
    .await?)
}

/// Titles of the employee's organization links, for conditions.
async fn texts(hc: &mut HookCtx<'_>, emp: &Doc, vars: &mut Vars) -> Result<()> {
    for (var, entity, title) in [
        ("designation", "Designation", "designation_name"),
        ("department", "Department", "department_name"),
        ("branch", "Branch", "branch_name"),
        ("grade", "EmployeeGrade", "grade_name"),
        ("employment_type", "EmploymentType", "employment_type_name"),
    ] {
        let name = lookup(hc, entity, id(emp, var)).await?.and_then(|r| s(&r, title).map(str::to_string)).unwrap_or_default();
        vars.texts.insert(var.into(), name);
    }
    Ok(())
}

struct Proration<'a> {
    co: &'a Company,
    payment_days: Decimal,
    working: Decimal,
}

impl Proration<'_> {
    fn prorate(&self, full: Decimal, comp: &Doc) -> Decimal {
        let whole = b(comp, "round_to_the_nearest_integer");
        if !b(comp, "depends_on_payment_days") {
            return round_amount(full, whole, self.co.precision);
        }
        if self.working.is_zero() {
            return Decimal::ZERO;
        }
        round_amount(full * self.payment_days / self.working, whole, self.co.precision)
    }
}

/// A record's fields as a formula reads them: every field of the entity (empty ones null), numbers as numbers,
/// secrets left out.
fn fields_of(hc: &HookCtx<'_>, entity: &str, doc: Option<&Doc>) -> Value {
    use ferrum_meta::FieldType as F;
    let mut out = serde_json::Map::new();
    let Some(ent) = hc.ctx.schema.entity(entity) else { return Value::Object(out) };
    for f in ent.def.fields.iter().filter(|f| !matches!(f.ftype, F::Secret | F::Table)) {
        let v = doc.and_then(|d| d.get(&f.name)).cloned().unwrap_or(Value::Null);
        let v = match (f.ftype, &v) {
            (F::Int | F::Decimal | F::Currency | F::Percent, Value::String(x)) => x.parse::<f64>().map(|n| json!(n)).unwrap_or(Value::Null),
            _ => v,
        };
        out.insert(f.name.clone(), v);
    }
    Value::Object(out)
}

/// `(abbreviation, amount)` sums as a formula's map, every component's abbreviation there (0 when it paid none).
fn by_abbr(all: &[String], paid: Vec<(Option<String>, Decimal)>) -> Value {
    let mut m: serde_json::Map<String, Value> = all.iter().map(|a| (a.clone(), json!(0.0))).collect();
    for (a, v) in paid {
        if let Some(a) = a {
            m.insert(a, json!(v.to_f64().unwrap_or_default()));
        }
    }
    Value::Object(m)
}

/// What statutory formulas read ([`super::formula::CONTEXT`]) for `emp`'s period `[start, end]`, slip `me`
/// left out of what was paid before it, and the lookup tables as of `end`.
async fn context(
    hc: &mut HookCtx<'_>,
    emp: &Doc,
    co: &Company,
    (start, end): (NaiveDate, NaiveDate),
    me: Option<Uuid>,
    frequency: &str,
    vars: &mut Vars,
) -> Result<()> {
    let tenant = hc.ctx.tenant;
    let emp_id = id(emp, "id").unwrap_or_default();
    vars.maps.insert("employee".into(), fields_of(hc, "Employee", Some(emp)));
    let branch = lookup(hc, "Branch", id(emp, "branch")).await?;
    vars.maps.insert("location".into(), fields_of(hc, "Branch", branch.as_ref()));
    // The payroll year: the payroll period holding the end, else the calendar year.
    let period: Option<(Uuid, NaiveDate, NaiveDate)> = sqlx::query_as(
        "select id, start_date, end_date from e_payroll_period where tenant_id = $1 and company = $2 and start_date <= $3 and end_date >= $3 limit 1",
    )
    .bind(tenant)
    .bind(co.id)
    .bind(end)
    .fetch_optional(&mut **hc.tx)
    .await?;
    let (pid, y_start, y_end) = match period {
        Some((p, s, e)) => (Some(p), s, e),
        None => (None, NaiveDate::from_ymd_opt(end.year(), 1, 1).unwrap_or(end), NaiveDate::from_ymd_opt(end.year(), 12, 31).unwrap_or(end)),
    };
    vars.maps.insert(
        "period".into(),
        json!({"start": start.to_string(), "end": end.to_string(), "month": end.month(), "year": end.year(), "days": (end - start).num_days() + 1,
               "number": periods_between(frequency, y_start, start) + 1, "count": periods_between(frequency, y_start, y_end) + 1,
               "left": periods_left(frequency, start, y_end)}),
    );
    let all: Vec<String> =
        sqlx::query_scalar("select abbr from e_salary_component where tenant_id = $1 and abbr is not null").bind(tenant).fetch_all(&mut **hc.tx).await?;
    let rows_of = |which: &str| {
        format!(
            "select r.abbr, sum(r.amount) from (select parent_id, abbr, amount, tenant_id from e_salary_slip__earnings union all select parent_id, abbr, amount, tenant_id \
             from e_salary_slip__deductions union all select parent_id, abbr, amount, tenant_id from e_salary_slip__employer_contributions) r \
             join e_salary_slip s on s.tenant_id = r.tenant_id and s.id = r.parent_id where r.tenant_id = $1 and s.employee = $2 and s.docstatus = 1 \
             and ($3::uuid is null or s.id <> $3) and {which} group by r.abbr"
        )
    };
    let ytd: Vec<(Option<String>, Decimal)> = sqlx::query_as(sqlx::AssertSqlSafe(rows_of("s.start_date >= $4 and s.end_date < $5")))
        .bind(tenant)
        .bind(emp_id)
        .bind(me)
        .bind(y_start)
        .bind(start)
        .fetch_all(&mut **hc.tx)
        .await?;
    vars.maps.insert("ytd".into(), by_abbr(&all, ytd));
    let totals: (Decimal, Decimal, Decimal, Decimal, Decimal, Decimal) = sqlx::query_as(
        "select coalesce(sum(gross_pay), 0), coalesce(sum(total_deduction), 0), coalesce(sum(net_pay), 0), coalesce(sum(total_employer_contribution), 0), \
         coalesce(sum(taxable_earnings), 0), coalesce(sum(income_tax_deducted), 0) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus = 1 \
         and ($3::uuid is null or id <> $3) and start_date >= $4 and end_date < $5",
    )
    .bind(tenant)
    .bind(emp_id)
    .bind(me)
    .bind(y_start)
    .bind(start)
    .fetch_one(&mut **hc.tx)
    .await?;
    let opening: (Decimal, Decimal) = sqlx::query_as(
        "select coalesce(sum(coalesce(taxable_earnings, 0) + coalesce(previous_employer_income, 0)), 0), \
         coalesce(sum(coalesce(tax_deducted, 0) + coalesce(previous_employer_tax, 0)), 0) from e_payroll_opening \
         where tenant_id = $1 and employee = $2 and payroll_period = $3 and docstatus = 1",
    )
    .bind(tenant)
    .bind(emp_id)
    .bind(pid)
    .fetch_one(&mut **hc.tx)
    .await?;
    let f = |d: Decimal| d.to_f64().unwrap_or_default();
    vars.maps.insert(
        "ytd_total".into(),
        json!({"gross": f(totals.0), "deductions": f(totals.1), "net": f(totals.2), "employer": f(totals.3),
               "taxable": f(totals.4 + opening.0), "tax": f(totals.5 + opening.1)}),
    );
    let previous: Vec<(Option<String>, Decimal)> = sqlx::query_as(sqlx::AssertSqlSafe(rows_of(
        "s.id = (select p.id from e_salary_slip p where p.tenant_id = $1 and p.employee = $2 and p.docstatus = 1 and not coalesce(p.off_cycle, false) \
         and ($3::uuid is null or p.id <> $3) and p.end_date < $4 order by p.end_date desc limit 1)",
    )))
    .bind(tenant)
    .bind(emp_id)
    .bind(me)
    .bind(start)
    .fetch_all(&mut **hc.tx)
    .await?;
    vars.maps.insert("previous".into(), by_abbr(&all, previous));
    // What the employee declared for the payroll year, by category code, and of it what HR verified.
    let codes: Vec<String> = sqlx::query_scalar("select code from e_employee_tax_exemption_category where tenant_id = $1 and code is not null")
        .bind(tenant)
        .fetch_all(&mut **hc.tx)
        .await?;
    let declared: Vec<(Option<String>, Decimal, Decimal)> = sqlx::query_as(
        "select c.code, sum(r.amount), sum(case when coalesce(r.verified, false) then r.amount else 0 end) \
         from e_employee_tax_exemption_declaration d join e_employee_tax_exemption_declaration__declarations r on r.tenant_id = d.tenant_id and r.parent_id = d.id \
         join e_employee_tax_exemption_category c on c.tenant_id = r.tenant_id and c.id = r.exemption_category \
         where d.tenant_id = $1 and d.employee = $2 and d.payroll_period = $3 and d.docstatus = 1 and c.code is not null group by c.code",
    )
    .bind(tenant)
    .bind(emp_id)
    .bind(pid)
    .fetch_all(&mut **hc.tx)
    .await?;
    vars.maps.insert("declared".into(), by_abbr(&codes, declared.iter().map(|(c, a, _)| (c.clone(), *a)).collect()));
    vars.maps.insert("verified".into(), by_abbr(&codes, declared.iter().map(|(c, _, v)| (c.clone(), *v)).collect()));
    let rows: Vec<(String, NaiveDate, Option<String>, Option<String>, Option<Decimal>, Option<Decimal>, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        "select t.table_name, r.effective_from, r.key1, r.key2, r.from_amount, r.to_amount, r.rate, r.amount from e_payroll_lookup_table t \
         join e_payroll_lookup_table__rows r on r.tenant_id = t.tenant_id and r.parent_id = t.id where t.tenant_id = $1 and r.effective_from <= $2",
    )
    .bind(tenant)
    .bind(end)
    .fetch_all(&mut **hc.tx)
    .await?;
    let n = |d: Option<Decimal>| d.and_then(|d| d.to_f64()).unwrap_or_default();
    vars.tables = std::sync::Arc::new(Tables {
        on: Some(end),
        rows: rows
            .into_iter()
            .map(|(table, effective_from, k1, k2, lo, hi, rate, amount)| LookupRow {
                table,
                effective_from,
                key1: k1.unwrap_or_default(),
                key2: k2.unwrap_or_default(),
                from: n(lo),
                to: n(hi),
                rate: n(rate),
                amount: n(amount),
            })
            .collect(),
    });
    Ok(())
}

/// The structure's rows for a period of `days`, with the period's additional salary for the
/// components it has (taken out of `extra`), and the income tax components apart. Formulas reading the
/// statutory context get it for `period`, slip `me` left out of what was paid before.
#[allow(clippy::too_many_arguments)]
async fn evaluate(
    hc: &mut HookCtx<'_>,
    emp: &Doc,
    ssa: &Doc,
    structure: &Doc,
    co: &Company,
    days: Days,
    extra: &mut Extra,
    period: (NaiveDate, NaiveDate),
    me: Option<Uuid>,
) -> Result<(Rows, Vec<Doc>)> {
    let mut vars = Vars::default();
    vars.set("base", d(ssa, "base"));
    vars.set("variable", d(ssa, "variable"));
    vars.set("payment_days", days.payment);
    vars.set("working_days", days.working);
    vars.set("lwp_days", days.lwp);
    vars.set("absent_days", days.absent);
    vars.set("gross", Decimal::ZERO);
    texts(hc, emp, &mut vars).await?;
    // The context, only when a formula of the structure reads it.
    let abbrs: Vec<String> = TABLES.iter().flat_map(|t| rows(structure, t)).filter_map(|r| s(&r, "abbr").map(str::to_string)).collect();
    let mut reads = false;
    for r in TABLES.iter().flat_map(|t| rows(structure, t)) {
        let comp = lookup(hc, "SalaryComponent", id(&r, "salary_component")).await?;
        let sources = [s(&r, "condition"), comp.as_ref().and_then(|c| row_formula(&r, c))];
        reads |= sources.into_iter().flatten().any(|src| Formula::compile(src).is_ok_and(|f| f.uses_context(&abbrs)));
    }
    if reads {
        let frequency = s(structure, "payroll_frequency").unwrap_or("Monthly").to_string();
        context(hc, emp, co, period, me, &frequency, &mut vars).await?;
    }
    let cx = Proration { co, payment_days: days.payment, working: days.working };
    let mut out: Rows = HashMap::new();
    let mut tax: Vec<Doc> = Vec::new();
    let mut gross_full = Decimal::ZERO;
    let mut errors = Vec::new();
    for table in TABLES {
        for r in rows(structure, table) {
            if let Some(a) = s(&r, "abbr") {
                vars.set(a, Decimal::ZERO);
            }
        }
    }
    for table in TABLES {
        for (i, r) in rows(structure, table).into_iter().enumerate() {
            let Some(comp) = lookup(hc, "SalaryComponent", id(&r, "salary_component")).await? else { continue };
            if b(&comp, "disabled") {
                continue;
            }
            if b(&comp, "variable_based_on_taxable_salary") {
                tax.push(comp);
                continue;
            }
            let cname = s(&comp, "component_name").unwrap_or_default().to_string();
            let abbr = s(&comp, "abbr").unwrap_or_default().to_string();
            if let Some(c) = s(&r, "condition") {
                match Formula::compile(c).and_then(|f| f.holds(&vars)) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(m) => {
                        errors.push(FieldError::new("condition", format!("{cname}: {m}")).in_row(table, i));
                        continue;
                    }
                }
            }
            let full = match row_formula(&r, &comp) {
                Some(f) => match Formula::compile(f).and_then(|f| f.amount(&vars)) {
                    Ok(v) => v,
                    Err(m) => {
                        errors.push(FieldError::new("formula", format!("{cname}: {m}")).in_row(table, i));
                        continue;
                    }
                },
                None if !d(&r, "amount").is_zero() => d(&r, "amount"),
                None => d(&comp, "amount"),
            };
            if full < Decimal::ZERO {
                errors.push(FieldError::new("formula", format!("{cname} came out negative ({})", full.normalize())).in_row(table, i));
                continue;
            }
            let full = round_amount(full, b(&comp, "round_to_the_nearest_integer"), co.precision);
            vars.set(&abbr, full);
            let mut row = Row { default_amount: full, ..Row::new(comp.clone(), cx.prorate(full, &comp)) };
            if let Some((a, overwrite, full_tax)) = extra.remove(&id(&row.component, "id").unwrap_or_default()) {
                row.additional = a;
                row.full_tax = full_tax;
                row.amount = if overwrite { a } else { row.amount + a };
            }
            if table == "earnings" && !row.flag("statistical_component") {
                gross_full += full;
                vars.set("gross", gross_full);
            }
            out.entry(table).or_default().push(row);
        }
    }
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    Ok((out, tax))
}

/// One period's difference for one component (`due - paid`).
struct Arrear {
    start: NaiveDate,
    end: NaiveDate,
    component: Doc,
    paid: Decimal,
    due: Decimal,
    /// The slip that paid the period.
    slip: Option<Uuid>,
    assignment: Option<Uuid>,
}

/// Arrears already paid for a period by the employee's submitted slips other than `me`, by component.
async fn arrears_paid(hc: &mut HookCtx<'_>, employee: Uuid, me: Option<Uuid>, start: NaiveDate, end: NaiveDate) -> Result<BTreeMap<Uuid, Decimal>> {
    Ok(sqlx::query_as::<_, (Uuid, Decimal)>(
        "select r.salary_component, sum(r.amount) from e_salary_slip__arrears r join e_salary_slip p on p.tenant_id = r.tenant_id and p.id = r.parent_id \
         where r.tenant_id = $1 and p.employee = $2 and p.docstatus = 1 and p.id is distinct from $3 and r.period_start = $4 and r.period_end = $5 \
         group by r.salary_component",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(me)
    .bind(start)
    .bind(end)
    .fetch_all(&mut **hc.tx)
    .await?
    .into_iter()
    .collect())
}

/// Arrears for the periods paid before `before`: each regular slip submitted for a period whose
/// salary assignment is no longer the one in force (a revision that took effect in it), or for
/// which arrears were paid, is worked out again as it was paid (its working and payment days,
/// leave without pay and absences) with the assignment in force now, and each component's
/// difference with what was paid for the period (by the slip and by arrears on other slips) is due.
/// Worked out from what is submitted every time, so nothing is paid twice and a revision withdrawn
/// before its arrears are paid leaves nothing behind.
async fn arrears(hc: &mut HookCtx<'_>, emp: &Doc, co: &Company, me: Option<Uuid>, before: NaiveDate) -> Result<Vec<Arrear>> {
    let emp_id = id(emp, "id").unwrap_or_default();
    let periods: Vec<Uuid> = sqlx::query_scalar(
        "select s.id from e_salary_slip s where s.tenant_id = $1 and s.employee = $2 and s.docstatus = 1 and not coalesce(s.off_cycle, false) \
         and s.start_date < $3 and s.id is distinct from $4 \
         and (s.salary_structure_assignment is distinct from (select a.id from e_salary_structure_assignment a where a.tenant_id = s.tenant_id \
                and a.employee = s.employee and a.docstatus = 1 and a.from_date <= s.end_date order by a.from_date desc limit 1) \
              or exists (select 1 from e_salary_slip__arrears r join e_salary_slip p on p.tenant_id = r.tenant_id and p.id = r.parent_id \
                where r.tenant_id = s.tenant_id and p.employee = s.employee and p.docstatus = 1 and p.id is distinct from $4 \
                and r.period_start = s.start_date and r.period_end = s.end_date)) \
         order by s.start_date",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(before)
    .bind(me)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut out = Vec::new();
    for slip_id in periods {
        let paid_slip = get(hc.kernel, hc.ctx, hc.tx, "SalarySlip", slip_id).await?;
        let (Some(start), Some(end)) = (opt_date(&paid_slip, "start_date"), opt_date(&paid_slip, "end_date")) else { continue };
        let Some(ssa) = assignment_on(hc, emp_id, end).await? else { continue };
        let structure = get(hc.kernel, hc.ctx, hc.tx, "SalaryStructure", id(&ssa, "salary_structure").unwrap_or_default()).await?;
        let days = Days {
            working: d(&paid_slip, "total_working_days"),
            payment: d(&paid_slip, "payment_days"),
            lwp: d(&paid_slip, "leave_without_pay"),
            absent: d(&paid_slip, "absent_days"),
        };
        let (now, _) = evaluate(hc, emp, &ssa, &structure, co, days, &mut BTreeMap::new(), (start, end), Some(slip_id)).await?;
        let mut due: BTreeMap<Uuid, (Doc, Decimal)> = BTreeMap::new();
        for r in now.into_values().flatten().filter(|r| !r.flag("statistical_component")) {
            due.insert(id(&r.component, "id").unwrap_or_default(), (r.component, r.amount));
        }
        // What the slip paid of the structure: not its additional salary, nor arrears it carried for
        // other periods. A component additional salary replaced that period has none.
        let mut paid: BTreeMap<Uuid, Decimal> = BTreeMap::new();
        let mut replaced = Vec::new();
        for r in TABLES.iter().flat_map(|t| rows(&paid_slip, t)) {
            if b(&r, "statistical_component") || b(&r, "variable_based_on_taxable_salary") {
                continue;
            }
            let Some(comp) = id(&r, "salary_component") else { continue };
            let (amount, additional, arrears) = (d(&r, "amount"), d(&r, "additional_amount"), d(&r, "arrears_amount"));
            if !additional.is_zero() && amount - arrears == additional && !d(&r, "default_amount").is_zero() {
                replaced.push(comp);
            }
            *paid.entry(comp).or_default() += amount - additional - arrears;
        }
        let before_here = arrears_paid(hc, emp_id, me, start, end).await?;
        let comps: Vec<Uuid> =
            due.keys().chain(paid.keys()).chain(before_here.keys()).copied().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        for comp in comps.into_iter().filter(|c| !replaced.contains(c)) {
            let due_now = due.get(&comp).map(|x| x.1).unwrap_or_default();
            let paid_for = paid.get(&comp).copied().unwrap_or_default() + before_here.get(&comp).copied().unwrap_or_default();
            if due_now == paid_for {
                continue;
            }
            let component = match due.remove(&comp) {
                Some((c, _)) => c,
                None => get(hc.kernel, hc.ctx, hc.tx, "SalaryComponent", comp).await?,
            };
            out.push(Arrear { start, end, component, paid: paid_for, due: due_now, slip: Some(slip_id), assignment: id(&ssa, "id") });
        }
    }
    Ok(out)
}

/// Whether arrears are due to (or from) the employee for the periods paid up to `day`.
pub(crate) async fn arrears_due(hc: &mut HookCtx<'_>, employee: Uuid, day: NaiveDate) -> Result<bool> {
    let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", employee).await?;
    let co = company(hc.kernel, hc.ctx, hc.tx, id(&emp, "company").unwrap_or_default()).await?;
    Ok(!arrears(hc, &emp, &co, None, day + chrono::Duration::days(1)).await?.is_empty())
}

/// A regular period's days as a slip pays them, from leave and attendance as they are now: working
/// days are the period's (less holidays when the company excludes them), payment days those the
/// employee was employed, less approved leave without pay and absences.
async fn days_of(hc: &mut HookCtx<'_>, emp: &Doc, co: &Company, start: NaiveDate, end: NaiveDate) -> Result<Days> {
    let emp_id = id(emp, "id").unwrap_or_default();
    let exclude = b(&co.doc, "pay_exclude_holidays_from_working_days");
    let holidays = employee_holidays(hc.tx, hc.ctx.tenant, emp_id, start, end).await?;
    let count = |a: NaiveDate, b_: NaiveDate| -> i64 {
        if b_ < a {
            0
        } else if exclude {
            working_days(a, b_, &holidays)
        } else {
            (b_ - a).num_days() + 1
        }
    };
    let working = count(start, end);
    let from = opt_date(emp, "date_of_joining").map_or(start, |j| j.max(start));
    let to = opt_date(emp, "relieving_date").map_or(end, |r| r.min(end));
    let employed = count(from, to);
    let (lwp, absent) = if employed > 0 {
        (crate::leave::lwp_days(hc, emp_id, from, to, &holidays, !exclude).await?, absent_days(hc, emp_id, from, to).await?)
    } else {
        (Decimal::ZERO, Decimal::ZERO)
    };
    Ok(Days { working: Decimal::from(working), payment: (Decimal::from(employed) - lwp - absent).max(Decimal::ZERO), lwp, absent })
}

/// Days of a paid period carried to another slip: refunded (+) or taken off (−), at a day's pay.
struct Adjustment {
    start: NaiveDate,
    end: NaiveDate,
    /// The slip that paid the period.
    slip: Option<Uuid>,
    days: Decimal,
    rate: Decimal,
    amount: Decimal,
    reason: String,
}

/// The days and amount carried for a period by the employee's submitted slips other than `me`.
async fn carried(hc: &mut HookCtx<'_>, employee: Uuid, me: Option<Uuid>, start: NaiveDate, end: NaiveDate) -> Result<(Decimal, Decimal)> {
    Ok(sqlx::query_as(
        "select coalesce(sum(r.days), 0), coalesce(sum(r.amount), 0) from e_salary_slip__adjustments r join e_salary_slip p \
         on p.tenant_id = r.tenant_id and p.id = r.parent_id where r.tenant_id = $1 and p.employee = $2 and p.docstatus = 1 \
         and p.id is distinct from $3 and r.period_start = $4 and r.period_end = $5",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(me)
    .bind(start)
    .bind(end)
    .fetch_one(&mut **hc.tx)
    .await?)
}

/// What a submitted regular slip leaves to pay (or recover) now that its period's leave or
/// attendance changed: its unpaid days then and now, a day's pay as it paid it (its prorated
/// earnings over its working days), and the days and amount not yet carried by other slips.
struct Pending {
    counted: Decimal,
    now: Decimal,
    rate: Decimal,
    days: Decimal,
    amount: Decimal,
}

async fn pending(hc: &mut HookCtx<'_>, emp: &Doc, co: &Company, paid: &Doc, me: Option<Uuid>) -> Result<Pending> {
    let (Some(start), Some(end)) = (opt_date(paid, "start_date"), opt_date(paid, "end_date")) else {
        return Ok(Pending { counted: Decimal::ZERO, now: Decimal::ZERO, rate: Decimal::ZERO, days: Decimal::ZERO, amount: Decimal::ZERO });
    };
    let days = days_of(hc, emp, co, start, end).await?;
    let now = days.lwp + days.absent;
    let counted = d(paid, "leave_without_pay") + d(paid, "absent_days");
    let working = d(paid, "total_working_days");
    let full: Decimal =
        rows(paid, "earnings").iter().filter(|r| b(r, "depends_on_payment_days") && !b(r, "statistical_component")).map(|r| d(r, "default_amount")).sum();
    let rate = if working.is_zero() { Decimal::ZERO } else { round(full / working, co.precision) };
    let (carried_days, carried_amount) = carried(hc, id(emp, "id").unwrap_or_default(), me, start, end).await?;
    let target = counted - now;
    Ok(Pending { counted, now, rate, days: target - carried_days, amount: round(target * rate, co.precision) - carried_amount })
}

/// The pay adjustments a slip carries: for each earlier period paid whose slip says changes after
/// payroll are carried (every one, on a leaver's last slip), what its leave and attendance now leave
/// unpaid against what it paid, less what other slips carried; and on a slip paying a period again
/// (a correction), what other slips carried for it, taken back, as the slip pays the days as they are.
async fn adjustments(
    hc: &mut HookCtx<'_>,
    emp: &Doc,
    co: &Company,
    me: Option<Uuid>,
    (start, end): (NaiveDate, NaiveDate),
    off_cycle: bool,
    leaving: bool,
) -> Result<Vec<Adjustment>> {
    let emp_id = id(emp, "id").unwrap_or_default();
    let before = if off_cycle { start + chrono::Duration::days(1) } else { start };
    let paid: Vec<Uuid> = sqlx::query_scalar(
        "select id from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus = 1 and not coalesce(off_cycle, false) \
         and id is distinct from $3 and start_date < $4 and (late_changes = 'Carry to Next Pay' or $5) order by start_date",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(me)
    .bind(before)
    .bind(leaving)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut out = Vec::new();
    for slip in paid {
        let paid = get(hc.kernel, hc.ctx, hc.tx, "SalarySlip", slip).await?;
        let p = pending(hc, emp, co, &paid, me).await?;
        if p.days.is_zero() && p.amount.is_zero() {
            continue;
        }
        let (Some(from), Some(to)) = (opt_date(&paid, "start_date"), opt_date(&paid, "end_date")) else { continue };
        let reason =
            format!("{} unpaid days when salary slip {} paid it, {} now", p.counted.normalize(), s(&paid, "number").unwrap_or_default(), p.now.normalize());
        out.push(Adjustment { start: from, end: to, slip: Some(slip), days: p.days, rate: p.rate, amount: p.amount, reason });
    }
    if !off_cycle {
        let (days, amount) = carried(hc, emp_id, me, start, end).await?;
        if !days.is_zero() || !amount.is_zero() {
            let reason = "Carried to a later slip before this one paid the period again: taken back, as this slip pays its days as they are".to_string();
            out.push(Adjustment { start, end, slip: None, days: -days, rate: Decimal::ZERO, amount: -amount, reason });
        }
    }
    Ok(out)
}

/// Brings a submitted regular slip's waiting adjustment up to date. Returns it with the slip.
async fn refresh_pending(hc: &mut HookCtx<'_>, slip: Uuid) -> Result<Option<(Pending, Doc)>> {
    let paid = get(hc.kernel, hc.ctx, hc.tx, "SalarySlip", slip).await?;
    if d(&paid, "docstatus") != Decimal::ONE || b(&paid, "off_cycle") {
        return Ok(None);
    }
    let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", id(&paid, "employee").unwrap_or_default()).await?;
    let co = company(hc.kernel, hc.ctx, hc.tx, id(&paid, "company").unwrap_or_default()).await?;
    let p = pending(hc, &emp, &co, &paid, None).await?;
    let mut v = Doc::new();
    set(&mut v, "changed_after_payroll", !p.days.is_zero() || !p.amount.is_zero());
    set(&mut v, "pending_adjustment_days", money(p.days));
    set(&mut v, "pending_adjustment", money(p.amount));
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", slip, v).await?;
    Ok(Some((p, paid)))
}

/// After leave or attendance of `[from, to]` changed: each submitted regular slip of those days
/// shows what now waits. A change by a person that alters what a slip paid is refused until someone
/// with the right to submit slips chose, on the slip, how such changes are handled.
pub(crate) async fn late_change(hc: &mut HookCtx<'_>, employee: Option<Uuid>, from: Option<NaiveDate>, to: Option<NaiveDate>) -> Result<()> {
    let (Some(employee), Some(from), Some(to)) = (employee, from, to) else { return Ok(()) };
    if hc.ctx.schema.entity("SalarySlip").is_none() {
        return Ok(());
    }
    let paid: Vec<Uuid> = sqlx::query_scalar(
        "select id from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus = 1 and not coalesce(off_cycle, false) \
         and start_date <= $4 and end_date >= $3 order by start_date",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **hc.tx)
    .await?;
    for slip in paid {
        let Some((p, doc)) = refresh_pending(hc, slip).await? else { continue };
        if !hc.ctx.is_system && !p.days.is_zero() && s(&doc, "late_changes").is_none() {
            return Err(Error::invalid(format!(
                "{}'s pay for {} to {} is already submitted (salary slip {}), and this leaves {} days of it unpaid where the slip counted {}; \
                 someone who submits salary slips first chooses on that slip whether such changes are carried to the next pay or left for its correction \
                 (Changes After Payroll)",
                s(&doc, "employee_name").unwrap_or("the employee"),
                s(&doc, "start_date").unwrap_or_default(),
                s(&doc, "end_date").unwrap_or_default(),
                s(&doc, "number").unwrap_or_default(),
                p.now.normalize(),
                p.counted.normalize()
            )));
        }
    }
    Ok(())
}

/// Chooses how leave or attendance changed after the slip is submitted is handled.
async fn choose_late_changes(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
    let handling = params.get("handling").and_then(Value::as_str).filter(|h| ["Carry to Next Pay", "Correct the Slip"].contains(h));
    let handling = handling.ok_or_else(|| Error::field("handling", "choose Carry to Next Pay or Correct the Slip"))?;
    let label = format!("salary slip {}", s(doc, "number").unwrap_or_default());
    if b(doc, "off_cycle") || id(doc, "replaced_by").is_some() {
        return Err(Error::invalid(format!("{label} is off-cycle or already corrected; nothing of its period is carried from it")));
    }
    let mut v = Doc::new();
    set(&mut v, "late_changes", handling);
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await?;
    refresh_pending(hc, me).await?;
    let message = if handling == "Carry to Next Pay" {
        format!("Changes to the leave and attendance of {label}'s period are carried to the next salary slip")
    } else {
        format!("Changes to the leave and attendance of {label}'s period wait for its correction (Correct)")
    };
    Ok(json!({"message": message}))
}

pub(crate) async fn compute(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    let Some(emp) = stamp_company(hc, doc).await? else { return Ok(()) };
    // An off-cycle slip is for one day.
    let off_cycle = b(doc, "off_cycle");
    if off_cycle && let Some(day) = doc.get("start_date").cloned() {
        set(doc, "end_date", day);
    }
    let (Some(start), Some(end), Some(cid)) = (opt_date(doc, "start_date"), opt_date(doc, "end_date"), id(doc, "company")) else { return Ok(()) };
    if end < start {
        return Ok(());
    }
    let me = id(doc, "id");
    let emp_id = id(&emp, "id").unwrap_or_default();
    let name = s(&emp, "employee_name").unwrap_or_default().to_string();
    let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
    set(doc, "currency", co.currency_id.to_string());
    let ssa =
        assignment_on(hc, emp_id, end).await?.ok_or_else(|| Error::field("employee", format!("{name} has no salary structure assigned on or before {end}")))?;
    let structure = get(hc.kernel, hc.ctx, hc.tx, "SalaryStructure", id(&ssa, "salary_structure").unwrap_or_default()).await?;
    if d(&structure, "docstatus") != Decimal::ONE {
        return Err(Error::field(
            "employee",
            format!("{name}'s salary structure {} is no longer submitted", s(&structure, "structure_name").unwrap_or_default()),
        ));
    }
    set(doc, "salary_structure", structure.get("id").cloned().unwrap_or_default());
    set(doc, "salary_structure_assignment", ssa.get("id").cloned().unwrap_or_default());
    let frequency = s(&structure, "payroll_frequency").unwrap_or("Monthly").to_string();
    set(doc, "payroll_frequency", frequency.as_str());

    // ── days ──
    let days = if off_cycle {
        Days { working: Decimal::ZERO, payment: Decimal::ZERO, lwp: Decimal::ZERO, absent: Decimal::ZERO }
    } else {
        days_of(hc, &emp, &co, start, end).await?
    };
    set(doc, "total_working_days", money(days.working));
    set(doc, "leave_without_pay", money(days.lwp));
    set(doc, "absent_days", money(days.absent));
    set(doc, "payment_days", money(days.payment));

    // ── components ──
    // Additional salary of the period, or for an off-cycle slip of its day and named for one; one
    // that repeats, when this period is one of its occurrences.
    let mut extra: Extra = BTreeMap::new();
    for (c, a, o, f, first, every) in sqlx::query_as::<_, (Uuid, Decimal, bool, bool, NaiveDate, Option<i64>)>(
        "select salary_component, amount, coalesce(overwrite_salary_structure_amount, false), coalesce(deduct_full_tax, false), payroll_date, \
         case when coalesce(recurring, false) then greatest(coalesce(every, 1), 1) end from e_additional_salary \
         where tenant_id = $1 and employee = $2 and docstatus = 1 and coalesce(off_cycle, false) = $5 \
         and (payroll_date between $3 and $4 or (coalesce(recurring, false) and not $5 and payroll_date <= $4 and (to_date is null or to_date >= $3)))",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(start)
    .bind(end)
    .bind(off_cycle)
    .fetch_all(&mut **hc.tx)
    .await?
    {
        if every.is_some_and(|n| periods_between(&frequency, first, end) % n != 0) {
            continue;
        }
        let e = extra.entry(c).or_default();
        e.0 += a;
        e.1 |= o && !off_cycle;
        if f {
            e.2 += a;
        }
    }
    // An off-cycle slip pays none of the structure: its full period is worked out only to project the
    // year's income for tax.
    let (mut out, tax) = if off_cycle {
        let full = Days { working: Decimal::ONE, payment: Decimal::ONE, lwp: Decimal::ZERO, absent: Decimal::ZERO };
        evaluate(hc, &emp, &ssa, &structure, &co, full, &mut BTreeMap::new(), (start, end), me).await?
    } else {
        evaluate(hc, &emp, &ssa, &structure, &co, days, &mut extra, (start, end), me).await?
    };
    let taxable_earning = |r: &Row| r.flag("is_tax_applicable") && !r.flag("statistical_component");
    let exempt_deduction = |r: &Row| r.flag("exempted_from_income_tax") && !r.flag("statistical_component");
    let sum = |rows: Option<&Vec<Row>>, f: &dyn Fn(&Row) -> Decimal, keep: &dyn Fn(&Row) -> bool| -> Decimal {
        rows.map_or(Decimal::ZERO, |rows| rows.iter().filter(|r| keep(r)).map(f).sum())
    };
    let taxable_full = sum(out.get("earnings"), &|r| r.default_amount, &taxable_earning) - sum(out.get("deductions"), &|r| r.default_amount, &exempt_deduction);
    if off_cycle {
        out.clear();
    }
    // Additional salary for components outside the structure.
    for (comp_id, (a, _, full_tax)) in extra {
        let comp = get(hc.kernel, hc.ctx, hc.tx, "SalaryComponent", comp_id).await?;
        out.entry(table_of(&comp)).or_default().push(Row { additional: a, full_tax, ..Row::new(comp, a) });
    }

    // ── arrears ──
    let mut due = arrears(hc, &emp, &co, me, if off_cycle { start + chrono::Duration::days(1) } else { start }).await?;
    // Arrears another slip paid for this very period (before it was corrected) are already paid.
    if !off_cycle {
        for (comp, elsewhere) in arrears_paid(hc, emp_id, me, start, end).await?.into_iter().filter(|a| !a.1.is_zero()) {
            let component = get(hc.kernel, hc.ctx, hc.tx, "SalaryComponent", comp).await?;
            let own = out.get(table_of(&component)).and_then(|t| t.iter().find(|r| id(&r.component, "id") == Some(comp)));
            let structure_part = own.map_or(Decimal::ZERO, |r| r.amount - r.additional);
            due.push(Arrear { start, end, component, paid: structure_part + elsewhere, due: structure_part, slip: None, assignment: id(&ssa, "id") });
        }
    }
    for a in &due {
        let amount = a.due - a.paid;
        let list = out.entry(table_of(&a.component)).or_default();
        match list.iter_mut().find(|r| id(&r.component, "id") == id(&a.component, "id")) {
            Some(r) => {
                r.amount += amount;
                r.arrears += amount;
            }
            None => list.push(Row { arrears: amount, ..Row::new(a.component.clone(), amount) }),
        }
    }

    // ── pay adjustments: periods paid whose leave or attendance changed since ──
    let leaving = !off_cycle && opt_date(&emp, "relieving_date").is_some_and(|r| r <= end);
    let adjusted = adjustments(hc, &emp, &co, me, (start, end), off_cycle, leaving).await?;
    let adjustment: Decimal = adjusted.iter().map(|a| a.amount).sum();

    // ── income tax ──
    let earn = out.remove("earnings").unwrap_or_default();
    let mut ded = out.remove("deductions").unwrap_or_default();
    let con = out.remove("employer_contributions").unwrap_or_default();
    let taxable_now = sum(Some(&earn), &|r| r.amount, &taxable_earning) - sum(Some(&ded), &|r| r.amount, &exempt_deduction) + adjustment;
    // Its full tax now: all an off-cycle slip pays (no later period counts on it), and additional
    // salary marked so; the rest is spread over the periods left with regular pay.
    let one_off =
        if off_cycle { taxable_now } else { sum(Some(&earn), &|r| r.full_tax, &taxable_earning) - sum(Some(&ded), &|r| r.full_tax, &exempt_deduction) };
    let mut tax_total = Decimal::ZERO;
    for comp in tax {
        let t = income_tax(hc, doc, &ssa, &co, &frequency, (start, end, leaving, off_cycle), (taxable_now - one_off, one_off), taxable_full).await?;
        let t = round_amount(t, b(&comp, "round_to_the_nearest_integer"), co.precision);
        tax_total += t;
        ded.push(Row { default_amount: t, ..Row::new(comp, t) });
    }

    // ── totals and rows ──
    let counted = |r: &Row| !r.flag("statistical_component");
    let gross = sum(Some(&earn), &|r| r.amount, &counted) + adjustment;
    let deductions = sum(Some(&ded), &|r| r.amount, &counted);
    // What an earlier slip of a payroll run paid too much (corrected after the bank paid it) is taken
    // back from this pay, as far as it goes.
    let excess: Decimal =
        if id(doc, "payroll_entry").is_some() { super::payments::excess(hc, emp_id, me).await?.iter().map(|e| e.1).sum() } else { Decimal::ZERO };
    let recovery = excess.min((gross - deductions).max(Decimal::ZERO));
    // Loan instalments of the month (with the connector to ERP), never more than the pay left.
    let (loans, loan_total) = if !off_cycle && hc.entity.field("loans").is_some() {
        super::loans::slip_rows(hc, emp_id, me, end, co.precision, gross - deductions - recovery).await?
    } else {
        (Vec::new(), Decimal::ZERO)
    };
    let net = gross - deductions - recovery - loan_total;
    if net < Decimal::ZERO {
        return Err(Error::invalid(format!("deductions ({}) exceed earnings ({}) for {name}", deductions.normalize(), gross.normalize())));
    }
    let contributions = sum(Some(&con), &|r| r.amount, &counted);
    for (table, list) in [("earnings", earn), ("deductions", ded), ("employer_contributions", con)] {
        let old: HashMap<String, String> =
            rows(doc, table).iter().filter_map(|r| Some((s(r, "salary_component")?.to_string(), s(r, "id")?.to_string()))).collect();
        let mut made = Vec::new();
        for r in list.into_iter().filter(|r| !r.amount.is_zero() || !r.additional.is_zero() || !r.arrears.is_zero()) {
            let comp_id = s(&r.component, "id").unwrap_or_default().to_string();
            let mut row = Doc::new();
            set(&mut row, "id", old.get(&comp_id).cloned().unwrap_or_else(|| Uuid::now_v7().to_string()));
            set(&mut row, "idx", made.len());
            set(&mut row, "salary_component", comp_id);
            set(&mut row, "abbr", r.component.get("abbr").cloned().unwrap_or_default());
            set(&mut row, "amount", money(r.amount));
            set(&mut row, "default_amount", money(r.default_amount));
            set(&mut row, "additional_amount", money(r.additional));
            set(&mut row, "arrears_amount", money(r.arrears));
            for f in FLAGS {
                set(&mut row, f, r.flag(f));
            }
            made.push(Value::Object(row));
        }
        set(doc, table, Value::Array(made));
    }
    let arrears: Vec<Value> = due
        .iter()
        .enumerate()
        .map(|(i, a)| {
            json!({"id": Uuid::now_v7().to_string(), "idx": i, "period_start": a.start.to_string(), "period_end": a.end.to_string(),
                   "salary_component": s(&a.component, "id"), "abbr": a.component.get("abbr"), "paid_amount": money(a.paid), "due_amount": money(a.due),
                   "amount": money(a.due - a.paid), "salary_slip": a.slip.map(|u| u.to_string()), "salary_structure_assignment": a.assignment.map(|u| u.to_string())})
        })
        .collect();
    set(doc, "arrears", Value::Array(arrears));
    let adjusted: Vec<Value> = adjusted
        .iter()
        .enumerate()
        .map(|(i, a)| {
            json!({"id": Uuid::now_v7().to_string(), "idx": i, "period_start": a.start.to_string(), "period_end": a.end.to_string(), "days": money(a.days),
                   "daily_rate": money(a.rate), "amount": money(a.amount), "salary_slip": a.slip.map(|u| u.to_string()), "reason": a.reason})
        })
        .collect();
    set(doc, "adjustments", Value::Array(adjusted));
    set(doc, "adjustment_amount", money(adjustment));
    set(doc, "gross_pay", money(gross));
    set(doc, "recovery_amount", money(recovery));
    set(doc, "recovery_left", money(excess - recovery));
    if hc.entity.field("loans").is_some() {
        set(doc, "loans", Value::Array(loans));
        set(doc, "total_loan_repayment", money(loan_total));
    }
    set(doc, "total_deduction", money(deductions + recovery + loan_total));
    set(doc, "net_pay", money(net));
    set(doc, "rounded_total", money(round_amount(net, true, co.precision)));
    set(doc, "total_employer_contribution", money(contributions));
    set(doc, "final_settlement", leaving);
    set(doc, "taxable_earnings", money(taxable_now));
    set(doc, "income_tax_deducted", money(tax_total));
    Ok(())
}

/// This period's income tax: the year's tax on projected taxable salary (with the year's opening
/// figures and what earlier slips paid, off-cycle ones included), less tax already deducted, spread
/// over the periods left; additional salary and arrears raise the year's projection like regular
/// pay. What is taken in full (an off-cycle slip's pay, additional salary marked Deduct Full Tax)
/// adds the tax it brings to the year, now. A leaver's last slip settles the year: the tax on what
/// it actually paid less what was deducted.
#[allow(clippy::too_many_arguments)]
async fn income_tax(
    hc: &mut HookCtx<'_>,
    doc: &Doc,
    ssa: &Doc,
    co: &Company,
    frequency: &str,
    (start, end, leaving, off_cycle): (NaiveDate, NaiveDate, bool, bool),
    (regular_now, one_off): (Decimal, Decimal),
    taxable_full: Decimal,
) -> Result<Decimal> {
    let slab = lookup(hc, "IncomeTaxSlab", id(ssa, "income_tax_slab"))
        .await?
        .ok_or_else(|| Error::field("employee", format!("salary structure assignment {} has no income tax slab", s(ssa, "number").unwrap_or_default())))?;
    let period: Option<(Uuid, NaiveDate, NaiveDate)> = sqlx::query_as(
        "select id, start_date, end_date from e_payroll_period where tenant_id = $1 and company = $2 and start_date <= $3 and end_date >= $3 limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(co.id)
    .bind(end)
    .fetch_optional(&mut **hc.tx)
    .await?;
    let (period, p_start, p_end) =
        period.ok_or_else(|| Error::field("end_date", format!("no payroll period of the company covers {end}; create one for income tax")))?;
    let (prev_taxable, prev_tax): (Decimal, Decimal) = sqlx::query_as(
        "select coalesce(sum(taxable_earnings), 0), coalesce(sum(income_tax_deducted), 0) from e_salary_slip where tenant_id = $1 and employee = $2 \
         and docstatus = 1 and start_date >= $3 and start_date <= $6 and (end_date < $4 or coalesce(off_cycle, false)) and ($5::uuid is null or id <> $5)",
    )
    .bind(hc.ctx.tenant)
    .bind(id(doc, "employee"))
    .bind(p_start)
    .bind(start)
    .bind(id(doc, "id"))
    .bind(p_end)
    .fetch_one(&mut **hc.tx)
    .await?;
    // What was paid before the first slip made here, and by a previous employer that year.
    let (opening_taxable, opening_tax): (Decimal, Decimal) = sqlx::query_as(
        "select coalesce(sum(coalesce(taxable_earnings, 0) + coalesce(previous_employer_income, 0)), 0), \
         coalesce(sum(coalesce(tax_deducted, 0) + coalesce(previous_employer_tax, 0)), 0) from e_payroll_opening \
         where tenant_id = $1 and employee = $2 and payroll_period = $3 and docstatus = 1",
    )
    .bind(hc.ctx.tenant)
    .bind(id(doc, "employee"))
    .bind(period)
    .fetch_one(&mut **hc.tx)
    .await?;
    let (prev_taxable, prev_tax) = (prev_taxable + opening_taxable, prev_tax + opening_tax);
    // A leaver's last slip settles the year: nothing is projected or spread beyond it.
    let left = Decimal::from(if leaving { 1 } else { periods_left(frequency, start, p_end) });
    let declared: Decimal = if b(&slab, "allow_tax_exemption") {
        sqlx::query_scalar(
            "select coalesce(sum(total_exemption_amount), 0) from e_employee_tax_exemption_declaration where tenant_id = $1 and employee = $2 \
             and payroll_period = $3 and docstatus = 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(period)
        .fetch_one(&mut **hc.tx)
        .await?
    } else {
        Decimal::ZERO
    };
    let exempt = d(&slab, "standard_tax_exemption_amount") + declared;
    let slabs: Vec<(Decimal, Decimal, Decimal)> =
        rows(&slab, "slabs").iter().map(|r| (d(r, "from_amount"), d(r, "to_amount"), d(r, "percent_deduction"))).collect();
    let charges: Vec<(Decimal, Decimal, Decimal)> =
        rows(&slab, "other_taxes_and_charges").iter().map(|r| (d(r, "percent"), d(r, "min_taxable_income"), d(r, "max_taxable_income"))).collect();
    let rebate = (d(&slab, "rebate_limit"), d(&slab, "rebate_max"));
    let tax = |annual: Decimal| annual_tax(annual.max(Decimal::ZERO), &slabs, rebate, &charges);
    let due = if off_cycle {
        // The year as projected without this slip, its own month included.
        let base = prev_taxable + taxable_full * left - exempt;
        tax(base + one_off) - tax(base)
    } else {
        let base = prev_taxable + regular_now + taxable_full * (left - Decimal::ONE) - exempt;
        if leaving { tax(base + one_off) - prev_tax } else { (tax(base) - prev_tax).max(Decimal::ZERO) / left + tax(base + one_off) - tax(base) }
    };
    Ok(due.max(Decimal::ZERO))
}

/// The payroll entry a slip belongs to, when its slips have been submitted: a slip made or
/// corrected after that posts its own accrual.
async fn submitted_run(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Option<Doc>> {
    Ok(lookup(hc, "PayrollEntry", id(doc, "payroll_entry")).await?.filter(|pe| d(pe, "docstatus") == Decimal::ONE && b(pe, "salary_slips_submitted")))
}

/// Takes back a slip of a submitted payroll entry and makes it again as a draft for the same
/// period: its share of the run's accrual reversed, nobody else's touched. The draft is submitted
/// on its own, posting its own accrual; what the bank already paid becomes what was paid of it.
async fn correct(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
    let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
    let label = format!("salary slip {}", s(doc, "number").unwrap_or_default());
    let run = submitted_run(hc, doc)
        .await?
        .ok_or_else(|| Error::invalid(format!("{label} is not a slip of a payroll entry whose slips are submitted; cancel it and make it again")))?;
    if let Some(next) = lookup(hc, "SalarySlip", id(doc, "replaced_by")).await? {
        return Err(Error::invalid(format!("{label} is already corrected by {}", s(&next, "number").unwrap_or_default())));
    }
    hc.ctx.require("SalarySlip", ferrum_kernel::Action::Create)?;
    hc.kernel.cancel_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, None).await?;
    let mut slip = Doc::new();
    for f in ["employee", "start_date", "end_date", "payroll_entry", "off_cycle"] {
        set(&mut slip, f, doc.get(f).cloned().unwrap_or_default());
    }
    set(&mut slip, "posting_date", crate::today().to_string());
    set(&mut slip, "replaces", me.to_string());
    let made = hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "SalarySlip", slip).await?;
    let made_id = id(&made, "id").unwrap_or_default();
    if b(doc, "held") {
        let mut v = Doc::new();
        for f in ["held", "hold_reason"] {
            v.insert(f.into(), doc.get(f).cloned().unwrap_or_default());
        }
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", made_id, v).await?;
    }
    run::refresh_totals(hc, &run, 1, Doc::new()).await?;
    Ok(json!({
        "entity": "SalarySlip",
        "id": made_id.to_string(),
        "message": format!("{label} is cancelled and its accrual taken back; its correction {} is a draft: put right what needs it, then submit it",
            s(&made, "number").unwrap_or_default()),
    }))
}

/// Holds a slip's salary back from bank payments, or releases it to the next one.
async fn hold(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value, holding: bool) -> Result<Value> {
    let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
    let who = s(doc, "employee_name").unwrap_or("the employee").to_string();
    let period = format!("{} to {}", s(doc, "start_date").unwrap_or_default(), s(doc, "end_date").unwrap_or_default());
    let mut v = Doc::new();
    if holding {
        let reason = params.get("reason").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty());
        let reason = reason.ok_or_else(|| Error::field("reason", "say why the salary is held"))?;
        if id(doc, "payroll_entry").is_none() {
            return Err(Error::invalid(format!("{who}'s slip for {period} is not paid through a payroll entry; there is no bank payment to hold it from")));
        }
        if d(doc, "paid_amount") > Decimal::ZERO {
            return Err(Error::invalid(format!("{who}'s salary for {period} is already paid ({}); it cannot be held", d(doc, "paid_amount").normalize())));
        }
        set(&mut v, "held", true);
        set(&mut v, "hold_reason", reason);
        set(&mut v, "released_on", Value::Null);
    } else {
        if !b(doc, "held") {
            return Err(Error::invalid(format!("{who}'s salary for {period} is not held")));
        }
        set(&mut v, "held", false);
        set(&mut v, "released_on", crate::today().to_string());
    }
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await?;
    if let Some(run) = lookup(hc, "PayrollEntry", id(doc, "payroll_entry")).await? {
        run::refresh_totals(hc, &run, 1, Doc::new()).await?;
        super::payments::run_status(hc, id(&run, "id").unwrap_or_default()).await?;
    }
    let message = if holding {
        format!("{who}'s salary for {period} is held: it is left out of bank payments until released")
    } else {
        format!("{who}'s salary for {period} is released: the next bank payment that lists them pays it, or their full and final statement")
    };
    Ok(json!({"message": message}))
}

#[async_trait]
impl DocHook for SlipHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0 {
            set(doc, "status", "Draft");
            compute(hc, doc).await?;
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(start), Some(end)) = (opt_date(doc, "start_date"), opt_date(doc, "end_date")) else { return Ok(()) };
        let off_cycle = b(doc, "off_cycle");
        let clash: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus < 2 and id <> $3 \
             and start_date <= $5 and end_date >= $4 and coalesce(off_cycle, false) = $6 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(id(doc, "id"))
        .bind(start)
        .bind(end)
        .bind(off_cycle)
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = clash {
            return Err(Error::field(
                "start_date",
                if off_cycle {
                    format!("off-cycle salary slip {n} already pays the employee on this day")
                } else {
                    format!("salary slip {n} already covers part of this period for the employee")
                },
            ));
        }
        // Opening figures count the pay up to their day; a slip here starts after it, so nothing counts twice.
        let opening: Option<(String, NaiveDate)> = sqlx::query_as(
            "select coalesce(o.number, o.id::text), o.up_to_date from e_payroll_opening o join e_payroll_period p on p.tenant_id = o.tenant_id \
             and p.id = o.payroll_period where o.tenant_id = $1 and o.employee = $2 and o.docstatus = 1 and p.start_date <= $3 and o.up_to_date >= $3 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(start)
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some((n, up_to)) = opening {
            return Err(Error::field(
                "start_date",
                format!(
                    "opening payroll figures {n} already count {}'s pay up to {up_to}; a slip made here starts after it",
                    s(doc, "employee_name").unwrap_or("the employee")
                ),
            ));
        }
        Ok(())
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if id(doc, "payroll_entry").is_some() && !hc.ctx.is_system && submitted_run(hc, doc).await?.is_none() {
            return Err(Error::invalid("this slip belongs to a payroll entry; submit it from the payroll entry"));
        }
        Ok(())
    }

    /// A slip outside a payroll entry, or made or corrected after the entry's slips were submitted,
    /// posts its own accrual; slips of a payroll entry are posted together by the entry. A correction
    /// takes over what was paid of the slip it replaces, and a slip that recovers an excess settles it.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
        let mut v = Doc::new();
        set(&mut v, "status", "Submitted");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await?;
        let late = submitted_run(hc, doc).await?;
        if super::posts(hc) {
            let cid = id(doc, "company").unwrap_or_default();
            let on = opt_date(doc, "posting_date").unwrap_or_else(crate::today);
            let source = ("SalarySlip", me, s(doc, "number").map(str::to_string));
            match (&late, id(doc, "payroll_entry")) {
                (_, None) => {
                    let payable = crate::people::company_account(hc, cid, "pay_payroll_payable_account", "payroll payable account").await?;
                    run::post_accrual(hc, cid, source, on, std::slice::from_ref(doc), payable, None, Decimal::ONE).await?;
                }
                (Some(pe), Some(_)) => {
                    let payable = id(pe, "payroll_payable_account").ok_or_else(|| Error::field("payroll_payable_account", "is required"))?;
                    run::post_accrual(hc, cid, source, on, std::slice::from_ref(doc), payable, id(pe, "cost_center"), Decimal::ONE).await?;
                }
                _ => {}
            }
        }
        let Some(run_id) = id(doc, "payroll_entry") else { return Ok(()) };
        // The slip this one corrects: the cancelled slip of the same run and period not yet replaced.
        let old: Option<Uuid> = sqlx::query_scalar(
            "select id from e_salary_slip where tenant_id = $1 and payroll_entry = $2 and employee = $3 and start_date = $4 and end_date = $5 \
             and coalesce(off_cycle, false) = $6 and docstatus = 2 and replaced_by is null and ($7::uuid is null or id = $7) order by updated_at desc limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(run_id)
        .bind(id(doc, "employee"))
        .bind(opt_date(doc, "start_date"))
        .bind(opt_date(doc, "end_date"))
        .bind(b(doc, "off_cycle"))
        .bind(id(doc, "replaces"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(old) = old {
            let sctx = sys(hc.ctx);
            let mut v = Doc::new();
            set(&mut v, "replaced_by", me.to_string());
            hc.kernel.set_values_tx(&sctx, hc.tx, "SalarySlip", old, v).await?;
            let mut v = Doc::new();
            set(&mut v, "replaces", old.to_string());
            hc.kernel.set_values_tx(&sctx, hc.tx, "SalarySlip", me, v).await?;
            super::payments::take_over(hc, old, me).await?;
        }
        // The excess this slip takes back settles the slips that paid it, oldest first.
        let mut left = d(doc, "recovery_amount");
        let mut settled = Vec::new();
        for (slip, over) in super::payments::excess(hc, id(doc, "employee").unwrap_or_default(), Some(me)).await? {
            if left <= Decimal::ZERO {
                break;
            }
            let x = over.min(left);
            settled.push((slip, -x));
            left -= x;
        }
        super::payments::allocate(hc, me, &settled).await?;
        super::payments::refresh(hc, &[me]).await?;
        super::loans::slip_changed(hc, doc).await?;
        // The periods it carried, and its own, show what still waits.
        for slip in std::iter::once(me).chain(rows(doc, "adjustments").iter().filter_map(|r| id(r, "salary_slip"))) {
            refresh_pending(hc, slip).await?;
        }
        if let Some(pe) = late {
            run::refresh_totals(hc, &pe, 1, Doc::new()).await?;
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(pe) = lookup(hc, "PayrollEntry", id(doc, "payroll_entry")).await?
            && d(&pe, "docstatus") == Decimal::ONE
            && !hc.ctx.is_system
        {
            return Err(Error::invalid(format!(
                "this slip was posted by payroll entry {}; correct it (Correct), or cancel the payroll entry instead",
                s(&pe, "number").unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Reverses what the slip posted itself. A slip of a submitted run being corrected has its share
    /// of the run's accrual taken back; what it recovered from earlier slips is undone.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
        if super::posts(hc) {
            let cid = id(doc, "company").unwrap_or_default();
            let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
            let reversed = ledger::reverse(hc.tx, hc.ctx, "SalarySlip", me, crate::today(), co.precision).await?;
            if reversed == 0
                && let Some(pe) = submitted_run(hc, doc).await?
            {
                let payable = id(&pe, "payroll_payable_account").ok_or_else(|| Error::field("payroll_payable_account", "is required"))?;
                let source = ("SalarySlip", me, s(doc, "number").map(str::to_string));
                run::post_accrual(hc, cid, source, crate::today(), std::slice::from_ref(doc), payable, id(&pe, "cost_center"), -Decimal::ONE).await?;
            }
        }
        super::payments::unallocate(hc, me).await?;
        super::loans::slip_changed(hc, doc).await?;
        for slip in rows(doc, "adjustments").iter().filter_map(|r| id(r, "salary_slip")) {
            refresh_pending(hc, slip).await?;
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "make_correction" => correct(hc, doc).await.map(Some),
            "hold_salary" => hold(hc, doc, params, true).await.map(Some),
            "release_salary" => hold(hc, doc, params, false).await.map(Some),
            "choose_late_changes" => choose_late_changes(hc, doc, params).await.map(Some),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_left_in_the_tax_year() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(periods_left("Monthly", d("2026-04-01"), d("2027-03-31")), 12);
        assert_eq!(periods_left("Monthly", d("2026-10-01"), d("2027-03-31")), 6);
        assert_eq!(periods_left("Monthly", d("2027-03-01"), d("2027-03-31")), 1);
        assert_eq!(periods_left("Weekly", d("2027-03-15"), d("2027-03-31")), 3);
        assert_eq!(periods_left("Fortnightly", d("2027-03-01"), d("2027-03-31")), 3);
        assert_eq!(periods_left("Monthly", d("2027-05-01"), d("2027-03-31")), 1);
        assert_eq!(round_amount(Decimal::new(15750, 2), true, 2), Decimal::new(15800, 2));
    }

    #[test]
    fn periods_of_pay_that_repeats() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(periods_between("Monthly", d("2026-09-15"), d("2026-09-30")), 0);
        assert_eq!(periods_between("Monthly", d("2026-09-15"), d("2027-01-31")), 4);
        assert_eq!(periods_between("Weekly", d("2026-10-05"), d("2026-10-11")), 0);
        assert_eq!(periods_between("Weekly", d("2026-10-05"), d("2026-10-18")), 1);
        assert_eq!(periods_between("Fortnightly", d("2026-10-05"), d("2026-11-01")), 1);
    }
}
