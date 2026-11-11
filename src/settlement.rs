//! The full and final statement of an employee who leaves, gathered from their records. Paid to
//! them: the pay earned up to the last working day that no slip covers yet (a salary slip of its
//! own, which settles the year's income tax, submitted with the statement), their encashable leave
//! as of that day (used up from their balance on submit), notice pay when the employer ended the
//! notice early, gratuity by the company's rule, their open expense claims, and a salary of a payroll
//! run held or still owed (a correction, a release). Recovered from them: notice not served, their
//! unsettled advances, and what a corrected run slip paid them too much. Each line can be adjusted with a reason, and lines
//! can be added. One journal entry settles it: it debits what is paid, credits what is recovered
//! and pays the difference from the bank (or takes it in), and submitting it settles each claim and
//! advance the statement names, as their own payments do; cancelling it undoes that.

use crate::expense::{advance_moved, bank_param, claim_paid, net_on, payment_entry};
use crate::leave::{self, LedgerEntry, allocation_on, available};
use crate::payroll::formula::{Formula, Vars};
use crate::people::company_account;
use crate::{lookup, opt_date, stamp_company};
use async_trait::async_trait;
use chrono::{Datelike, Duration, Months, NaiveDate};
use ferrum_domain::util::{Company, b, company, d, elevated, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, HookCtx, HookRegistry, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// The journal entry's `hr_reference_type` for a statement.
pub(crate) const KIND: &str = "Full and Final Statement";
const ENTITY: &str = "FullAndFinalStatement";
const CLAIM: &str = "Expense Claim";
const ADVANCE: &str = "Employee Advance";
const SLIP: &str = "Salary Slip";
/// What is still owed of an employee loan (with the connector's payroll).
const LOAN: &str = "Employee Loan";
const ENCASHMENT: &str = "Leave Encashment";
/// A payroll run's slip still owed (or paid too much) on the last day.
const RUN_SLIP: &str = "Payroll Salary";

pub(crate) fn register(h: &mut HookRegistry) {
    h.register(ENTITY, Arc::new(StatementHook)).register("EmployeeSeparation", Arc::new(FromSeparation));
}

fn docstatus(doc: &Doc) -> i64 {
    doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0)
}

/// The document a gathered line names, read for settling (locked when `write`), with what it is
/// called in messages. It must be submitted and the statement's employee's.
async fn referenced(hc: &mut HookCtx<'_>, st: &Doc, line: &Doc, write: bool) -> Result<Option<(Doc, String)>> {
    let entity = match s(line, "reference_type") {
        Some(CLAIM) => "ExpenseClaim",
        Some(ADVANCE) => "EmployeeAdvance",
        _ => return Ok(None),
    };
    let target =
        id(line, "reference").ok_or_else(|| Error::invalid(format!("a line for {} names no document", s(line, "reference_number").unwrap_or("a document"))))?;
    let doc = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, entity, target, write).await?;
    let label = format!("{} {}", s(line, "reference_type").unwrap_or_default(), s(&doc, "number").unwrap_or_default());
    if docstatus(&doc) != 1 {
        return Err(Error::invalid(format!("{label} is not submitted")));
    }
    if id(&doc, "employee") != id(st, "employee") {
        return Err(Error::invalid(format!("{label} is another employee's")));
    }
    Ok(Some((doc, label)))
}

/// Settles (`sign` 1) or unsettles (−1) every claim and advance the statement names: a claim is
/// paid its line's amount, an advance has it returned. Checks only, unless `write`.
async fn settle_lines(hc: &mut HookCtx<'_>, st: &Doc, sign: Decimal, write: bool) -> Result<()> {
    for line in rows(st, "payables").iter().chain(rows(st, "receivables").iter()) {
        // A loan's balance is closed by the statement's entry: an event of the loan naming the statement.
        if s(line, "reference_type") == Some(LOAN) {
            let Some(loan) = lookup(hc, "EmployeeLoan", id(line, "reference")).await? else { continue };
            let lid = id(&loan, "id").unwrap_or_default();
            let st_id = s(st, "id").unwrap_or_default().to_string();
            if sign > Decimal::ZERO {
                if d(line, "amount") > d(&loan, "balance") {
                    return Err(Error::invalid(format!(
                        "employee loan {} has {} left, less than the statement recovers; get the outstanding amounts again",
                        s(&loan, "number").unwrap_or_default(),
                        d(&loan, "balance").normalize()
                    )));
                }
                if write {
                    let e = json!({"event_date": crate::today().to_string(), "event_type": "Settled by Statement", "amount": money(d(line, "amount")),
                                   "account": line.get("account"), "reference": st_id, "reason": format!("Full and final statement {}", s(st, "number").unwrap_or_default())});
                    crate::payroll::loans::event(hc, lid, Some(e), None).await?;
                }
            } else if write {
                crate::payroll::loans::event(hc, lid, None, Some(&st_id)).await?;
            }
            continue;
        }
        let Some((doc, label)) = referenced(hc, st, line, write).await? else { continue };
        let delta = sign * d(line, "amount");
        if s(line, "reference_type") == Some(CLAIM) {
            claim_paid(hc, &doc, &label, delta, write).await?;
        } else {
            advance_moved(hc, &doc, &label, "return_amount", delta, write).await?;
        }
    }
    Ok(())
}

