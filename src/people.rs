//! Organization and employees: departments, the employee master and its sign-in, HR accounts per
//! company, self-service rules, and the lifecycle documents (onboarding, separation, promotion,
//! transfer).

use crate::{employee_of, lookup, opt_date, stamp_company, today};
use async_trait::async_trait;
use ferrum_domain::util::{b, elevated, find, get, id, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, Filter, HookCtx, HookRegistry, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// Entities whose records are about one employee and owned by them (`owner_fields: [employee_user]`).
pub(crate) const OWNED: &[&str] = &[
    "EmployeeCheckin",
    "Attendance",
    "AttendanceRequest",
    "ShiftAssignment",
    "LeaveAllocation",
    "LeaveApplication",
    "LeaveLedgerEntry",
    "ExpenseClaim",
    "EmployeeAdvance",
    "Appraisal",
    "TrainingRecord",
    "EmployeePromotion",
    "EmployeeTransfer",
    "EmployeeSeparation",
    "SalarySlip",
    "LeaveEncashment",
    "EmployeeTaxExemptionDeclaration",
];

pub(crate) fn register(h: &mut HookRegistry) {
    // The company's HR and payroll accounts come with the connectors to ERP.
    h.scoped(&[crate::HR_ERP.to_string(), crate::PAYROLL_ERP.to_string()], |h| {
        h.register("Company", Arc::new(CompanyHook));
    });
    h.register("Department", Arc::new(DepartmentHook))
        .register("Employee", Arc::new(EmployeeHook))
        .register("EmployeeOnboarding", Arc::new(ChecklistHook))
        .register("EmployeeSeparation", Arc::new(ChecklistHook))
        .register("EmployeePromotion", Arc::new(MoveHook(&PROMOTION)))
        .register("EmployeeTransfer", Arc::new(MoveHook(&TRANSFER)));
}

pub(crate) fn register_self_service(h: &mut HookRegistry) {
    for e in OWNED {
        h.register(e, Arc::new(SelfService));
    }
}

/// Users who may only work on their own records (the Employee role) can only create or change
/// documents about themselves; the kernel alone would let them file one for a colleague.
struct SelfService;

#[async_trait]
impl DocHook for SelfService {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let entity = hc.entity.def.name.clone();
        if crate::full_access(hc.ctx, &entity, &[Action::Create, Action::Write]) {
            return Ok(());
        }
        let emp = lookup(hc, "Employee", id(doc, "employee")).await?;
        if emp.as_ref().and_then(|e| s(e, "user")).is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email)) {
            return Ok(());
        }
        Err(Error::Forbidden(format!("you can only create or change your own {}", hc.entity.def.label.to_lowercase())))
    }
}

// ───────────────────────────── accounts ─────────────────────────────

/// HR and payroll accounts per company: (company field, account name, number, parent group, root type).
const ACCOUNTS: &[(&str, &str, &str, &str, &str)] = &[
    ("hr_employee_payable_account", "Employee Payables", "2150", "Current Liabilities", "Liability"),
    ("hr_employee_advance_account", "Employee Advances", "1170", "Current Assets", "Asset"),
    ("pay_payroll_payable_account", "Payroll Payable", "2160", "Current Liabilities", "Liability"),
    ("pay_salary_expense_account", "Salaries", "5220", "Indirect Expenses", "Expense"),
];

/// Creates (or finds by name) the company's HR and payroll accounts and links the empty company
/// fields to them. Idempotent; returns how many links were set. The accounts carry no account
/// type: the ledger has no employee party, and journal lines on Payable accounts need one.
pub(crate) async fn ensure_accounts(hc: &mut HookCtx<'_>, company: Uuid) -> Result<usize> {
    let sctx = sys(hc.ctx);
    let co = get(hc.kernel, hc.ctx, hc.tx, "Company", company).await?;
    let Some(cent) = hc.ctx.schema.entity("Company").cloned() else { return Ok(0) };
    let mut linked = Doc::new();
    for (field, name, number, parent, root) in ACCOUNTS {
        if cent.field(field).is_none() || id(&co, field).is_some() {
            continue;
        }
        let by_name = |f: &str, v: &str| vec![Filter::eq("company", company.to_string()), Filter::eq(f, v)];
        let existing = find(hc.kernel, hc.ctx, hc.tx, "Account", by_name("account_name", name), None, 1).await?;
        let account = match existing.first().and_then(|a| id(a, "id")) {
            Some(a) => a,
            None => {
                let group = find(hc.kernel, hc.ctx, hc.tx, "Account", by_name("account_name", parent), None, 1).await?;
                let taken = !find(hc.kernel, hc.ctx, hc.tx, "Account", by_name("account_number", number), None, 1).await?.is_empty();
                let mut a = Doc::new();
                set(&mut a, "account_name", *name);
                set(&mut a, "company", company.to_string());
                set(&mut a, "root_type", *root);
                set(&mut a, "account_currency", co.get("default_currency").cloned().unwrap_or_default());
                if !taken {
                    set(&mut a, "account_number", *number);
                }
                if let Some(g) = group.first().filter(|g| b(g, "is_group") && s(g, "root_type") == Some(*root)) {
                    set(&mut a, "parent_account", g.get("id").cloned().unwrap_or_default());
                }
                let created = hc.kernel.insert_tx(&sctx, hc.tx, "Account", a).await?;
                id(&created, "id").ok_or_else(|| Error::internal("account without id"))?
            }
        };
        linked.insert(field.to_string(), json!(account.to_string()));
    }
    let n = linked.len();
    if n > 0 {
        hc.kernel.set_values_tx(&sctx, hc.tx, "Company", company, linked).await?;
    }
    Ok(n)
}

