//! Payroll: salary components with formulas, salary structures and their assignment, payroll
//! periods with progressive income tax, additional salary and leave encashment. Salary slips are
//! in `slip`, payroll runs and their postings in `run`.

pub(crate) mod formula;
pub(crate) mod run;
pub(crate) mod slip;

use crate::leave::{self, LedgerEntry, allocation_on, available, sync_expiry};
use crate::{employee_of, lookup, opt_date, stamp_company};
use async_trait::async_trait;
use chrono::NaiveDate;
use ferrum_domain::util::{b, d, elevated, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Doc, DocHook, Error, FieldError, HookCtx, HookRegistry, Result};
use formula::{Formula, Vars, check_abbr};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

/// Whether this workspace posts payroll to the ledger: the `payroll_erp` connector with ERP. Without
/// it, payroll runs compute and submit salary slips and post nothing.
pub(crate) fn posts(hc: &HookCtx<'_>) -> bool {
    hc.ctx.schema.packages.iter().any(|p| p == crate::PAYROLL_ERP)
}

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("SalaryComponent", Arc::new(ComponentHook))
        .register("SalaryStructure", Arc::new(StructureHook))
        .register("SalaryStructureAssignment", Arc::new(AssignmentHook))
        .register("IncomeTaxSlab", Arc::new(TaxSlabHook))
        .register("EmployeeTaxExemptionDeclaration", Arc::new(DeclarationHook))
        .register("AdditionalSalary", Arc::new(AdditionalSalaryHook))
        .register("LeaveEncashment", Arc::new(EncashmentHook))
        .register("SalarySlip", Arc::new(slip::SlipHook))
        .register("PayrollEntry", Arc::new(run::RunHook));
}

/// The submitted salary structure assignment in force on `day`: the latest starting on or before it.
pub(crate) async fn assignment_on(hc: &mut HookCtx<'_>, employee: Uuid, day: NaiveDate) -> Result<Option<Doc>> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "select id from e_salary_structure_assignment where tenant_id = $1 and employee = $2 and docstatus = 1 and from_date <= $3 \
         order by from_date desc limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(day)
    .fetch_optional(&mut **hc.tx)
    .await?;
    lookup(hc, "SalaryStructureAssignment", found).await
}

/// The formula a structure row uses: its own, else the component's (when the row sets no amount).
pub(crate) fn row_formula<'a>(row: &'a Doc, comp: &'a Doc) -> Option<&'a str> {
    if let Some(f) = s(row, "formula") {
        return Some(f);
    }
    if d(row, "amount").is_zero() && b(comp, "amount_based_on_formula") { s(comp, "formula") } else { None }
}

/// The salary component flags copied onto structure and slip rows.
pub(crate) const FLAGS: &[&str] =
    &["depends_on_payment_days", "statistical_component", "is_tax_applicable", "exempted_from_income_tax", "variable_based_on_taxable_salary"];

struct ComponentHook;

#[async_trait]
impl DocHook for ComponentHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        if let Some(abbr) = s(doc, "abbr")
            && let Err(m) = check_abbr(abbr)
        {
            errors.push(FieldError::new("abbr", m));
        }
        if b(doc, "amount_based_on_formula")
            && let Some(src) = s(doc, "formula")
        {
            let others: Vec<String> = sqlx::query_scalar("select abbr from e_salary_component where tenant_id = $1 and id <> $2 and abbr is not null")
                .bind(hc.ctx.tenant)
                .bind(id(doc, "id"))
                .fetch_all(&mut **hc.tx)
                .await?;
            if let Err(m) = check_formula(src, &others, false) {
                errors.push(FieldError::new("formula", m));
            }
        }
        let mut seen = HashSet::new();
        for (i, r) in rows(doc, "accounts").iter().enumerate() {
            let (Some(co), Some(acc)) = (id(r, "company"), id(r, "account")) else { continue };
            if !seen.insert(co) {
                errors.push(FieldError::new("company", "has two accounts").in_row("accounts", i));
            }
            if let Some(a) = lookup(hc, "Account", Some(acc)).await?
                && id(&a, "company") != Some(co)
            {
                errors.push(FieldError::new("account", "belongs to another company").in_row("accounts", i));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }
}

