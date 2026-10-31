//! Salary slips, recomputed on every save while draft:
//!
//! * working days are the period's days (less holidays when the company excludes them);
//!   payment days are the working days the employee was employed (joining/relieving), less
//!   approved leave without pay and absences marked in attendance (half days count half);
//! * structure rows are evaluated earnings first, then deductions, in order. Formulas see the
//!   full-period amounts of the rows above (`BS`, `HRA`, … and `gross`), and each row is then
//!   prorated by payment days when its component says so (ERPNext's semantics);
//! * additional salary for the period is added (or replaces the structure amount); income tax is
//!   the year's tax on projected taxable salary, less tax already deducted, spread over the
//!   periods left in the payroll period.

use super::formula::{Formula, Vars, slab_tax};
use super::{FLAGS, assignment_on, row_formula, run};
use crate::calendar::{employee_holidays, working_days};
use crate::{lookup, opt_date, stamp_company};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use ferrum_app_erp::ledger;
use ferrum_domain::util::{Company, b, company, d, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Doc, DocHook, Error, FieldError, HookCtx, Result};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

pub(crate) struct SlipHook;

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
}

impl Row {
    fn flag(&self, f: &str) -> bool {
        b(&self.component, f)
    }
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

pub(crate) async fn compute(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    let Some(emp) = stamp_company(hc, doc).await? else { return Ok(()) };
    let (Some(start), Some(end), Some(cid)) = (opt_date(doc, "start_date"), opt_date(doc, "end_date"), id(doc, "company")) else { return Ok(()) };
    if end < start {
        return Ok(());
    }
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
    let from = opt_date(&emp, "date_of_joining").map_or(start, |j| j.max(start));
    let to = opt_date(&emp, "relieving_date").map_or(end, |r| r.min(end));
    let employed = count(from, to);
    let (lwp, absent) = if employed > 0 {
        (crate::leave::lwp_days(hc, emp_id, from, to, &holidays, !exclude).await?, absent_days(hc, emp_id, from, to).await?)
    } else {
        (Decimal::ZERO, Decimal::ZERO)
    };
    let payment_days = (Decimal::from(employed) - lwp - absent).max(Decimal::ZERO);
    set(doc, "total_working_days", money(Decimal::from(working)));
    set(doc, "leave_without_pay", money(lwp));
    set(doc, "absent_days", money(absent));
    set(doc, "payment_days", money(payment_days));

    // ── components ──
    let mut vars = Vars::default();
    vars.set("base", d(&ssa, "base"));
    vars.set("variable", d(&ssa, "variable"));
    vars.set("payment_days", payment_days);
    vars.set("working_days", Decimal::from(working));
    vars.set("lwp_days", lwp);
    vars.set("absent_days", absent);
    vars.set("gross", Decimal::ZERO);
    texts(hc, &emp, &mut vars).await?;
    let mut extra: BTreeMap<Uuid, (Decimal, bool)> = sqlx::query_as::<_, (Uuid, Decimal, bool)>(
        "select salary_component, sum(amount), bool_or(overwrite_salary_structure_amount) from e_additional_salary \
         where tenant_id = $1 and employee = $2 and docstatus = 1 and payroll_date between $3 and $4 group by salary_component",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(start)
    .bind(end)
    .fetch_all(&mut **hc.tx)
    .await?
    .into_iter()
    .map(|(c, a, o)| (c, (a, o)))
    .collect();
    let cx = Proration { co: &co, payment_days, working: Decimal::from(working) };
    let mut out: HashMap<&str, Vec<Row>> = HashMap::new();
    let mut tax: Vec<Doc> = Vec::new();
    let mut gross_full = Decimal::ZERO;
    let mut errors = Vec::new();
    for table in ["earnings", "deductions"] {
        for r in rows(&structure, table) {
            if let Some(a) = s(&r, "abbr") {
                vars.set(a, Decimal::ZERO);
            }
        }
    }
    for table in ["earnings", "deductions"] {
        for (i, r) in rows(&structure, table).into_iter().enumerate() {
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
            let mut amount = cx.prorate(full, &comp);
            let mut additional = Decimal::ZERO;
            if let Some((a, overwrite)) = extra.remove(&id(&comp, "id").unwrap_or_default()) {
                additional = a;
                amount = if overwrite { a } else { amount + a };
            }
            if table == "earnings" && !b(&comp, "statistical_component") {
                gross_full += full;
                vars.set("gross", gross_full);
            }
            out.entry(table).or_default().push(Row { component: comp, amount, default_amount: full, additional });
        }
    }
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    // Additional salary for components outside the structure.
    for (comp_id, (a, _)) in extra {
        let comp = get(hc.kernel, hc.ctx, hc.tx, "SalaryComponent", comp_id).await?;
        let table = if s(&comp, "component_type") == Some("Deduction") { "deductions" } else { "earnings" };
        out.entry(table).or_default().push(Row { component: comp, amount: a, default_amount: Decimal::ZERO, additional: a });
    }

    // ── income tax ──
    let sum = |rows: &[Row], f: fn(&Row) -> Decimal, keep: &dyn Fn(&Row) -> bool| -> Decimal { rows.iter().filter(|r| keep(r)).map(f).sum() };
    let earn = out.remove("earnings").unwrap_or_default();
    let mut ded = out.remove("deductions").unwrap_or_default();
    let taxable_earning = |r: &Row| r.flag("is_tax_applicable") && !r.flag("statistical_component");
    let exempt_deduction = |r: &Row| r.flag("exempted_from_income_tax") && !r.flag("statistical_component");
    let taxable_now = sum(&earn, |r| r.amount, &taxable_earning) - sum(&ded, |r| r.amount, &exempt_deduction);
    let taxable_full = sum(&earn, |r| r.default_amount, &taxable_earning) - sum(&ded, |r| r.default_amount, &exempt_deduction);
    let mut tax_total = Decimal::ZERO;
    for comp in tax {
        let t = income_tax(hc, doc, &ssa, &co, &frequency, start, end, taxable_now, taxable_full).await?;
        let t = round_amount(t, b(&comp, "round_to_the_nearest_integer"), co.precision);
        tax_total += t;
        ded.push(Row { component: comp, amount: t, default_amount: t, additional: Decimal::ZERO });
    }

    // ── totals and rows ──
    let counted = |r: &Row| !r.flag("statistical_component");
    let gross = sum(&earn, |r| r.amount, &counted);
    let deductions = sum(&ded, |r| r.amount, &counted);
    let net = gross - deductions;
    if net < Decimal::ZERO {
        return Err(Error::invalid(format!("deductions ({}) exceed earnings ({}) for {name}", deductions.normalize(), gross.normalize())));
    }
    for (table, list) in [("earnings", earn), ("deductions", ded)] {
        let old: HashMap<String, String> =
            rows(doc, table).iter().filter_map(|r| Some((s(r, "salary_component")?.to_string(), s(r, "id")?.to_string()))).collect();
        let mut made = Vec::new();
        for r in list.into_iter().filter(|r| !r.amount.is_zero() || !r.additional.is_zero()) {
            let comp_id = s(&r.component, "id").unwrap_or_default().to_string();
            let mut row = Doc::new();
            set(&mut row, "id", old.get(&comp_id).cloned().unwrap_or_else(|| Uuid::now_v7().to_string()));
            set(&mut row, "idx", made.len());
            set(&mut row, "salary_component", comp_id);
            set(&mut row, "abbr", r.component.get("abbr").cloned().unwrap_or_default());
            set(&mut row, "amount", money(r.amount));
            set(&mut row, "default_amount", money(r.default_amount));
            set(&mut row, "additional_amount", money(r.additional));
            for f in FLAGS {
                set(&mut row, f, r.flag(f));
            }
            made.push(Value::Object(row));
        }
        set(doc, table, Value::Array(made));
    }
    set(doc, "gross_pay", money(gross));
    set(doc, "total_deduction", money(deductions));
    set(doc, "net_pay", money(net));
    set(doc, "rounded_total", money(round_amount(net, true, co.precision)));
    set(doc, "taxable_earnings", money(taxable_now));
    set(doc, "income_tax_deducted", money(tax_total));
    Ok(())
}

/// This period's share of the year's income tax.
#[allow(clippy::too_many_arguments)]
async fn income_tax(
    hc: &mut HookCtx<'_>,
    doc: &Doc,
    ssa: &Doc,
    co: &Company,
    frequency: &str,
    start: NaiveDate,
    end: NaiveDate,
    taxable_now: Decimal,
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
         and docstatus = 1 and start_date >= $3 and end_date < $4 and ($5::uuid is null or id <> $5)",
    )
    .bind(hc.ctx.tenant)
    .bind(id(doc, "employee"))
    .bind(p_start)
    .bind(start)
    .bind(id(doc, "id"))
    .fetch_one(&mut **hc.tx)
    .await?;
    let left = periods_left(frequency, start, p_end);
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
    let annual =
        (prev_taxable + taxable_now + taxable_full * Decimal::from(left - 1) - d(&slab, "standard_tax_exemption_amount") - declared).max(Decimal::ZERO);
    let slabs: Vec<(Decimal, Decimal, Decimal)> =
        rows(&slab, "slabs").iter().map(|r| (d(r, "from_amount"), d(r, "to_amount"), d(r, "percent_deduction"))).collect();
    let due = (slab_tax(annual, &slabs) - prev_tax).max(Decimal::ZERO);
    Ok(due / Decimal::from(left))
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
        let clash: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus < 2 and id <> $3 \
             and start_date <= $5 and end_date >= $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(id(doc, "id"))
        .bind(start)
        .bind(end)
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = clash {
            return Err(Error::field("start_date", format!("salary slip {n} already covers part of this period for the employee")));
        }
        Ok(())
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if id(doc, "payroll_entry").is_some() && !hc.ctx.is_system {
            return Err(Error::invalid("this slip belongs to a payroll entry; submit it from the payroll entry"));
        }
        Ok(())
    }