/// A journal entry referencing a statement: on each of the statement's accounts it posts what the
/// statement says (what is paid debited, what is recovered credited), the rest being the bank.
/// Submitting it settles the statement's claims and advances and marks it paid; cancelling it
/// undoes both.
pub(crate) async fn settle(hc: &mut HookCtx<'_>, je: &Doc, target: Uuid, sign: Decimal, write: bool) -> Result<()> {
    let st = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, ENTITY, target, write).await?;
    let label = format!("{KIND} {}", s(&st, "number").unwrap_or_default());
    if docstatus(&st) != 1 {
        return Err(Error::invalid(format!("{label} is not submitted")));
    }
    if id(&st, "company") != id(je, "company") {
        return Err(Error::invalid(format!("{label} belongs to another company")));
    }
    let paying = sign > Decimal::ZERO;
    if s(&st, "status") != Some(if paying { "Unpaid" } else { "Paid" }) {
        return Err(Error::invalid(if paying { format!("{label} is already paid") } else { format!("{label} is not paid") }));
    }
    let mut want: HashMap<Uuid, Decimal> = HashMap::new();
    for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
        for r in rows(&st, table) {
            let account = id(&r, "account").ok_or_else(|| Error::invalid(format!("a line of {label} has no account")))?;
            *want.entry(account).or_default() += side * d(&r, "amount");
        }
    }
    if want.iter().any(|(account, net)| net_on(je, Some(*account)) != *net) {
        return Err(Error::invalid(format!("the entry must debit what {label} pays and credit what it recovers, on their accounts")));
    }
    settle_lines(hc, &st, sign, write).await?;
    // The run slips it pays (or recovers) are paid by this entry, if nothing paid them meanwhile.
    if paying {
        for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
            for r in rows(&st, table).iter().filter(|r| s(r, "reference_type") == Some(RUN_SLIP)) {
                let slip = lookup(hc, "SalarySlip", id(r, "reference")).await?.filter(|sl| docstatus(sl) == 1);
                if slip.is_none_or(|sl| d(&sl, "net_pay") - d(&sl, "paid_amount") != side * d(r, "amount")) {
                    return Err(Error::invalid(format!(
                        "{} was settled since {label} was made; cancel the statement and make it again",
                        s(r, "component").unwrap_or("a salary slip")
                    )));
                }
            }
        }
    }
    if write {
        let je_id = id(je, "id").ok_or_else(|| Error::internal("journal entry without id"))?;
        if paying {
            let mut items = Vec::new();
            for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
                for r in rows(&st, table).iter().filter(|r| s(r, "reference_type") == Some(RUN_SLIP)) {
                    items.push((id(r, "reference").unwrap_or_default(), side * d(r, "amount")));
                }
            }
            crate::payroll::payments::allocate(hc, je_id, &items).await?;
        } else {
            crate::payroll::payments::unallocate(hc, je_id).await?;
        }
    }
    if write {
        let mut v = Doc::new();
        set(&mut v, "status", if paying { "Paid" } else { "Unpaid" });
        set(&mut v, "journal_entry", if paying { json!(s(je, "id").unwrap_or_default()) } else { Value::Null });
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, ENTITY, target, v).await?;
    }
    Ok(())
}