/// Compiles a formula (or condition) and checks every name it uses is known; a trial run with
/// placeholder values catches type errors before payroll day.
pub(crate) fn check_formula(src: &str, known: &[String], condition: bool) -> std::result::Result<(), String> {
    let f = Formula::compile(src)?;
    let unknown = f.unknown(known);
    if !unknown.is_empty() {
        return Err(format!(
            "unknown name{} {} (use base, variable, gross, payment_days, working_days, lwp_days, absent_days or the abbreviation of a component above)",
            if unknown.len() > 1 { "s" } else { "" },
            unknown.join(", ")
        ));
    }
    let sample = Vars::sample(known);
    if condition { f.holds(&sample).map(|_| ()) } else { f.amount(&sample).map(|_| ()) }
}

struct StructureHook;

#[async_trait]
impl DocHook for StructureHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        for table in ["earnings", "deductions"] {
            let mut list = rows(doc, table);
            for r in list.iter_mut() {
                if let Some(comp) = lookup(hc, "SalaryComponent", id(r, "salary_component")).await? {
                    for f in FLAGS {
                        set(r, f, b(&comp, f));
                    }
                    set(r, "abbr", comp.get("abbr").cloned().unwrap_or_default());
                }
            }
            set(doc, table, Value::Array(list.into_iter().map(Value::Object).collect()));
        }
        Ok(())
    }

    /// Rows are computed earnings first, then deductions, each in order: a formula may only use
    /// the abbreviations of rows above it.
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        let mut known: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut tax_rows = 0;
        for (table, kind) in [("earnings", "Earning"), ("deductions", "Deduction")] {
            for (i, r) in rows(doc, table).iter().enumerate() {
                let Some(comp) = lookup(hc, "SalaryComponent", id(r, "salary_component")).await? else { continue };
                let name = s(&comp, "component_name").unwrap_or_default().to_string();
                let mut err = |f: &str, m: String| errors.push(FieldError::new(f, m).in_row(table, i));
                if s(&comp, "component_type") != Some(kind) {
                    err("salary_component", format!("{name} is not an {}", kind.to_lowercase()));
                }
                if b(&comp, "disabled") {
                    err("salary_component", format!("{name} is disabled"));
                }
                if !seen.insert(id(&comp, "id")) {
                    err("salary_component", format!("{name} appears twice"));
                }
                if b(&comp, "variable_based_on_taxable_salary") {
                    tax_rows += 1;
                } else {
                    if let Some(c) = s(r, "condition")
                        && let Err(m) = check_formula(c, &known, true)
                    {
                        err("condition", format!("{name}: {m}"));
                    }
                    if let Some(f) = row_formula(r, &comp)
                        && let Err(m) = check_formula(f, &known, false)
                    {
                        err("formula", format!("{name}: {m}"));
                    }
                }
                if let Some(a) = s(&comp, "abbr") {
                    known.push(a.to_string());
                }
            }
        }
        if tax_rows > 1 {
            errors.push(FieldError::new("deductions", "only one income tax component is allowed"));
        }
        crate::people::same_company(hc, doc, "payment_account", "Account").await?;
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let used: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_salary_structure_assignment where tenant_id = $1 and salary_structure = $2 and docstatus = 1 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        match used {
            Some(n) => Err(Error::invalid(format!("salary structure assignment {n} uses this structure; cancel the assignments or deactivate the structure"))),
            None => Ok(()),
        }
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if name != "set_active" {
            return Ok(None);
        }
        let active = params.get("active").and_then(Value::as_bool).unwrap_or(!b(doc, "is_active"));
        let mut v = Doc::new();
        set(&mut v, "is_active", active);
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "SalaryStructure", id(doc, "id").unwrap_or_default(), v).await?;
        Ok(Some(json!({"is_active": active})))
    }
}

struct AssignmentHook;