    /// A slip outside a payroll entry posts its own accrual; slips of a payroll entry are posted
    /// together by the entry.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
        let mut v = Doc::new();
        set(&mut v, "status", "Submitted");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await?;
        if id(doc, "payroll_entry").is_none() && super::posts(hc) {
            let cid = id(doc, "company").unwrap_or_default();
            let payable = crate::people::company_account(hc, cid, "pay_payroll_payable_account", "payroll payable account").await?;
            let on = opt_date(doc, "posting_date").unwrap_or_else(crate::today);
            run::post_accrual(hc, cid, ("SalarySlip", me, s(doc, "number").map(str::to_string)), on, std::slice::from_ref(doc), payable, None).await?;
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(pe) = lookup(hc, "PayrollEntry", id(doc, "payroll_entry")).await?
            && d(&pe, "docstatus") == Decimal::ONE
        {
            return Err(Error::invalid(format!(
                "this slip was posted by payroll entry {}; cancel the payroll entry instead",
                s(&pe, "number").unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("slip without id"))?;
        if id(doc, "payroll_entry").is_none() && super::posts(hc) {
            let co = company(hc.kernel, hc.ctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
            ledger::reverse(hc.tx, hc.ctx, "SalarySlip", me, crate::today(), co.precision).await?;
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalarySlip", me, v).await
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
}