/// A company account by field, creating the standard HR accounts when it is not set yet.
pub(crate) async fn company_account(hc: &mut HookCtx<'_>, company: Uuid, field: &str, what: &str) -> Result<Uuid> {
    let co = get(hc.kernel, hc.ctx, hc.tx, "Company", company).await?;
    if let Some(a) = id(&co, field) {
        return Ok(a);
    }
    ensure_accounts(hc, company).await?;
    let co = get(hc.kernel, hc.ctx, hc.tx, "Company", company).await?;
    id(&co, field).ok_or_else(|| Error::invalid(format!("set the company's {what} (Company → {field})")))
}

struct CompanyHook;

#[async_trait]
impl DocHook for CompanyHook {
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        // The chart of accounts exists by now: the domain's company hook runs first.
        if old.is_none() && hc.ctx.schema.entity("Employee").is_some() {
            let company = id(doc, "id").ok_or_else(|| Error::internal("company without id"))?;
            ensure_accounts(hc, company).await?;
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "setup_hr_accounts" {
            return Ok(None);
        }
        let company = id(doc, "id").ok_or_else(|| Error::internal("company without id"))?;
        let linked = ensure_accounts(hc, company).await?;
        Ok(Some(json!({"linked": linked, "message": if linked > 0 { "HR and payroll accounts set up" } else { "HR and payroll accounts were already set" }})))
    }
}

// ───────────────────────────── organization ─────────────────────────────

/// A linked record must belong to the document's company.
pub(crate) async fn same_company(hc: &mut HookCtx<'_>, doc: &Doc, field: &str, entity: &str) -> Result<()> {
    let (Some(company), Some(linked)) = (id(doc, "company"), lookup(hc, entity, id(doc, field)).await?) else { return Ok(()) };
    if id(&linked, "company").is_some_and(|c| c != company) {
        return Err(Error::field(field, "belongs to another company"));
    }
    Ok(())
}

struct DepartmentHook;

#[async_trait]
impl DocHook for DepartmentHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "parent_department", "Department").await?;
        same_company(hc, doc, "payroll_cost_center", "CostCenter").await
    }
}

// ───────────────────────────── employee ─────────────────────────────

pub(crate) fn person_name(parts: &[Option<&str>]) -> String {
    parts.iter().flatten().map(|p| p.trim()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

struct EmployeeHook;

#[async_trait]
impl DocHook for EmployeeHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let name = person_name(&[s(doc, "first_name"), s(doc, "middle_name"), s(doc, "last_name")]);
        set(doc, "employee_name", name);
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(email) = s(doc, "user") {
            let exists: bool = sqlx::query_scalar("select exists(select 1 from app_user where tenant_id = $1 and email = $2::citext)")
                .bind(hc.ctx.tenant)
                .bind(email)
                .fetch_one(&mut **hc.tx)
                .await?;
            if !exists {
                return Err(Error::field("user", "no user signs in with this email; create the user first"));
            }
            let me = id(doc, "id").map(|i| i.to_string()).unwrap_or_default();
            let taken = find(hc.kernel, hc.ctx, hc.tx, "Employee", vec![Filter::eq("user", email)], None, 2).await?;
            if let Some(other) = taken.iter().find(|e| s(e, "id") != Some(me.as_str())) {
                return Err(Error::field("user", format!("this user is already linked to {}", s(other, "employee_name").unwrap_or_default())));
            }
        }
        same_company(hc, doc, "department", "Department").await?;
        same_company(hc, doc, "payroll_cost_center", "CostCenter").await
    }

    /// A new sign-in moves every record about the employee to it.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        let Some(old) = old else { return Ok(()) };
        if s(old, "user") == s(doc, "user") {
            return Ok(());
        }
        let emp = id(doc, "id").ok_or_else(|| Error::internal("employee without id"))?;
        for entity in OWNED {
            let Some(ent) = hc.ctx.schema.entity(entity) else { continue };
            let sql = format!(
                "update {} set employee_user = $3 where tenant_id = $1 and employee = $2 and employee_user is distinct from $3",
                ferrum_kernel::ddl::q(&ent.table)
            );
            sqlx::query(sqlx::AssertSqlSafe(sql)).bind(hc.ctx.tenant).bind(emp).bind(s(doc, "user")).execute(&mut **hc.tx).await?;
        }
        Ok(())
    }
}