#[async_trait]
impl DocHook for AssignmentHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(from), Some(st)) = (opt_date(doc, "from_date"), id(doc, "salary_structure")) else { return Ok(()) };
        let emp = employee_of(hc, doc).await?;
        let structure = get(hc.kernel, hc.ctx, hc.tx, "SalaryStructure", st).await?;
        let name = s(&structure, "structure_name").unwrap_or_default().to_string();
        if d(&structure, "docstatus") != Decimal::ONE || !b(&structure, "is_active") {
            return Err(Error::field("salary_structure", format!("{name} is not a submitted, active salary structure")));
        }
        if id(&structure, "company") != id(doc, "company") {
            return Err(Error::field("salary_structure", format!("{name} belongs to another company")));
        }
        if opt_date(&emp, "date_of_joining").is_some_and(|j| from < j) {
            return Err(Error::field("from_date", "is before the employee's date of joining"));
        }
        let has_tax = rows(&structure, "deductions").iter().any(|r| b(r, "variable_based_on_taxable_salary"));
        if has_tax && id(doc, "income_tax_slab").is_none() {
            return Err(Error::field("income_tax_slab", format!("{name} deducts income tax; choose the employee's income tax slab")));
        }
        let dup: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_salary_structure_assignment where tenant_id = $1 and employee = $2 and from_date = $3 \
             and docstatus = 1 and id <> $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(from)
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = dup {
            return Err(Error::field("from_date", format!("assignment {n} already starts on {from} for this employee; cancel it first")));
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let paid: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_salary_slip where tenant_id = $1 and salary_structure_assignment = $2 and docstatus = 1 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        match paid {
            Some(n) => Err(Error::invalid(format!("salary slip {n} was computed from this assignment; start a new assignment instead"))),
            None => Ok(()),
        }
    }
}

struct TaxSlabHook;

#[async_trait]
impl DocHook for TaxSlabHook {
    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut slabs: Vec<(usize, Decimal, Decimal)> =
            rows(doc, "slabs").iter().enumerate().map(|(i, r)| (i, d(r, "from_amount"), d(r, "to_amount"))).collect();
        slabs.sort_by_key(|a| a.1);
        for w in slabs.windows(2) {
            let ((_, _, to), (i, from, _)) = (w[0], w[1]);
            if to.is_zero() || from < to {
                return Err(Error::Validation(vec![FieldError::new("from_amount", "overlaps the slab below it").in_row("slabs", i)]));
            }
        }
        Ok(())
    }
}

struct DeclarationHook;

#[async_trait]
impl DocHook for DeclarationHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        let mut list = rows(doc, "declarations");
        let (mut declared, mut exempt) = (Decimal::ZERO, Decimal::ZERO);
        for r in list.iter_mut() {
            let cap = match lookup(hc, "EmployeeTaxExemptionCategory", id(r, "exemption_category")).await? {
                Some(c) => d(&c, "max_amount"),
                None => Decimal::ZERO,
            };
            set(r, "max_amount", money(cap));
            let amount = d(r, "amount");
            declared += amount;
            exempt += if cap > Decimal::ZERO { amount.min(cap) } else { amount };
        }
        set(doc, "declarations", Value::Array(list.into_iter().map(Value::Object).collect()));
        set(doc, "total_declared_amount", money(declared));
        set(doc, "total_exemption_amount", money(exempt));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        crate::people::same_company(hc, doc, "payroll_period", "PayrollPeriod").await?;
        let dup: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_employee_tax_exemption_declaration where tenant_id = $1 and employee = $2 \
             and payroll_period = $3 and docstatus < 2 and id <> $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(id(doc, "payroll_period"))
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = dup {
            return Err(Error::field("payroll_period", format!("declaration {n} already covers this payroll period")));
        }
        Ok(())
    }
}

/// A submitted salary slip of the employee covering `day`, if any.
async fn submitted_slip_on(hc: &mut HookCtx<'_>, employee: Option<Uuid>, day: Option<NaiveDate>) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "select coalesce(number, id::text) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus = 1 \
         and start_date <= $3 and end_date >= $3 limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(day)
    .fetch_optional(&mut **hc.tx)
    .await?)
}

struct AdditionalSalaryHook;

#[async_trait]
impl DocHook for AdditionalSalaryHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(day) = opt_date(doc, "payroll_date") else { return Ok(()) };
        let emp = employee_of(hc, doc).await?;
        if opt_date(&emp, "date_of_joining").is_some_and(|j| day < j) || opt_date(&emp, "relieving_date").is_some_and(|r| day > r) {
            return Err(Error::field("payroll_date", "the employee is not employed on this date"));
        }
        Ok(())
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(n) = submitted_slip_on(hc, id(doc, "employee"), opt_date(doc, "payroll_date")).await? {
            return Err(Error::field("payroll_date", format!("salary slip {n} for this date is already submitted; use a date in the next period")));
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(n) = submitted_slip_on(hc, id(doc, "employee"), opt_date(doc, "payroll_date")).await? {
            return Err(Error::invalid(format!("salary slip {n} already paid this; cancel the slip first")));
        }
        Ok(())
    }
}

struct EncashmentHook;

