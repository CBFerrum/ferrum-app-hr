//! HR and payroll: employees, attendance, shifts, leave, expense claims, recruitment, appraisals,
//! salary structures and payroll posting.
//!
//! Records about an employee carry the employee's sign-in email in `employee_user` (copied from
//! the employee and listed as the entity's owner field), so employees see their own leave,
//! attendance, claims and salary slips whoever created them. Money posts through the domain
//! ledger; the ledger has no employee party, so employee balances live on dedicated accounts and
//! every line names its source document (`against`).

mod attendance;
mod calendar;
mod expense;
mod leave;
mod payroll;
mod people;
mod reports;
mod talent;

use chrono::NaiveDate;
use ferrum_domain::reports::ReportSource;
use ferrum_domain::util::{get, id, s, sys};
use ferrum_kernel::{Access, Action, Ctx, Doc, Error, HookCtx, HookRegistry, Kernel, Result};
use std::sync::Arc;
use uuid::Uuid;

/// Registers this package's document hooks.
pub fn register(h: &mut HookRegistry) {
    // First, so a refused self-service write fails before any other check. Every hook's
    // `before_validate` (which may derive the employee) still runs before any `validate`.
    people::register_self_service(h);
    people::register(h);
    attendance::register(h);
    leave::register(h);
    expense::register(h);
    talent::register(h);
    payroll::register(h);
}

/// This package's reports.
pub fn reports() -> Vec<Arc<dyn ReportSource>> {
    vec![Arc::new(reports::HrReports)]
}

/// Periodic upkeep: every minute, on one replica at a time (advisory lock); must be idempotent and cheap.
/// Per tenant with HR installed: attendance from check-ins for finished shifts, expiry of ended
/// leave allocations, and closing job openings past their closing date.
pub async fn maintenance(k: &Kernel) -> Result<()> {
    let tenants: Vec<Uuid> = {
        let mut tx = k.db.system_tx().await?;
        let t = sqlx::query_scalar("select id from tenant where status = 'active'").fetch_all(&mut *tx).await?;
        tx.commit().await?;
        t
    };
    for tenant in tenants {
        let schema = match k.registry.schema(tenant).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%tenant, "hr upkeep skipped: {e}");
                continue;
            }
        };
        if schema.entity("ShiftType").is_none() {
            continue;
        }
        let ctx = Ctx::system(tenant, schema);
        let results = [
            ("attendance", attendance::auto_attendance(k, &ctx).await),
            ("leave expiry", leave::expire_allocations(k, &ctx).await),
            ("job openings", talent::close_openings(k, &ctx).await),
        ];
        for (what, r) in results {
            if let Err(e) = r {
                tracing::warn!(%tenant, "hr upkeep ({what}) failed: {e}");
            }
        }
    }
    Ok(())
}

pub(crate) fn today() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

/// Whether `day` is after today in the tenant's time zone. Only days next to today's UTC date
/// can differ, so the zone is looked up just for those.
pub(crate) async fn is_future(hc: &mut HookCtx<'_>, day: NaiveDate) -> Result<bool> {
    let utc = today();
    if day < utc || (day - utc).num_days() > 1 {
        return Ok(day > utc);
    }
    let tz = attendance::timezone(hc.tx, hc.ctx.tenant).await?;
    let local: NaiveDate = sqlx::query_scalar("select (now() at time zone $1)::date").bind(tz).fetch_one(&mut **hc.tx).await?;
    Ok(day > local)
}

/// An optional date field.
pub(crate) fn opt_date(doc: &Doc, k: &str) -> Option<NaiveDate> {
    s(doc, k).and_then(|v| ferrum_kernel::value::parse_date(&v[..v.len().min(10)]).ok())
}

