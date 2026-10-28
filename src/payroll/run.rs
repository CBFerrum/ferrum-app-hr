//! Payroll runs: pick the employees, create and submit their salary slips, post one balanced
//! accrual journal for the whole run (earnings debit their expense accounts, deductions credit
//! their liability accounts, each slip's net pay credits payroll payable) and pay it through a
//! bank journal entry. Cancelling reverses the journal and then cancels the slips; it is refused
//! while a payment stands.

use crate::expense::{amount_param, bank_param, payment_entry};
use crate::people::company_account;
use crate::{lookup, opt_date, today};
use async_trait::async_trait;
use chrono::NaiveDate;
use ferrum_domain::ledger::{self, Entry, Line};
use ferrum_domain::util::{b, company, d, elevated, find, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, Filter, HookCtx, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(crate) struct RunHook;

/// Cost center for an employee's salary: their own, their department's, else the given default.
async fn cost_center(hc: &mut HookCtx<'_>, emp: &Doc, default: Option<Uuid>) -> Result<Option<Uuid>> {
    if let Some(cc) = id(emp, "payroll_cost_center") {
        return Ok(Some(cc));
    }
    let dept = lookup(hc, "Department", id(emp, "department")).await?;
    Ok(dept.and_then(|d| id(&d, "payroll_cost_center")).or(default))
}

/// A component's account for the company: its own, else (earnings only) the company's default
/// salary expense account. Deductions are liabilities to someone specific and need their own.
async fn component_account(hc: &mut HookCtx<'_>, cache: &mut HashMap<Uuid, Uuid>, component: Uuid, company: Uuid) -> Result<Uuid> {
    if let Some(a) = cache.get(&component) {
        return Ok(*a);
    }
    let comp = get(hc.kernel, hc.ctx, hc.tx, "SalaryComponent", component).await?;
    let own = rows(&comp, "accounts").iter().find(|r| id(r, "company") == Some(company)).and_then(|r| id(r, "account"));
    let account = match own {
        Some(a) => a,
        None if s(&comp, "component_type") == Some("Earning") => {
            company_account(hc, company, "pay_salary_expense_account", "default salary expense account").await?
        }
        None => {
            return Err(Error::invalid(format!(
                "set the account of salary component {} for this company (Salary Component → Accounts)",
                s(&comp, "component_name").unwrap_or_default()
            )));
        }
    };
    cache.insert(component, account);
    Ok(account)
}

/// Posts the salary accrual of `slips` as one balanced journal entry of `source`.
pub(crate) async fn post_accrual(
    hc: &mut HookCtx<'_>,
    company_id: Uuid,
    source: (&str, Uuid, Option<String>),
    on: NaiveDate,
    slips: &[Doc],
    payable: Uuid,
    default_cc: Option<Uuid>,
) -> Result<()> {
    let co = company(hc.kernel, hc.ctx, hc.tx, company_id).await?;
    let default_cc = default_cc.or(co.account("default_cost_center"));
    let mut entry = Entry::new(company_id, on, source.0, source.1, source.2.clone());
    entry.memo = Some(format!("Salary accrual of {} employee{}", slips.len(), if slips.len() == 1 { "" } else { "s" }));
    let mut accounts = HashMap::new();
    for slip in slips {
        let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", id(slip, "employee").unwrap_or_default()).await?;
        let name = s(&emp, "employee_name").unwrap_or_default().to_string();
        let cc = cost_center(hc, &emp, default_cc).await?;
        for (table, sign) in [("earnings", Decimal::ONE), ("deductions", -Decimal::ONE)] {
            for r in rows(slip, table) {
                if b(&r, "statistical_component") || d(&r, "amount").is_zero() {
                    continue;
                }
                let account = component_account(hc, &mut accounts, id(&r, "salary_component").unwrap_or_default(), company_id).await?;
                entry.lines.push(Line::new(account, sign * d(&r, "amount"), &co.currency_code).cost_center(cc));
            }
        }
        let net = d(slip, "net_pay");
        if !net.is_zero() {
            let slip_id = id(slip, "id").unwrap_or_default();
            entry.lines.push(Line::new(payable, -net, &co.currency_code).against("SalarySlip", slip_id).remarks(format!("Net pay of {name}")));
        }
    }
    ledger::post(hc.tx, hc.ctx, entry, co.precision).await?;
    Ok(())
}

/// Slips of a payroll entry in a given state, with their rows.
async fn slips_of(hc: &mut HookCtx<'_>, run: Uuid, docstatus: i64) -> Result<Vec<Doc>> {
    let found =
        find(hc.kernel, hc.ctx, hc.tx, "SalarySlip", vec![Filter::eq("payroll_entry", run.to_string()), Filter::eq("docstatus", docstatus)], None, 100_000)
            .await?;
    let mut out = Vec::with_capacity(found.len());
    for f in found {
        out.push(get(hc.kernel, hc.ctx, hc.tx, "SalarySlip", id(&f, "id").unwrap_or_default()).await?);
    }
    Ok(out)
}

/// Copies the slips' totals onto the run and each employee row.
async fn refresh_totals(hc: &mut HookCtx<'_>, run: &Doc, docstatus: i64, extra: Doc) -> Result<Doc> {
    let me = id(run, "id").unwrap_or_default();
    let sctx = sys(hc.ctx);
    let slips: Vec<(Uuid, Uuid, Decimal, Decimal, Decimal)> = sqlx::query_as(
        "select id, employee, coalesce(gross_pay, 0), coalesce(total_deduction, 0), coalesce(net_pay, 0) from e_salary_slip \
         where tenant_id = $1 and payroll_entry = $2 and docstatus = $3",
    )
    .bind(hc.ctx.tenant)
    .bind(me)
    .bind(docstatus as i16)
    .fetch_all(&mut **hc.tx)
    .await?;
    let by_employee: HashMap<Uuid, (Uuid, Decimal)> = slips.iter().map(|s| (s.1, (s.0, s.4))).collect();
    for row in rows(run, "employees") {
        let (Some(row_id), Some(emp)) = (id(&row, "id"), id(&row, "employee")) else { continue };
        let (slip, net) = by_employee.get(&emp).map_or((Value::Null, Value::Null), |(sl, n)| (json!(sl.to_string()), money(*n)));
        let mut v = Doc::new();
        v.insert("salary_slip".into(), slip);
        v.insert("net_pay".into(), net);
        hc.kernel.set_child_values_tx(&sctx, hc.tx, "PayrollEntry", "employees", row_id, v).await?;
    }
    let mut v = extra;
    v.insert("total_gross_pay".into(), money(slips.iter().map(|s| s.2).sum()));
    v.insert("total_deductions".into(), money(slips.iter().map(|s| s.3).sum()));
    v.insert("total_net_pay".into(), money(slips.iter().map(|s| s.4).sum()));
    hc.kernel.set_values_tx(&sctx, hc.tx, "PayrollEntry", me, v).await?;
    hc.kernel.get_tx(&sctx, hc.tx, "PayrollEntry", me, false).await
}

/// Puts the employee's name in front of what went wrong with their slip.
fn for_employee(e: Error, name: &str) -> Error {
    match e {
        Error::Validation(fields) => {
            Error::Invalid(format!("{name}: {}", fields.iter().map(|f| format!("{} {}", f.field.replace('_', " "), f.message)).collect::<Vec<_>>().join("; ")))
        }
        Error::Invalid(m) => Error::Invalid(format!("{name}: {m}")),
        other => other,
    }
}

impl RunHook {
    async fn get_employees(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
        let me = id(doc, "id").unwrap_or_default();
        let (Some(start), Some(end)) = (opt_date(doc, "start_date"), opt_date(doc, "end_date")) else { return Err(Error::invalid("set the period first")) };
        let found: Vec<Uuid> = sqlx::query_scalar(
            "select e.id from e_employee e where e.tenant_id = $1 and e.company = $2 and e.date_of_joining <= $4 \
             and (e.relieving_date is null or e.relieving_date >= $3) and e.status in ('Active', 'Left') \
             and ($5::uuid is null or e.department = $5) and ($6::uuid is null or e.branch = $6) and ($7::uuid is null or e.designation = $7) \
             and (select st.payroll_frequency from e_salary_structure_assignment a \
                  join e_salary_structure st on st.tenant_id = a.tenant_id and st.id = a.salary_structure \
                  where a.tenant_id = e.tenant_id and a.employee = e.id and a.docstatus = 1 and a.from_date <= $4 \
                  order by a.from_date desc limit 1) = $8 \
             and not exists (select 1 from e_salary_slip ss where ss.tenant_id = e.tenant_id and ss.employee = e.id and ss.docstatus < 2 \
                  and ss.start_date <= $4 and ss.end_date >= $3 and ss.payroll_entry is distinct from $9) \
             order by e.employee_name, e.id",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "company"))
        .bind(start)
        .bind(end)
        .bind(id(doc, "department"))
        .bind(id(doc, "branch"))
        .bind(id(doc, "designation"))
        .bind(s(doc, "payroll_frequency").unwrap_or("Monthly"))
        .bind(me)
        .fetch_all(&mut **hc.tx)
        .await?;
        let list: Vec<Value> = found.iter().map(|e| json!({"employee": e.to_string()})).collect();
        let mut patch = Doc::new();
        patch.insert("employees".into(), Value::Array(list));
        Ok(Value::Object(hc.kernel.update_tx(hc.ctx, hc.tx, "PayrollEntry", me, patch, None).await?))
    }

    /// Draft slips for every employee of the run that has none yet (running it again is harmless).
    async fn create_slips(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
        hc.ctx.require("SalarySlip", Action::Create)?;
        let me = id(doc, "id").unwrap_or_default();
        let user = elevated(hc.ctx);
        for row in rows(doc, "employees") {
            let Some(emp) = id(&row, "employee") else { continue };
            let exists: bool = sqlx::query_scalar(
                "select exists (select 1 from e_salary_slip where tenant_id = $1 and payroll_entry = $2 and employee = $3 and docstatus < 2)",
            )
            .bind(hc.ctx.tenant)
            .bind(me)
            .bind(emp)
            .fetch_one(&mut **hc.tx)
            .await?;
            if exists {
                continue;
            }
            let mut slip = Doc::new();
            set(&mut slip, "employee", emp.to_string());
            set(&mut slip, "start_date", doc.get("start_date").cloned().unwrap_or_default());
            set(&mut slip, "end_date", doc.get("end_date").cloned().unwrap_or_default());
            set(&mut slip, "posting_date", doc.get("posting_date").cloned().unwrap_or_default());
            set(&mut slip, "payroll_entry", me.to_string());
            let name = s(&row, "employee_name").unwrap_or_default().to_string();
            hc.kernel.insert_tx(&user, hc.tx, "SalarySlip", slip).await.map_err(|e| for_employee(e, &name))?;
        }
        let mut flags = Doc::new();
        flags.insert("salary_slips_created".into(), json!(true));
        flags.insert("status".into(), json!("Slips Created"));
        Ok(Value::Object(refresh_totals(hc, doc, 0, flags).await?))
    }

    /// Submits the run's slips and posts one accrual journal for all of them.
    async fn submit_slips(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
        hc.ctx.require("SalarySlip", Action::Submit)?;
        let me = id(doc, "id").unwrap_or_default();
        let user = elevated(hc.ctx);
        for slip in slips_of(hc, me, 0).await? {
            let name = s(&slip, "employee_name").unwrap_or_default().to_string();
            hc.kernel.submit_tx(&user, hc.tx, "SalarySlip", id(&slip, "id").unwrap_or_default(), None).await.map_err(|e| for_employee(e, &name))?;
        }
        let slips = slips_of(hc, me, 1).await?;
        if slips.is_empty() {
            return Err(Error::invalid("there are no salary slips to submit; create them first"));
        }
        let cid = id(doc, "company").unwrap_or_default();
        let payable = id(doc, "payroll_payable_account").ok_or_else(|| Error::field("payroll_payable_account", "is required"))?;
        let on = opt_date(doc, "posting_date").unwrap_or_else(today);
        post_accrual(hc, cid, ("PayrollEntry", me, s(doc, "number").map(str::to_string)), on, &slips, payable, id(doc, "cost_center")).await?;
        let mut flags = Doc::new();
        flags.insert("salary_slips_submitted".into(), json!(true));
        flags.insert("status".into(), json!("Slips Submitted"));
        Ok(Value::Object(refresh_totals(hc, doc, 1, flags).await?))
    }
}