#[async_trait]
impl DocHook for EncashmentHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let Some(emp) = stamp_company(hc, doc).await? else { return Ok(()) };
        let (Some(emp_id), Some(lt), Some(day)) = (id(&emp, "id"), id(doc, "leave_type"), opt_date(doc, "encashment_date")) else { return Ok(()) };
        let alloc = allocation_on(hc.tx, hc.ctx.tenant, emp_id, lt, day).await?;
        set(doc, "leave_allocation", alloc.map(|a| json!(a.0.to_string())).unwrap_or_default());
        let balance = match alloc {
            Some(a) => available(hc.tx, hc.ctx.tenant, emp_id, lt, a, id(doc, "id")).await?,
            None => Decimal::ZERO,
        };
        set(doc, "leave_balance", money(balance));
        if doc.get("encashable_days").is_none_or(Value::is_null) {
            let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
            let max = (balance - d(&ltd, "encashment_threshold_days")).max(Decimal::ZERO);
            set(doc, "encashable_days", money(max));
        }
        if doc.get("per_day_amount").is_none_or(Value::is_null)
            && let Some(ssa) = assignment_on(hc, emp_id, day).await?
            && let Some(st) = lookup(hc, "SalaryStructure", id(&ssa, "salary_structure")).await?
        {
            set(doc, "per_day_amount", money(d(&st, "leave_encashment_amount_per_day")));
        }
        set(doc, "encashment_amount", money(round(d(doc, "encashable_days") * d(doc, "per_day_amount"), 2)));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(lt) = id(doc, "leave_type") else { return Ok(()) };
        let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
        let days = d(doc, "encashable_days");
        if days.is_zero() {
            return Ok(());
        }
        if id(doc, "leave_allocation").is_none() {
            return Err(Error::field("leave_type", "the employee has no allocation of this leave type on the encashment date"));
        }
        let max = (d(doc, "leave_balance") - d(&ltd, "encashment_threshold_days")).max(Decimal::ZERO);
        if days > max {
            return Err(Error::field("encashable_days", format!("at most {} days can be encashed", max.normalize())));
        }
        if d(doc, "per_day_amount") <= Decimal::ZERO {
            return Err(Error::field("per_day_amount", "set the amount per day (or the salary structure's leave encashment amount per day)"));
        }
        Ok(())
    }

    /// Uses up the leaves and pays them through an additional salary in the next slip.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("encashment without id"))?;
        let day = opt_date(doc, "encashment_date").unwrap_or_default();
        let e = LedgerEntry {
            employee: id(doc, "employee").unwrap_or_default(),
            leave_type: id(doc, "leave_type").unwrap_or_default(),
            kind: "Leave Encashment".into(),
            transaction: me,
            number: s(doc, "number").map(str::to_string),
            allocation: id(doc, "leave_allocation"),
            leaves: -d(doc, "encashable_days"),
            from: day,
            to: day,
            carry_forward: false,
            lwp: false,
            reversal: false,
        };
        leave::post(hc, e).await?;
        let mut a = Doc::new();
        set(&mut a, "employee", doc.get("employee").cloned().unwrap_or_default());
        set(&mut a, "salary_component", doc.get("salary_component").cloned().unwrap_or_default());
        set(&mut a, "amount", doc.get("encashment_amount").cloned().unwrap_or_default());
        set(&mut a, "payroll_date", day.to_string());
        set(&mut a, "reason", format!("Leave encashment {}", s(doc, "number").unwrap_or_default()));
        set(&mut a, "ref_doctype", "Leave Encashment");
        set(&mut a, "ref_docname", s(doc, "number").unwrap_or_default());
        let user = elevated(hc.ctx);
        let made = hc.kernel.insert_tx(&user, hc.tx, "AdditionalSalary", a).await?;
        let made = hc.kernel.submit_tx(&user, hc.tx, "AdditionalSalary", id(&made, "id").unwrap_or_default(), None).await?;
        let mut v = Doc::new();
        v.insert("additional_salary".into(), made.get("id").cloned().unwrap_or_default());
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "LeaveEncashment", me, v).await
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("encashment without id"))?;
        if let Some(extra) = lookup(hc, "AdditionalSalary", id(doc, "additional_salary")).await?
            && d(&extra, "docstatus") == Decimal::ONE
        {
            hc.kernel.cancel_tx(&elevated(hc.ctx), hc.tx, "AdditionalSalary", id(&extra, "id").unwrap_or_default(), None).await?;
        }
        leave::reverse(hc, me).await?;
        if let Some(a) = id(doc, "leave_allocation") {
            sync_expiry(hc, a, me).await?;
        }
        Ok(())
    }
}
