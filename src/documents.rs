//! Employee documents: a file, number and dates of a type the workspace keeps (`EmployeeDocumentType`),
//! verified by HR, warned about before it expires (once, by the upkeep, through the message template
//! `hr.document_expiring`), and reported when expiring or missing. Who sees one is the role rules': HR, the
//! employee when the type lets them, their manager for a general type that says so; identity and bank documents
//! stay with those who see the employee's identity or bank details.

use crate::{local_today, lookup, stamp_company, stored};
use async_trait::async_trait;
use ferrum_domain::util::{b, get, id, s, set, sys};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Action, Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) struct DocumentHook;

/// What a verification vouches for: changing any of it takes the verification away.
const VERIFIED_CONTENT: &[&str] = &["document_type", "file", "document_number", "issue_date", "expiry_date", "employee"];

#[async_trait]
impl DocHook for DocumentHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if let Some(old) = stored(hc, doc).await? {
            if VERIFIED_CONTENT.iter().any(|f| old.get(*f) != doc.get(*f)) {
                set(doc, "verified_by", Value::Null);
                set(doc, "verified_on", Value::Null);
            }
            // A new expiry date is warned about again.
            if old.get("expiry_date") != doc.get("expiry_date") {
                set(doc, "expiry_warned_on", Value::Null);
            }
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(kind) = lookup(hc, "EmployeeDocumentType", id(doc, "document_type")).await? else { return Ok(()) };
        if b(&kind, "expires") && s(doc, "expiry_date").is_none() {
            return Err(Error::field("expiry_date", format!("is required for a {}", s(&kind, "type_name").unwrap_or_default())));
        }
        let old = stored(hc, doc).await?;
        let action = if old.is_some() { Action::Write } else { Action::Create };
        if owner_only(hc.ctx, action) {
            // The employee themselves: only their own, of a type they upload, and not once HR verified it.
            let emp = lookup(hc, "Employee", id(doc, "employee")).await?;
            if !emp.as_ref().and_then(|e| s(e, "user")).is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email)) {
                return Err(Error::Forbidden("you can only add your own documents".into()));
            }
            if !b(&kind, "self_upload") || s(&kind, "visible_to") == Some("HR only") {
                return Err(Error::Forbidden(format!("HR adds documents of type {}", s(&kind, "type_name").unwrap_or_default())));
            }
            if old.as_ref().is_some_and(|o| s(o, "verified_on").is_some()) {
                return Err(Error::Forbidden("a verified document is changed by HR".into()));
            }
        }
        if b(&kind, "disabled") && old.is_none_or(|o| o.get("document_type") != doc.get("document_type")) {
            return Err(Error::field("document_type", "is disabled"));
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "verify" {
            return Ok(None);
        }
        // HR verifies; never the employee, nor anyone on their own document.
        if owner_only(hc.ctx, Action::Write) || s(doc, "employee_user").is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email)) {
            return Err(Error::Forbidden("HR verifies a document, never the person it is about".into()));
        }
        let me = id(doc, "id").ok_or_else(|| Error::internal("document without id"))?;
        let today = local_today(hc.tx, hc.ctx.tenant).await?;
        let mut v = Doc::new();
        set(&mut v, "verified_by", hc.ctx.email.as_str());
        set(&mut v, "verified_on", today.to_string());
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EmployeeDocument", me, v.clone()).await?;
        Ok(Some(Value::Object(v)))
    }
}

/// Whether the caller's rules for `action` on documents reach only their own (the Employee role), not others'
/// by a condition (HR User's) or all of them.
fn owner_only(ctx: &Ctx, action: Action) -> bool {
    ctx.perms.grants("EmployeeDocument", action).is_some_and(|g| !g.is_empty() && g.iter().all(|g| g.owner))
}

/// Maintenance: tells the employee and the HR managers about each document entering its warning window, once.
/// Marking the document and sending happen in one transaction, and the mark is taken by an update that only one
/// run can win, so two runs at once send one warning.
pub(crate) async fn warn_expiring(k: &Kernel, ctx: &Ctx) -> Result<()> {
    if ctx.schema.entity("EmployeeDocument").is_none() {
        return Ok(());
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let today = local_today(&mut tx, ctx.tenant).await?;
    let due: Vec<Uuid> = sqlx::query_scalar(
        "update e_employee_document set expiry_warned_on = $2 where tenant_id = $1 and expiry_warned_on is null and expiry_date is not null \
         and expiry_date - coalesce(warn_days_before::int, 30) <= $2 returning id",
    )
    .bind(ctx.tenant)
    .bind(today)
    .fetch_all(&mut *tx)
    .await?;
    let sctx = sys(ctx);
    for record in due {
        let doc = get(k, &sctx, &mut tx, "EmployeeDocument", record).await?;
        let kind = k.get_tx(&sctx, &mut tx, "EmployeeDocumentType", id(&doc, "document_type").unwrap_or_default(), false).await.ok();
        let value = Value::Object(doc.clone());
        let mut to: Vec<Recipient> = Vec::new();
        for spec in ["field:employee_user", "role:HR Manager"] {
            for r in messages::recipients(k, &mut tx, &sctx, "EmployeeDocument", &value, spec).await? {
                if !to.contains(&r) {
                    to.push(r);
                }
            }
        }
        let mut values = serde_json::Map::new();
        values.insert("document_type".into(), json!(kind.as_ref().and_then(|t| s(t, "type_name")).unwrap_or_default()));
        let m = Message {
            template: "hr.document_expiring".into(),
            entity: Some("EmployeeDocument".into()),
            record: Some(record),
            to,
            values,
            ..Default::default()
        };
        messages::send(k, &mut tx, &sctx, &m).await?;
    }
    tx.commit().await?;
    Ok(())
}