/// What is open each way for the statement's employee: their approved claims not yet paid, and
/// their paid advances not yet claimed or returned. Lines added by hand are kept as they are.
async fn outstanding(hc: &mut HookCtx<'_>, st: &Doc) -> Result<(Vec<Value>, Vec<Value>)> {
    let employee = id(st, "employee").ok_or_else(|| Error::field("employee", "is required"))?;
    let claims: Vec<(Uuid, Option<String>, Option<Uuid>, Decimal, NaiveDate)> = sqlx::query_as(
        "select id, number, payable_account, outstanding_amount, posting_date from e_expense_claim \
         where tenant_id = $1 and employee = $2 and docstatus = 1 and status = 'Unpaid' and outstanding_amount > 0 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?;
    let advances: Vec<(Uuid, Option<String>, Option<Uuid>, Decimal, Option<String>)> = sqlx::query_as(
        "select id, number, advance_account, open, purpose from (select id, number, advance_account, purpose, posting_date, created_at, \
         coalesce(paid_amount, 0) - coalesce(claimed_amount, 0) - coalesce(return_amount, 0) as open from e_employee_advance \
         where tenant_id = $1 and employee = $2 and docstatus = 1) a where open > 0 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?;
    let by_hand = |table: &str| rows(st, table).into_iter().filter(|r| s(r, "reference_type").is_none()).map(Value::Object);
    let (mut payables, mut receivables) = if hc.ctx.schema.entity("SalarySlip").is_some() { leaving(hc, st).await? } else { (Vec::new(), Vec::new()) };
    if hc.ctx.schema.packages.iter().any(|p| p == crate::PAYROLL_ERP) {
        let (pay, recover) = run_slips(hc, st, employee).await?;
        payables.extend(pay);
        receivables.extend(recover);
    }
    payables.extend(claims
        .into_iter()
        .map(|(claim, number, account, amount, posted)| {
            json!({"component": "Expense claim", "reference_type": CLAIM, "reference": claim.to_string(), "reference_number": number,
                   "account": account.map(|a| a.to_string()), "amount": money(amount), "gathered_amount": money(amount), "remark": format!("Approved, posted {posted}")})
        }));
    // Loans: what is owed after the last pay's own slip takes its instalment.
    if hc.ctx.schema.entity("EmployeeLoan").is_some() {
        let own = match payables.iter().find(|l| l.get("reference_type").and_then(Value::as_str) == Some(SLIP)) {
            Some(l) => lookup(hc, "SalarySlip", l.get("reference").and_then(Value::as_str).and_then(|v| v.parse().ok())).await?,
            None => None,
        };
        for (loan, number, account, balance) in crate::payroll::loans::open_for(hc, employee).await? {
            let by_last_pay: Decimal = own.iter().flat_map(|sl| rows(sl, "loans")).filter(|r| id(r, "loan") == Some(loan)).map(|r| d(&r, "principal")).sum();
            let amount = balance - by_last_pay;
            if amount > Decimal::ZERO {
                receivables.push(line(
                    "Employee loan".into(),
                    LOAN,
                    loan,
                    number,
                    account.unwrap_or_default(),
                    amount,
                    None,
                    "Still owed on the last working day".into(),
                ));
            }
        }
    }
    receivables.extend(advances.into_iter().map(|(advance, number, account, amount, purpose)| {
        json!({"component": "Employee advance", "reference_type": ADVANCE, "reference": advance.to_string(), "reference_number": number,
                   "account": account.map(|a| a.to_string()), "amount": money(amount), "gathered_amount": money(amount), "remark": purpose})
    }));
    // An amount a person adjusted, with their reason, stays when gathering again.
    for (table, list) in [("payables", &mut payables), ("receivables", &mut receivables)] {
        let old = rows(st, table);
        for line in list.iter_mut() {
            let same = |r: &&Doc| r.get("reference_type") == line.get("reference_type") && r.get("reference") == line.get("reference");
            if let Some(o) =
                old.iter().find(same).filter(|o| o.get("gathered_amount").is_some_and(|g| !g.is_null()) && d(o, "amount") != d(o, "gathered_amount"))
            {
                line["amount"] = o.get("amount").cloned().unwrap_or_default();
                line["reason"] = o.get("reason").cloned().unwrap_or_default();
            }
        }
    }
    payables.extend(by_hand("payables"));
    receivables.extend(by_hand("receivables"));
    Ok((payables, receivables))
}

/// Years of service from joining to the last working day (both counted), with a part year counted
/// as the company's rule says.
pub(crate) fn service_years(joined: NaiveDate, last: NaiveDate, part: &str) -> Decimal {
    let end = last + Duration::days(1);
    let after = |years: u32| joined.checked_add_months(Months::new(12 * years)).unwrap_or(NaiveDate::MAX);
    let mut years = 0;
    while after(years + 1) <= end {
        years += 1;
    }
    let (from, next) = (after(years), after(years + 1));
    let rest = match part {
        "Completed years only" => Decimal::ZERO,
        "Pro rata" => round(Decimal::from((end - from).num_days()) / Decimal::from((next - from).num_days()), 4),
        // India's: a part year of more than six months counts as a year.
        _ if from.checked_add_months(Months::new(6)).is_some_and(|half| end > half) => Decimal::ONE,
        _ => Decimal::ZERO,
    };
    Decimal::from(years) + rest
}

/// Whether `years` of service are complete by the last working day.
pub(crate) fn served(joined: NaiveDate, last: NaiveDate, years: u32) -> bool {
    joined.checked_add_months(Months::new(12 * years)).is_some_and(|d| d <= last + Duration::days(1))
}

/// A gathered line, with what was gathered kept beside the amount a person may adjust.
fn line(
    component: String,
    kind: &str,
    reference: Uuid,
    number: Option<String>,
    account: Uuid,
    amount: Decimal,
    quantity: Option<Decimal>,
    remark: String,
) -> Value {
    json!({"component": component, "reference_type": kind, "reference": reference.to_string(), "reference_number": number, "account": account.to_string(),
           "amount": money(amount), "gathered_amount": money(amount), "quantity": quantity.map(money), "remark": remark})
}

/// Payroll run slips the leaver's statement settles: a salary held (its entry pays it, held or not),
/// what is still owed on a correction or a released salary, and what a corrected slip paid too much.
async fn run_slips(hc: &mut HookCtx<'_>, st: &Doc, employee: Uuid) -> Result<(Vec<Value>, Vec<Value>)> {
    let (mut pay, mut recover) = (Vec::new(), Vec::new());
    let open = crate::payroll::payments::open_for_leaver(hc, employee).await?;
    if open.is_empty() {
        return Ok((pay, recover));
    }
    let cid = id(st, "company").ok_or_else(|| Error::field("company", "is required"))?;
    let payable = company_account(hc, cid, "pay_payroll_payable_account", "payroll payable account").await?;
    for (slip, number, from, to, owed, held) in open {
        let (component, remark) = match (owed > Decimal::ZERO, held) {
            (true, true) => (format!("Salary held, {from} to {to}"), "Held from its run's bank payment; paid by this statement"),
            (true, false) => (format!("Salary still owed, {from} to {to}"), "Corrected or released after its run was paid"),
            _ => (format!("Excess salary paid, {from} to {to}"), "Its corrected slip pays less than was paid"),
        };
        let l = line(component, RUN_SLIP, slip, number, payable, owed.abs(), None, remark.into());
        if owed > Decimal::ZERO { pay.push(l) } else { recover.push(l) }
    }
    Ok((pay, recover))
}

/// The pay earned up to the last working day that no salary slip covers yet: a draft slip of the
/// statement's own for the pay period the day falls in, made again at each gathering. None when a
/// slip (a payroll run's, or one made by hand) covers the day already, or nothing is assigned.
async fn last_pay(hc: &mut HookCtx<'_>, st: &Doc, emp: &Doc, last: NaiveDate) -> Result<Option<Doc>> {
    let user = elevated(hc.ctx);
    for r in rows(st, "payables").iter().filter(|r| s(r, "reference_type") == Some(SLIP)) {
        if let Some(old) = lookup(hc, "SalarySlip", id(r, "reference")).await?
            && docstatus(&old) == 0
        {
            hc.kernel.delete_tx(&user, hc.tx, "SalarySlip", id(&old, "id").unwrap_or_default(), None).await?;
        }
    }
    let emp_id = id(emp, "id").unwrap_or_default();
    let covered: bool = sqlx::query_scalar(
        "select exists (select 1 from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus < 2 and start_date <= $3 and end_date >= $3 \
         and not coalesce(off_cycle, false))",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(last)
    .fetch_one(&mut **hc.tx)
    .await?;
    let Some(ssa) = crate::payroll::assignment_on(hc, emp_id, last).await? else { return Ok(None) };
    if covered {
        return Ok(None);
    }
    let structure = get(hc.kernel, hc.ctx, hc.tx, "SalaryStructure", id(&ssa, "salary_structure").unwrap_or_default()).await?;
    let paid_to: Option<NaiveDate> = sqlx::query_scalar(
        "select max(end_date) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus < 2 and end_date < $3 and not coalesce(off_cycle, false)",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(last)
    .fetch_one(&mut **hc.tx)
    .await?;
    let after_paid = paid_to.map(|p| p + Duration::days(1));
    let (start, end) = match s(&structure, "payroll_frequency") {
        Some(f @ ("Weekly" | "Fortnightly")) => {
            let len = if f == "Weekly" { 7 } else { 14 };
            let start = after_paid.filter(|a| *a > last - Duration::days(len - 1)).unwrap_or(last - Duration::days(len - 1));
            (start, start + Duration::days(len - 1))
        }
        _ => {
            let first = last.with_day(1).unwrap_or(last);
            let month_end = first.checked_add_months(Months::new(1)).map_or(last, |n| n - Duration::days(1));
            (after_paid.map_or(first, |a| a.max(first)), month_end)
        }
    };
    if let Some(gap) = after_paid.filter(|a| *a < start) {
        return Err(Error::invalid(format!(
            "no salary slip pays {} for {gap} to {}; run payroll for it before the full and final statement",
            s(emp, "employee_name").unwrap_or_default(),
            start - Duration::days(1)
        )));
    }
    let mut slip = Doc::new();
    set(&mut slip, "employee", emp_id.to_string());
    set(&mut slip, "start_date", start.to_string());
    set(&mut slip, "end_date", end.to_string());
    set(&mut slip, "posting_date", st.get("transaction_date").cloned().unwrap_or_default());
    Ok(Some(hc.kernel.insert_tx(&user, hc.tx, "SalarySlip", slip).await?))
}

/// What leaving brings to the statement: the last pay, leave encashed, notice short and gratuity.
/// A day's pay is the last salary slip's full gross over its working days.
async fn leaving(hc: &mut HookCtx<'_>, st: &Doc) -> Result<(Vec<Value>, Vec<Value>)> {
    let (mut pay, mut recover) = (Vec::new(), Vec::new());
    let Some(sep) = lookup(hc, "EmployeeSeparation", id(st, "separation")).await? else { return Ok((pay, recover)) };
    let (Some(last), Some(sep_id)) = (opt_date(&sep, "relieving_date"), id(&sep, "id")) else { return Ok((pay, recover)) };
    let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", id(st, "employee").unwrap_or_default()).await?;
    let emp_id = id(&emp, "id").unwrap_or_default();
    let co = company(hc.kernel, hc.ctx, hc.tx, id(&emp, "company").unwrap_or_default()).await?;
    let p = co.precision;

    // ── the last pay ──
    if let Some(slip) = last_pay(hc, st, &emp, last).await? {
        let net = d(&slip, "net_pay");
        if net > Decimal::ZERO {
            let payable = company_account(hc, co.id, "pay_payroll_payable_account", "payroll payable account").await?;
            let from = s(&slip, "start_date").unwrap_or_default().to_string();
            let tax = if b(&slip, "final_settlement") { ", income tax settled for the year" } else { "" };
            pay.push(line(
                format!("Pay from {from} to {last}"),
                SLIP,
                id(&slip, "id").unwrap_or_default(),
                s(&slip, "number").map(str::to_string),
                payable,
                net,
                None,
                format!("Net pay of its own salary slip{tax}"),
            ));
        }
    }
    let basis: Option<Uuid> = sqlx::query_scalar(
        "select id from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus < 2 and start_date <= $3 and not coalesce(off_cycle, false) \
         order by end_date desc limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(emp_id)
    .bind(last)
    .fetch_optional(&mut **hc.tx)
    .await?;
    let Some(basis) = lookup(hc, "SalarySlip", basis).await? else { return Ok((pay, recover)) };
    let full: Vec<Doc> = rows(&basis, "earnings");
    let gross: Decimal = full.iter().filter(|r| !b(r, "statistical_component")).map(|r| d(r, "default_amount")).sum();
    let working = d(&basis, "total_working_days");
    let day_pay = if working.is_zero() { Decimal::ZERO } else { gross / working };
    let expense = company_account(hc, co.id, "pay_salary_expense_account", "default salary expense account").await?;

    // ── leave encashed ──
    let per_day = match lookup(hc, "SalaryStructure", id(&basis, "salary_structure")).await? {
        Some(st) if d(&st, "leave_encashment_amount_per_day") > Decimal::ZERO => d(&st, "leave_encashment_amount_per_day"),
        _ => day_pay,
    };
    let types: Vec<(Uuid, String)> = sqlx::query_as(
        "select id, leave_type_name from e_leave_type where tenant_id = $1 and allow_encashment and not coalesce(disabled, false) order by leave_type_name",
    )
    .bind(hc.ctx.tenant)
    .fetch_all(&mut **hc.tx)
    .await?;
    for (lt, name) in types {
        let Some(alloc) = allocation_on(hc.tx, hc.ctx.tenant, emp_id, lt, last).await? else { continue };
        let days = available(hc.tx, hc.ctx.tenant, emp_id, lt, alloc, None).await?;
        let amount = round(days * per_day, p);
        if days > Decimal::ZERO && amount > Decimal::ZERO {
            let rate = round(per_day, p).normalize();
            pay.push(line(
                format!("{name} encashed"),
                ENCASHMENT,
                lt,
                Some(name),
                expense,
                amount,
                Some(days),
                format!("{} days left on {last}, at {rate} a day", days.normalize()),
            ));
        }
    }

    // ── notice short ──
    let short = Decimal::from(sep.get("notice_shortfall_days").and_then(Value::as_i64).unwrap_or(0));
    let amount = round(short * day_pay, p);
    let remark = format!("{} days of notice short, at {} a day", short.normalize(), round(day_pay, p).normalize());
    match s(&sep, "shortfall_settlement") {
        _ if amount <= Decimal::ZERO => {}
        Some("Recover from Employee") => recover.push(line(
            "Notice not served".into(),
            "Notice Recovery",
            sep_id,
            s(&sep, "number").map(str::to_string),
            expense,
            amount,
            Some(short),
            remark,
        )),
        Some("Pay in Lieu of Notice") => {
            pay.push(line("Pay in lieu of notice".into(), "Notice Pay", sep_id, s(&sep, "number").map(str::to_string), expense, amount, Some(short), remark))
        }
        _ => {}
    }

    // ── gratuity ──
    if let Some(g) = gratuity(hc, &co, &sep, &emp, last, &full, gross).await? {
        pay.push(line("Gratuity".into(), "Gratuity", sep_id, s(&sep, "number").map(str::to_string), expense, round(g.0, p), Some(g.1), g.2));
    }
    Ok((pay, recover))
}

/// Gratuity by the company's rule: (amount, years counted, how). None when the company pays none,
/// the employee has not served the minimum (which a death waives), or nothing is due.
async fn gratuity(
    hc: &mut HookCtx<'_>,
    co: &Company,
    sep: &Doc,
    emp: &Doc,
    last: NaiveDate,
    earnings: &[Doc],
    gross: Decimal,
) -> Result<Option<(Decimal, Decimal, String)>> {
    let per_year = d(&co.doc, "hr_gratuity_days_per_year");
    let Some(joined) = opt_date(emp, "date_of_joining").filter(|_| per_year > Decimal::ZERO) else { return Ok(None) };
    let min = co.doc.get("hr_gratuity_min_years").and_then(Value::as_u64).unwrap_or(0) as u32;
    if s(sep, "reason") != Some("Death") && !served(joined, last, min) {
        return Ok(None);
    }
    let years = service_years(joined, last, s(&co.doc, "hr_gratuity_part_year").unwrap_or_default());
    let month = match s(&co.doc, "hr_gratuity_pay_formula") {
        Some(src) => {
            let abbrs: Vec<String> = sqlx::query_scalar("select abbr from e_salary_component where tenant_id = $1 and abbr is not null")
                .bind(hc.ctx.tenant)
                .fetch_all(&mut **hc.tx)
                .await?;
            let mut vars = Vars::default();
            for a in &abbrs {
                vars.set(a, Decimal::ZERO);
            }
            for r in earnings {
                if let Some(a) = s(r, "abbr") {
                    vars.set(a, d(r, "default_amount"));
                }
            }
            vars.set("gross", gross);
            Formula::compile(src).and_then(|f| f.amount(&vars)).map_err(|m| Error::invalid(format!("the company's gratuity pay formula: {m}")))?
        }
        None => gross,
    };
    let in_month = Some(d(&co.doc, "hr_gratuity_days_in_month")).filter(|v| *v > Decimal::ZERO).unwrap_or(Decimal::from(30));
    let amount = month * per_year / in_month * years;
    if amount <= Decimal::ZERO {
        return Ok(None);
    }
    let how = format!(
        "{} years from {joined} to {last}: {} days' pay a year, a day being a month's {} over {}",
        years.normalize(),
        per_year.normalize(),
        month.normalize(),
        in_month.normalize()
    );
    Ok(Some((amount, years, how)))
}

/// Fills a draft statement's lines with what is open now. Written with engine rights: what a line
/// refers to is the engine's to set, never a person's.
async fn gather(hc: &mut HookCtx<'_>, st: &Doc) -> Result<Doc> {
    let me = id(st, "id").ok_or_else(|| Error::internal("statement without id"))?;
    let (payables, receivables) = outstanding(hc, st).await?;
    let mut patch = Doc::new();
    patch.insert("payables".into(), Value::Array(payables));
    patch.insert("receivables".into(), Value::Array(receivables));
    hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, ENTITY, me, patch, None).await
}

struct StatementHook;

#[async_trait]
impl DocHook for StatementHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if docstatus(doc) == 0 {
            set(doc, "status", "Draft");
        }
        let precision = match id(doc, "company") {
            Some(cid) => {
                let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
                set(doc, "currency", co.currency_id.to_string());
                co.precision
            }
            None => 2,
        };
        let mut totals = [Decimal::ZERO; 2];
        for (i, table) in ["payables", "receivables"].into_iter().enumerate() {
            let mut lines = rows(doc, table);
            for r in lines.iter_mut() {
                let amount = round(d(r, "amount"), precision);
                r.insert("amount".into(), money(amount));
                totals[i] += amount;
            }
            doc.insert(table.into(), Value::Array(lines.into_iter().map(Value::Object).collect()));
        }
        set(doc, "total_payable", money(totals[0]));
        set(doc, "total_receivable", money(totals[1]));
        set(doc, "net_payable", money(totals[0] - totals[1]));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        match lookup(hc, "EmployeeSeparation", id(doc, "separation")).await? {
            Some(sep) if id(&sep, "employee") != id(doc, "employee") => errors.push(FieldError::new("separation", "is another employee's")),
            Some(sep) if docstatus(&sep) != 1 => errors.push(FieldError::new("separation", "is not submitted")),
            Some(_) => {
                let other: Option<String> = sqlx::query_scalar(
                    "select coalesce(number, 'a draft') from e_full_and_final_statement where tenant_id = $1 and separation = $2 and docstatus < 2 and id <> $3",
                )
                .bind(hc.ctx.tenant)
                .bind(id(doc, "separation"))
                .bind(id(doc, "id").unwrap_or_default())
                .fetch_optional(&mut **hc.tx)
                .await?;
                if let Some(n) = other {
                    errors.push(FieldError::new("separation", format!("already has its statement ({n})")));
                }
            }
            None => {}
        }
        let co = id(doc, "company");
        for table in ["payables", "receivables"] {
            for (i, r) in rows(doc, table).iter().enumerate() {
                if let Some(a) = lookup(hc, "Account", id(r, "account")).await?
                    && id(&a, "company") != co
                {
                    errors.push(FieldError::new("account", "belongs to another company").in_row(table, i));
                }
                let gathered = r.get("gathered_amount").filter(|g| !g.is_null()).map(|_| d(r, "gathered_amount"));
                let Some(gathered) = gathered.filter(|g| *g != d(r, "amount")) else { continue };
                let what = s(r, "component").unwrap_or_default();
                if s(r, "reference_type") == Some(RUN_SLIP) {
                    errors.push(
                        FieldError::new("amount", format!("{what} is what its salary slip still owes ({}); it is settled as it is", gathered.normalize()))
                            .in_row(table, i),
                    );
                } else if s(r, "reference_type") == Some(SLIP) {
                    errors.push(
                        FieldError::new(
                            "amount",
                            format!(
                                "{what} is its salary slip's net pay ({}); change the slip with an additional salary and get the outstanding amounts again",
                                gathered.normalize()
                            ),
                        )
                        .in_row(table, i),
                    );
                } else if s(r, "reason").is_none() {
                    errors.push(FieldError::new("reason", format!("say why {what} differs from the {} gathered", gathered.normalize())).in_row(table, i));
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        // What it names is still open for what it says.
        settle_lines(hc, doc, Decimal::ONE, false).await?;
        let last = lookup(hc, "EmployeeSeparation", id(doc, "separation")).await?.and_then(|sep| opt_date(&sep, "relieving_date"));
        let employee = id(doc, "employee").unwrap_or_default();
        // A run slip it settles still owes what the line says.
        for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
            for r in rows(doc, table).iter().filter(|r| s(r, "reference_type") == Some(RUN_SLIP)) {
                let slip = lookup(hc, "SalarySlip", id(r, "reference")).await?.filter(|sl| docstatus(sl) == 1);
                if slip.is_none_or(|sl| d(&sl, "net_pay") - d(&sl, "paid_amount") != side * d(r, "amount")) {
                    return Err(Error::invalid(format!(
                        "{} no longer owes what the statement says; get the outstanding amounts again",
                        s(r, "component").unwrap_or("a salary slip")
                    )));
                }
            }
        }
        for r in rows(doc, "payables") {
            match s(&r, "reference_type") {
                // The last pay's slip, computed again with what is known now, still pays what the line says.
                Some(SLIP) => {
                    let slip = lookup(hc, "SalarySlip", id(&r, "reference")).await?.filter(|sl| docstatus(sl) == 0);
                    let slip =
                        slip.ok_or_else(|| Error::invalid("the last pay's salary slip is gone or no longer a draft; get the outstanding amounts again"))?;
                    let mut v = Doc::new();
                    set(&mut v, "posting_date", doc.get("transaction_date").cloned().unwrap_or_default());
                    let slip = hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, "SalarySlip", id(&slip, "id").unwrap_or_default(), v, None).await?;
                    if d(&slip, "net_pay") != d(&r, "amount") {
                        return Err(Error::invalid(format!(
                            "the last pay's salary slip now pays {} where the statement says {}; get the outstanding amounts again",
                            d(&slip, "net_pay").normalize(),
                            d(&r, "amount").normalize()
                        )));
                    }
                }
                Some(ENCASHMENT) => {
                    let (Some(lt), Some(last)) = (id(&r, "reference"), last) else { continue };
                    let left = match allocation_on(hc.tx, hc.ctx.tenant, employee, lt, last).await? {
                        Some(a) => available(hc.tx, hc.ctx.tenant, employee, lt, a, None).await?,
                        None => Decimal::ZERO,
                    };
                    if d(&r, "quantity") > left {
                        return Err(Error::invalid(format!(
                            "only {} days of {} are left to encash on {last}; get the outstanding amounts again",
                            left.normalize(),
                            s(&r, "reference_number").unwrap_or_default()
                        )));
                    }
                }
                _ => {}
            }
        }
        set(doc, "status", "Unpaid");
        Ok(())
    }

    /// The last pay's slip is submitted (its accrual posted) and the leave encashed is used up.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("statement without id"))?;
        let last = lookup(hc, "EmployeeSeparation", id(doc, "separation")).await?.and_then(|sep| opt_date(&sep, "relieving_date")).unwrap_or_default();
        let employee = id(doc, "employee").unwrap_or_default();
        for r in rows(doc, "payables") {
            let Some(reference) = id(&r, "reference") else { continue };
            match s(&r, "reference_type") {
                Some(SLIP) => {
                    hc.kernel.submit_tx(&elevated(hc.ctx), hc.tx, "SalarySlip", reference, None).await?;
                }

                Some(ENCASHMENT) => {
                    let e = LedgerEntry {
                        employee,
                        leave_type: reference,
                        kind: "Leave Encashment".into(),
                        transaction: me,
                        number: s(doc, "number").map(str::to_string),
                        allocation: allocation_on(hc.tx, hc.ctx.tenant, employee, reference, last).await?.map(|a| a.0),
                        leaves: -d(&r, "quantity"),
                        from: last,
                        to: last,
                        carry_forward: false,
                        lwp: false,
                        reversal: false,
                    };
                    leave::post(hc, e).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if s(doc, "status") == Some("Paid") {
            return Err(Error::invalid("this statement has been paid; cancel its journal entry first"));
        }
        Ok(())
    }

    /// A draft statement takes its draft slip with it.
    async fn before_delete(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        for r in rows(doc, "payables").iter().filter(|r| s(r, "reference_type") == Some(SLIP)) {
            if let Some(slip) = lookup(hc, "SalarySlip", id(r, "reference")).await?.filter(|sl| docstatus(sl) == 0) {
                hc.kernel.delete_tx(&elevated(hc.ctx), hc.tx, "SalarySlip", id(&slip, "id").unwrap_or_default(), None).await?;
            }
        }
        Ok(())
    }

    /// Undoes the last pay's slip (its accrual reversed) and gives back the leave encashed.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").unwrap_or_default();
        for r in rows(doc, "payables").iter().filter(|r| s(r, "reference_type") == Some(SLIP)) {
            if let Some(slip) = lookup(hc, "SalarySlip", id(r, "reference")).await?.filter(|sl| docstatus(sl) == 1) {
                hc.kernel.cancel_tx(&elevated(hc.ctx), hc.tx, "SalarySlip", id(&slip, "id").unwrap_or_default(), None).await?;
            }
        }
        leave::reverse(hc, me).await?;
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, ENTITY, id(doc, "id").unwrap_or_default(), v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "get_outstanding" => Ok(Some(Value::Object(gather(hc, doc).await?))),
            "make_payment_entry" => {
                let mut lines = Vec::new();
                for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
                    for r in rows(doc, table) {
                        let account = id(&r, "account").ok_or_else(|| Error::invalid("a line has no account"))?;
                        lines.push((account, side * d(&r, "amount")));
                    }
                }
                let net = d(doc, "net_payable");
                if !net.is_zero() {
                    let bank = bank_param(hc, params, id(doc, "company").unwrap_or_default(), None).await?;
                    lines.push((bank, -net));
                }
                let remark = format!("Full and final settlement {} with {}", s(doc, "number").unwrap_or_default(), s(doc, "employee_name").unwrap_or_default());
                payment_entry(hc, doc, KIND, &lines, params, remark).await.map(Some)
            }
            _ => Ok(None),
        }
    }
}

/// A submitted separation's Create Full and Final Statement: its statement, made now with what is
/// open, or the one it has.
struct FromSeparation;

#[async_trait]
impl DocHook for FromSeparation {
    /// A separation its submitted statement settles stays: the employee was paid off on it.
    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let settled: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_full_and_final_statement where tenant_id = $1 and separation = $2 and docstatus = 1 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        match settled {
            Some(n) => Err(Error::invalid(format!("full and final statement {n} settles this separation; cancel it first"))),
            None => Ok(()),
        }
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "make_full_and_final" {
            return Ok(None);
        }
        let sep = id(doc, "id").ok_or_else(|| Error::internal("separation without id"))?;
        let existing: Option<Uuid> = sqlx::query_scalar("select id from e_full_and_final_statement where tenant_id = $1 and separation = $2 and docstatus < 2")
            .bind(hc.ctx.tenant)
            .bind(sep)
            .fetch_optional(&mut **hc.tx)
            .await?;
        if let Some(st) = existing {
            return Ok(Some(json!({"entity": ENTITY, "id": st.to_string(), "message": "This separation already has its statement"})));
        }
        hc.ctx.require(ENTITY, Action::Create)?;
        let mut st = Doc::new();
        set(&mut st, "employee", s(doc, "employee").unwrap_or_default());
        set(&mut st, "separation", sep.to_string());
        let made = hc.kernel.insert_tx(hc.ctx, hc.tx, ENTITY, st).await?;
        let made = gather(hc, &made).await?;
        let lines = rows(&made, "payables").len() + rows(&made, "receivables").len();
        Ok(Some(json!({
            "entity": ENTITY,
            "id": s(&made, "id").unwrap_or_default(),
            "message": match lines {
                0 => "The statement is made: nothing is open either way; add what is owed".to_string(),
                1 => "The statement is made with the one amount open".to_string(),
                n => format!("The statement is made with the {n} amounts open"),
            },
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_counts_part_years_by_the_rule() {
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let years = |joined: &str, last: &str, part: &str| service_years(day(joined), day(last), part);
        // Five years complete on the eve of the fifth anniversary, the last working day counted.
        assert!(served(day("2021-08-20"), day("2026-08-19"), 5));
        assert!(!served(day("2021-08-21"), day("2026-08-19"), 5));
        assert_eq!(years("2021-08-20", "2026-08-19", ""), Decimal::from(5));
        // More than six months counts as a year; six exactly does not.
        assert_eq!(years("2023-01-01", "2026-08-14", ""), Decimal::from(4));
        assert_eq!(years("2023-01-01", "2026-06-30", ""), Decimal::from(3));
        assert_eq!(years("2023-01-01", "2026-08-14", "Completed years only"), Decimal::from(3));
        assert_eq!(years("2023-01-01", "2026-07-01", "Pro rata"), Decimal::new(34986, 4), "182 of 365 days");
    }
}
