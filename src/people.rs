//! Organization and employees: departments, the employee master and its sign-in, HR accounts per
//! company, self-service rules, and the lifecycle documents (onboarding, separation, promotion,
//! transfer).

use crate::{employee_of, lookup, opt_date, stamp_company, stored, today};
use async_trait::async_trait;
use ferrum_domain::util::{b, elevated, find, get, id, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, Filter, HookCtx, HookRegistry, Result};
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

/// Entities about one employee whose `employee_user` follows the employee's sign-in, whose own checks say who
/// may write them (an HR user reaches others' documents by a condition, so the self-service check does not fit).
const FOLLOWS_SIGN_IN: &[&str] = &["EmployeeDocument"];

pub(crate) fn register(h: &mut HookRegistry) {
    // The company's HR and payroll accounts come with the connectors to ERP.
    h.scoped(&[crate::HR_ERP.to_string(), crate::PAYROLL_ERP.to_string()], |h| {
        h.register("Company", Arc::new(CompanyHook));
    });
    h.register("Department", Arc::new(DepartmentHook))
        .register("Employee", Arc::new(EmployeeHook))
        .register("EmployeeOnboarding", Arc::new(ChecklistHook))
        .register("EmployeeSeparation", Arc::new(ChecklistHook))
        .register("EmployeeSeparation", Arc::new(SeparationHook))
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
    ("pay_employee_loan_account", "Employee Loans", "1175", "Current Assets", "Asset"),
    ("pay_loan_interest_account", "Interest on Employee Loans", "4220", "Indirect Income", "Income"),
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
        for entity in OWNED.iter().chain(FOLLOWS_SIGN_IN) {
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

/// Onboarding and separation: checklists whose required tasks' completion drives the status. Their tasks come
/// from a checklist template ([`choose_template`]) copied in by the `apply_checklist` action's `copy_lines` when
/// the record is made; each task's assignee is named from the employee's field the template gives (their manager,
/// say), else whoever holds its role does it.
struct ChecklistHook;

/// The date a checklist's tasks are due from, and the template purpose, by entity.
fn checklist_of(entity: &str) -> (&'static str, &'static str) {
    if entity == "EmployeeSeparation" { ("relieving_date", "Offboarding") } else { ("date_of_joining", "Onboarding") }
}

/// The template of `purpose` for `emp`: of those whose department, designation and employment type are empty or
/// the employee's, the highest priority, then the one matching most, then by name.
pub(crate) async fn choose_template(hc: &mut HookCtx<'_>, purpose: &str, emp: &Doc) -> Result<Option<String>> {
    if hc.ctx.schema.entity("ChecklistTemplate").is_none() {
        return Ok(None);
    }
    Ok(sqlx::query_scalar(
        "select id::text from e_checklist_template where tenant_id = $1 and purpose = $2 and not coalesce(disabled, false) \
         and (department is null or department = $3) and (designation is null or designation = $4) and (employment_type is null or employment_type = $5) \
         order by coalesce(priority, 0) desc, (department is not null)::int + (designation is not null)::int + (employment_type is not null)::int desc, template_name limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(purpose)
    .bind(id(emp, "department"))
    .bind(id(emp, "designation"))
    .bind(id(emp, "employment_type"))
    .fetch_optional(&mut **hc.tx)
    .await?)
}

/// Who an employee's `field` names: an email, or the sign-in of the employee a link names.
async fn assignee_from(hc: &mut HookCtx<'_>, emp: &Doc, field: &str) -> Result<Option<String>> {
    let Some(f) = hc.ctx.schema.entity("Employee").and_then(|e| e.field(field)).cloned() else { return Ok(None) };
    if f.to.as_deref() == Some("Employee") {
        return Ok(lookup(hc, "Employee", id(emp, field)).await?.and_then(|m| s(&m, "user").map(str::to_string)));
    }
    Ok(s(emp, field).filter(|v| v.contains('@')).map(str::to_string))
}

/// Whether a task counts as done: ticked and, when it asks for one, approved.
fn task_done(a: &Doc) -> bool {
    b(a, "done") && (!b(a, "requires_approval") || b(a, "approved"))
}

#[async_trait]
impl DocHook for ChecklistHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let emp = stamp_company(hc, doc).await?;
        if let Some(emp) = &emp {
            set(doc, "employee_name", s(emp, "employee_name").unwrap_or_default());
        } else if s(doc, "employee_name").is_none()
            && let Some(app) = lookup(hc, "JobApplicant", id(doc, "job_applicant")).await?
        {
            set(doc, "employee_name", s(&app, "applicant_name").unwrap_or_default());
        }
        let entity = hc.entity.def.name.clone();
        let (_, purpose) = checklist_of(&entity);
        let templated = hc.entity.field("checklist_template").is_some();
        let mut acts = rows(doc, "activities");
        if templated && s(doc, "checklist_template").is_none() && acts.is_empty() {
            // Chosen by the department, designation and employment type: the employee's, else the onboarding's own.
            let mut who = emp.clone().unwrap_or_default();
            for f in ["department", "designation"] {
                if id(&who, f).is_none() {
                    who.insert(f.into(), doc.get(f).cloned().unwrap_or_default());
                }
            }
            if let Some(tpl) = choose_template(hc, purpose, &who).await? {
                set(doc, "checklist_template", tpl);
            }
        }
        let old = stored(hc, doc).await?;
        let before: std::collections::HashMap<String, Doc> =
            old.as_ref().map(|o| rows(o, "activities").into_iter().filter_map(|r| Some((s(&r, "id")?.to_string(), r))).collect()).unwrap_or_default();
        for a in acts.iter_mut() {
            if s(a, "assignee").is_none()
                && let (Some(field), Some(emp)) = (s(a, "assignee_field").map(str::to_string), &emp)
                && let Some(who) = assignee_from(hc, emp, &field).await?
            {
                set(a, "assignee", who);
            }
            if !b(a, "done") {
                set(a, "completed_on", Value::Null);
            } else if s(a, "completed_on").is_none() {
                set(a, "completed_on", today().to_string());
            }
            let was = s(a, "id").and_then(|i| before.get(i));
            let approved_by = match was {
                _ if !b(a, "approved") => Value::Null,
                Some(w) if b(w, "approved") => w.get("approved_by").cloned().unwrap_or_default(),
                _ => json!(hc.ctx.email),
            };
            set(a, "approved_by", approved_by);
            // Reminded once per due date: a new one is reminded about again.
            let reminded = was.filter(|w| w.get("due_date") == a.get("due_date")).and_then(|w| w.get("reminded_on").cloned()).unwrap_or_default();
            set(a, "reminded_on", reminded);
        }
        let required: Vec<&Doc> = acts.iter().filter(|a| a.get("required").and_then(Value::as_bool).unwrap_or(true)).collect();
        let done = acts.iter().filter(|a| task_done(a)).count();
        let status = if done == 0 {
            "Pending"
        } else if required.iter().all(|a| task_done(a)) {
            "Completed"
        } else {
            "In Process"
        };
        set(doc, "status", status);
        set(doc, "activities", Value::Array(acts.into_iter().map(Value::Object).collect()));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let entity = hc.entity.def.name.clone();
        let old = stored(hc, doc).await?;
        let before: std::collections::HashMap<String, Doc> =
            old.as_ref().map(|o| rows(o, "activities").into_iter().filter_map(|r| Some((s(&r, "id")?.to_string(), r))).collect()).unwrap_or_default();
        let others = crate::full_access(hc.ctx, &entity, &[Action::Write]);
        let mut errors = Vec::new();
        for (i, a) in rows(doc, "activities").iter().enumerate() {
            let was = s(a, "id").and_then(|i| before.get(i));
            let changed = was.is_none_or(|w| w != a);
            // Someone reaching the record through a task named for them changes only their own tasks.
            if !others && changed && !s(a, "assignee").is_some_and(|x| x.eq_ignore_ascii_case(&hc.ctx.email)) {
                return Err(Error::Forbidden("you can change only the tasks named for you".into()));
            }
            if b(a, "approved") && !was.is_some_and(|w| b(w, "approved")) && s(a, "assignee").is_some_and(|x| x.eq_ignore_ascii_case(&hc.ctx.email)) {
                errors.push(FieldError::new("approved", "is given by someone other than whoever does the task").in_row("activities", i));
            }
            if b(a, "done")
                && !was.is_some_and(|w| b(w, "done"))
                && let (Some(kind), Some(emp)) = (id(a, "document_type"), id(doc, "employee"))
            {
                let has: bool =
                    sqlx::query_scalar("select exists(select 1 from e_employee_document where tenant_id = $1 and employee = $2 and document_type = $3)")
                        .bind(hc.ctx.tenant)
                        .bind(emp)
                        .bind(kind)
                        .fetch_one(&mut **hc.tx)
                        .await?;
                if !has {
                    errors.push(FieldError::new("done", "waits for the employee's document of this type").in_row("activities", i));
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    /// A new record with a template gets its tasks at once, through the template's `apply_checklist` action.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        let entity = hc.entity.def.name.clone();
        let (from, _) = checklist_of(&entity);
        if old.is_some() || s(doc, "checklist_template").is_none() || !rows(doc, "activities").is_empty() || s(doc, from).is_none() {
            return Ok(());
        }
        if !hc.entity.def.actions.iter().any(|a| a.name == "apply_checklist") {
            return Ok(());
        }
        let me = id(doc, "id").ok_or_else(|| Error::internal("checklist without id"))?;
        hc.kernel.run_action_tx(&elevated(hc.ctx), hc.tx, &entity, me, "apply_checklist", &json!({})).await?;
        Ok(())
    }
}

/// Maintenance: reminds whoever does an onboarding or offboarding task once it is overdue, once (the mark and the
/// sends in one transaction, the mark taken by an update one run wins).
pub(crate) async fn remind_overdue_tasks(k: &ferrum_kernel::Kernel, ctx: &ferrum_kernel::Ctx) -> Result<()> {
    let has = |e: &str| ctx.schema.entity(e).and_then(|e| e.field("activities")).is_some();
    if !has("EmployeeOnboarding") || ctx.schema.entity("EmployeeActivity").and_then(|e| e.field("reminded_on")).is_none() {
        return Ok(());
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let today = crate::local_today(&mut tx, ctx.tenant).await?;
    let sctx = sys(ctx);
    for (parent, child, entity) in [
        ("e_employee_onboarding", "e_employee_onboarding__activities", "EmployeeOnboarding"),
        ("e_employee_separation", "e_employee_separation__activities", "EmployeeSeparation"),
    ] {
        let sql = format!(
            "update {child} a set reminded_on = $2 from {parent} p where a.tenant_id = $1 and p.tenant_id = $1 and p.id = a.parent_id \
             and p.status <> 'Completed' and coalesce(p.docstatus, 0) < 2 and not coalesce(a.done, false) and a.due_date < $2 and a.reminded_on is null \
             returning a.parent_id, a.activity_name, a.assignee, a.assignee_role, a.due_date, p.employee_name"
        );
        let due: Vec<(Uuid, Option<String>, Option<String>, Option<String>, chrono::NaiveDate, Option<String>)> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(ctx.tenant).bind(today).fetch_all(&mut *tx).await?;
        for (record, task, assignee, role, due_date, name) in due {
            let to = match (assignee, role) {
                (Some(a), _) => vec![ferrum_kernel::messages::Recipient::address(&a)],
                (None, Some(r)) => ferrum_kernel::messages::recipients(k, &mut tx, &sctx, entity, &json!({}), &format!("role:{r}")).await?,
                _ => continue,
            };
            let mut values = serde_json::Map::new();
            values.insert("task".into(), json!(task));
            values.insert("employee_name".into(), json!(name));
            values.insert("due_date".into(), json!(due_date.to_string()));
            let m = ferrum_kernel::messages::Message {
                template: "hr.checklist_task_overdue".into(),
                entity: Some(entity.into()),
                record: Some(record),
                to,
                values,
                ..Default::default()
            };
            ferrum_kernel::messages::send(k, &mut tx, &sctx, &m).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Reasons a separation needs no notice.
const NO_NOTICE: &[&str] = &["Death", "Retirement", "Contract End"];

/// A separation: the notice due (the employee's notice period, else the company's; none on a
/// death, retirement or contract end), what the last working day leaves of it served, and how a
/// shortfall is settled. Submitted, its last working day becomes the employee's relieving date:
/// they stay at work (Active: paid, accruing leave, on the calendars and in the directory) until
/// it has passed, when the upkeep makes them Left and ends their sign-in ([`offboard_leavers`]),
/// at once when it has passed already. Cancelled (withdrawn), they stay. A sign-in already ended
/// stays ended: turning it on again is an administrator's.
struct SeparationHook;

#[async_trait]
impl DocHook for SeparationHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        let Some(emp) = lookup(hc, "Employee", id(doc, "employee")).await? else { return Ok(()) };
        let reason = s(doc, "reason").unwrap_or_default().to_string();
        if reason == "Death" {
            set(doc, "notice_period_days", 0);
        } else if doc.get("notice_period_days").is_none_or(Value::is_null) {
            let due = if NO_NOTICE.contains(&reason.as_str()) {
                Some(0)
            } else {
                match emp.get("notice_period_days").and_then(Value::as_i64) {
                    Some(n) => Some(n),
                    None => lookup(hc, "Company", id(&emp, "company")).await?.and_then(|c| c.get("hr_notice_period_days").and_then(Value::as_i64)),
                }
            };
            set(doc, "notice_period_days", due.unwrap_or(0));
        }
        let due = doc.get("notice_period_days").and_then(Value::as_i64).unwrap_or(0);
        let served = match (opt_date(doc, "resignation_letter_date"), opt_date(doc, "relieving_date")) {
            (Some(given), Some(last)) if last >= given => (last - given).num_days(),
            _ => 0,
        };
        let short = (due - served).max(0);
        set(doc, "notice_served_days", served);
        set(doc, "notice_shortfall_days", short);
        if short == 0 {
            set(doc, "shortfall_settlement", Value::Null);
        } else if s(doc, "shortfall_settlement").is_none() {
            set(doc, "shortfall_settlement", if reason == "Resignation" { "Recover from Employee" } else { "Pay in Lieu of Notice" });
        }
        Ok(())
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let (Some(given), Some(last)) = (opt_date(doc, "resignation_letter_date"), opt_date(doc, "relieving_date"))
            && given > last
        {
            return Err(Error::field("resignation_letter_date", format!("is after the last working day ({last})")));
        }
        Ok(())
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let day = opt_date(doc, "relieving_date").ok_or_else(|| Error::field("relieving_date", "is required to submit a separation"))?;
        let emp = id(doc, "employee").ok_or_else(|| Error::field("employee", "is required"))?;
        let mut v = Doc::new();
        set(&mut v, "relieving_date", day.to_string());
        if day < crate::local_today(hc.tx, hc.ctx.tenant).await? {
            set(&mut v, "status", "Left");
        }
        hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, "Employee", emp, v, None).await?;
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(e) = lookup(hc, "Employee", id(doc, "employee")).await? else { return Ok(()) };
        if !matches!(s(&e, "status"), Some("Left" | "Active")) || s(&e, "relieving_date") != s(doc, "relieving_date") {
            return Ok(());
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Active");
        set(&mut v, "relieving_date", Value::Null);
        hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, "Employee", id(&e, "id").unwrap_or_default(), v, None).await?;
        Ok(())
    }
}

/// Employees whose last working day has passed in the workspace's time zone become Left (those a
/// submitted separation names; one set Left or Active by hand is left alone). Then those who have
/// left lose their sign-in: Ferrum deprovisions the user (sessions, keys and grants end at once;
/// what they were named on moves to the user of the employee they reported to, when there is one).
/// An inactive user is not touched again, and a refusal (the workspace's last administrator) is
/// logged and tried again next time.
pub(crate) async fn offboard_leavers(k: &ferrum_kernel::Kernel, ctx: &ferrum_kernel::Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let tz = crate::attendance::timezone(&mut tx, ctx.tenant).await?;
    let gone: Vec<Uuid> = sqlx::query_scalar(
        "select e.id from e_employee e where e.tenant_id = $1 and e.status = 'Active' and e.relieving_date < (now() at time zone $2)::date \
         and exists (select 1 from e_employee_separation s where s.tenant_id = e.tenant_id and s.employee = e.id and s.docstatus = 1 \
         and s.relieving_date = e.relieving_date) order by e.relieving_date, e.id limit 100",
    )
    .bind(ctx.tenant)
    .bind(&tz)
    .fetch_all(&mut *tx)
    .await?;
    let admin = elevated(ctx);
    for emp in gone {
        let mut v = Doc::new();
        set(&mut v, "status", "Left");
        k.update_tx(&admin, &mut tx, "Employee", emp, v, None).await?;
    }
    let leavers: Vec<(String, Uuid, Option<Uuid>, chrono::NaiveDate)> = sqlx::query_as(
        "select e.employee_name, u.id, mu.id, e.relieving_date from e_employee e \
         join app_user u on u.tenant_id = e.tenant_id and u.email = e.\"user\"::citext and u.is_active and u.kind = 'member' \
         left join e_employee m on m.tenant_id = e.tenant_id and m.id = e.reports_to \
         left join app_user mu on mu.tenant_id = e.tenant_id and mu.email = m.\"user\"::citext and mu.is_active and mu.kind = 'member' \
         where e.tenant_id = $1 and e.status = 'Left' and e.relieving_date <= (now() at time zone $2)::date order by e.relieving_date, e.id limit 100",
    )
    .bind(ctx.tenant)
    .bind(&tz)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for (name, user, manager, day) in leavers {
        let reason = format!("{name} left on {day} (HR)");
        if let Err(e) = k.deprovision_user(&admin, user, manager.filter(|m| *m != user), &reason).await {
            tracing::warn!(tenant = %ctx.tenant, %user, "a leaver's sign-in could not be ended: {e}");
        }
    }
    Ok(())
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