/// A record read with engine rights; `None` when absent (link validation reports bad ids).
pub(crate) async fn lookup(hc: &mut HookCtx<'_>, entity: &str, record: Option<Uuid>) -> Result<Option<Doc>> {
    let Some(record) = record else { return Ok(None) };
    match hc.kernel.get_tx(&sys(hc.ctx), hc.tx, entity, record, false).await {
        Ok(doc) => Ok(Some(doc)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The saved version of the document being validated (`None` while it is being created).
pub(crate) async fn stored(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Option<Doc>> {
    let entity = hc.entity.def.name.clone();
    lookup(hc, &entity, id(doc, "id")).await
}

/// The employee a document is about.
pub(crate) async fn employee_of(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Doc> {
    let emp = id(doc, "employee").ok_or_else(|| Error::field("employee", "is required"))?;
    get(hc.kernel, hc.ctx, hc.tx, "Employee", emp).await
}

/// Copies the employee's company onto a document about them (company-scoped documents follow
/// their employee). Returns the employee when one is set.
pub(crate) async fn stamp_company(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<Option<Doc>> {
    let emp = lookup(hc, "Employee", id(doc, "employee")).await?;
    if let Some(e) = &emp {
        doc.insert("company".into(), e.get("company").cloned().unwrap_or_default());
    }
    Ok(emp)
}

/// The approver named on the employee (`field`), else the user of the employee they report to.
pub(crate) async fn default_approver(hc: &mut HookCtx<'_>, emp: &Doc, field: &str) -> Result<Option<String>> {
    if let Some(a) = s(emp, field) {
        return Ok(Some(a.to_string()));
    }
    let manager = lookup(hc, "Employee", id(emp, "reports_to")).await?;
    Ok(manager.and_then(|m| s(&m, "user").map(str::to_string)))
}

/// The id of the user signing in with `email`.
pub(crate) async fn user_id(hc: &mut HookCtx<'_>, email: &str) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar("select id from app_user where tenant_id = $1 and email = $2::citext and is_active")
        .bind(hc.ctx.tenant)
        .bind(email)
        .fetch_optional(&mut **hc.tx)
        .await?)
}

/// Whether the caller works on every record of the current entity (not only their own).
pub(crate) fn full_access(ctx: &Ctx, entity: &str, actions: &[Action]) -> bool {
    ctx.is_admin || ctx.is_system || actions.iter().any(|a| ctx.perms.access(entity, *a) == Access::All)
}

/// Whether the caller only works on their own records of the current entity (self-service):
/// such callers cannot choose approvers or accounts, which take their defaults.
pub(crate) fn self_service(ctx: &Ctx, entity: &str) -> bool {
    !full_access(ctx, entity, &[Action::Create, Action::Write])
}

/// Approvals come from the approver named on the document, an HR manager or an administrator;
/// nobody else decides on a document about themselves.
pub(crate) fn require_approver(ctx: &Ctx, doc: &Doc, approver: Option<&str>, what: &str) -> Result<()> {
    if ctx.is_system || ctx.has_role("HR Manager") {
        return Ok(());
    }
    if s(doc, "employee_user").is_some_and(|u| u.eq_ignore_ascii_case(&ctx.email)) {
        return Err(Error::Forbidden(format!("you cannot decide on your own {what} request")));
    }
    if approver.is_some_and(|a| a.eq_ignore_ascii_case(&ctx.email)) {
        return Ok(());
    }
    Err(Error::Forbidden(match approver {
        Some(a) => format!("only the {what} approver ({a}) or an HR manager can approve, reject or cancel this"),
        None => format!("no {what} approver is set; an HR manager must decide"),
    }))
}

/// An in-app notification to the user signing in with `email` (not to the caller themselves).
pub(crate) async fn notify(hc: &mut HookCtx<'_>, email: Option<&str>, title: &str, record: Uuid) -> Result<()> {
    let Some(email) = email else { return Ok(()) };
    let Some(user) = user_id(hc, email).await? else { return Ok(()) };
    if user == hc.ctx.user {
        return Ok(());
    }
    let entity = hc.entity.def.name.clone();
    ferrum_kernel::notify::send(hc.tx, hc.ctx.tenant, user, title, None, Some(&entity), Some(record)).await?;
    Ok(())
}

include!(concat!(env!("OUT_DIR"), "/packages.rs"));

/// This app's manifest (`app.yaml`).
pub const MANIFEST: &str = include_str!("../app.yaml");

/// This app's metadata packages, embedded at build time.
pub fn packages() -> std::result::Result<Vec<ferrum_meta::Package>, ferrum_meta::MetaError> {
    ferrum_meta::catalog::load_embedded(EMBEDDED)
}