// ───────────────────────────── lifecycle ─────────────────────────────

/// Onboarding and separation: activity checklists whose completion drives the status.
struct ChecklistHook;

#[async_trait]
impl DocHook for ChecklistHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(emp) = stamp_company(hc, doc).await? {
            set(doc, "employee_name", s(&emp, "employee_name").unwrap_or_default());
        } else if s(doc, "employee_name").is_none()
            && let Some(app) = lookup(hc, "JobApplicant", id(doc, "job_applicant")).await?
        {
            set(doc, "employee_name", s(&app, "applicant_name").unwrap_or_default());
        }
        let mut acts = rows(doc, "activities");
        for a in acts.iter_mut() {
            if !b(a, "done") {
                set(a, "completed_on", Value::Null);
            } else if s(a, "completed_on").is_none() {
                set(a, "completed_on", today().to_string());
            }
        }
        let done = acts.iter().filter(|a| b(a, "done")).count();
        let status = match done {
            0 => "Pending",
            n if n == acts.len() => "Completed",
            _ => "In Process",
        };
        set(doc, "status", status);
        set(doc, "activities", Value::Array(acts.into_iter().map(Value::Object).collect()));
        Ok(())
    }
}

/// A promotion or transfer: which employee fields it changes and where it keeps the old values.
struct Move {
    label: &'static str,
    date_field: &'static str,
    /// (employee field, current value field, new value field)
    fields: &'static [(&'static str, &'static str, &'static str)],
}

const PROMOTION: Move = Move {
    label: "promotion",
    date_field: "promotion_date",
    fields: &[
        ("designation", "current_designation", "new_designation"),
        ("grade", "current_grade", "new_grade"),
        ("department", "current_department", "new_department"),
    ],
};

const TRANSFER: Move = Move {
    label: "transfer",
    date_field: "transfer_date",
    fields: &[
        ("department", "current_department", "new_department"),
        ("branch", "current_branch", "new_branch"),
        ("reports_to", "current_reports_to", "new_reports_to"),
    ],
};

struct MoveHook(&'static Move);

impl MoveHook {
    /// Writes values onto the employee through the normal pipeline (validation, audit).
    async fn apply(&self, hc: &mut HookCtx<'_>, doc: &Doc, to_new: bool) -> Result<()> {
        let emp = id(doc, "employee").ok_or_else(|| Error::field("employee", "is required"))?;
        let mut patch = Doc::new();
        for (field, current, new) in self.0.fields {
            if id(doc, new).is_some() {
                let source = if to_new { new } else { current };
                patch.insert(field.to_string(), doc.get(*source).cloned().unwrap_or(Value::Null));
            }
        }
        hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, "Employee", emp, patch, None).await?;
        Ok(())
    }
}

#[async_trait]
impl DocHook for MoveHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let Some(emp) = stamp_company(hc, doc).await? else { return Ok(()) };
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0 {
            for (field, current, _) in self.0.fields {
                doc.insert(current.to_string(), emp.get(*field).cloned().unwrap_or(Value::Null));
            }
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let changes = self.0.fields.iter().filter(|(_, cur, new)| id(doc, new).is_some() && id(doc, new) != id(doc, cur)).count();
        if changes == 0 {
            return Err(Error::invalid(format!("this {} changes nothing; set at least one new value", self.0.label)));
        }
        same_company(hc, doc, "new_department", "Department").await?;
        if self.0.fields.iter().any(|f| f.2 == "new_reports_to") && id(doc, "new_reports_to").is_some() && id(doc, "new_reports_to") == id(doc, "employee") {
            return Err(Error::field("new_reports_to", "an employee cannot report to themselves"));
        }
        Ok(())
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(day) = opt_date(doc, self.0.date_field)
            && crate::is_future(hc, day).await?
        {
            return Err(Error::field(self.0.date_field, format!("a {} cannot be submitted before its date", self.0.label)));
        }
        Ok(())
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        self.apply(hc, doc, true).await
    }

    /// Only the latest change can be undone: the employee must still have this document's values.
    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let emp = employee_of(hc, doc).await?;
        for (field, _, new) in self.0.fields {
            if id(doc, new).is_some() && id(&emp, field) != id(doc, new) {
                return Err(Error::invalid(format!(
                    "the employee's {} changed after this {}; cancel the later change first",
                    field.replace('_', " "),
                    self.0.label
                )));
            }
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        self.apply(hc, doc, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_join_present_parts() {
        assert_eq!(person_name(&[Some(" Asha "), None, Some("Rao")]), "Asha Rao");
        assert_eq!(person_name(&[Some("Asha"), Some(""), None]), "Asha");
    }
}