#[async_trait]
impl DocHook for RunHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0 {
            set(doc, "status", "Draft");
        }
        let Some(cid) = id(doc, "company") else { return Ok(()) };
        if id(doc, "payroll_payable_account").is_none() {
            let acc = company_account(hc, cid, "pay_payroll_payable_account", "payroll payable account").await?;
            set(doc, "payroll_payable_account", acc.to_string());
        }
        let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
        for (field, default) in [("cost_center", "default_cost_center"), ("payment_account", "default_bank_account")] {
            if id(doc, field).is_none() {
                set(doc, field, co.account(default).map(|a| json!(a.to_string())).unwrap_or_default());
            }
        }
        let mut list = rows(doc, "employees");
        for r in list.iter_mut() {
            if let Some(emp) = lookup(hc, "Employee", id(r, "employee")).await? {
                set(r, "department", emp.get("department").cloned().unwrap_or_default());
                set(r, "designation", emp.get("designation").cloned().unwrap_or_default());
            }
        }
        set(doc, "number_of_employees", list.len());
        set(doc, "employees", Value::Array(list.into_iter().map(Value::Object).collect()));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let dup: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_payroll_entry where tenant_id = $1 and company = $2 and start_date = $3 and end_date = $4 \
             and payroll_frequency = $5 and department is not distinct from $6 and branch is not distinct from $7 \
             and designation is not distinct from $8 and docstatus < 2 and id <> $9 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "company"))
        .bind(opt_date(doc, "start_date"))
        .bind(opt_date(doc, "end_date"))
        .bind(s(doc, "payroll_frequency"))
        .bind(id(doc, "department"))
        .bind(id(doc, "branch"))
        .bind(id(doc, "designation"))
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = dup {
            return Err(Error::field("start_date", format!("payroll entry {n} already covers this period for the same employees")));
        }
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        for (i, r) in rows(doc, "employees").iter().enumerate() {
            let Some(emp) = lookup(hc, "Employee", id(r, "employee")).await? else { continue };
            if !seen.insert(id(&emp, "id")) {
                errors.push(FieldError::new("employee", "is listed twice").in_row("employees", i));
            }
            if id(&emp, "company") != id(doc, "company") {
                errors.push(FieldError::new("employee", "belongs to another company").in_row("employees", i));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut v = Doc::new();
        set(&mut v, "status", "Submitted");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "PayrollEntry", id(doc, "id").unwrap_or_default(), v).await
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if d(doc, "paid_amount") > Decimal::ZERO {
            return Err(Error::invalid("salaries of this payroll entry have been paid; cancel the bank entry first"));
        }
        Ok(())
    }

    /// Reverses the accrual, then cancels the submitted slips and removes the drafts.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").unwrap_or_default();
        let co = company(hc.kernel, hc.ctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
        ledger::reverse(hc.tx, hc.ctx, "PayrollEntry", me, today(), co.precision).await?;
        let sctx = sys(hc.ctx);
        for (status, cancel) in [(1, true), (0, false)] {
            let found =
                find(hc.kernel, hc.ctx, hc.tx, "SalarySlip", vec![Filter::eq("payroll_entry", me.to_string()), Filter::eq("docstatus", status)], None, 100_000)
                    .await?;
            for slip in found {
                let sid = id(&slip, "id").unwrap_or_default();
                if cancel {
                    hc.kernel.cancel_tx(&sctx, hc.tx, "SalarySlip", sid, None).await?;
                } else {
                    hc.kernel.delete_tx(&sctx, hc.tx, "SalarySlip", sid, None).await?;
                }
            }
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sctx, hc.tx, "PayrollEntry", me, v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "get_employees" => self.get_employees(hc, doc).await.map(Some),
            "create_salary_slips" => self.create_slips(hc, doc).await.map(Some),
            "submit_salary_slips" => self.submit_slips(hc, doc).await.map(Some),
            "make_bank_entry" => {
                let amount = amount_param(params, d(doc, "total_net_pay") - d(doc, "paid_amount"))?;
                let payable = id(doc, "payroll_payable_account").ok_or_else(|| Error::field("payroll_payable_account", "is required"))?;
                let bank = bank_param(hc, params, id(doc, "company").unwrap_or_default(), id(doc, "payment_account")).await?;
                let remark = format!("Salaries of {} to {}", s(doc, "start_date").unwrap_or_default(), s(doc, "end_date").unwrap_or_default());
                payment_entry(hc, doc, "Payroll Entry", &[(payable, amount), (bank, -amount)], params, remark).await.map(Some)
            }
            _ => Ok(None),
        }
    }
}
