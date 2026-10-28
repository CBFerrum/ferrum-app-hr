//! Recruitment (openings, applicants, interviews, offers that become employees) and appraisals.

use crate::people::{person_name, same_company};
use crate::{lookup, opt_date, stamp_company, today};
use async_trait::async_trait;
use ferrum_domain::util::{d, elevated, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Action, Ctx, Doc, DocHook, Error, FieldError, HookCtx, HookRegistry, Kernel, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("JobOpening", Arc::new(OpeningHook))
        .register("Interview", Arc::new(InterviewHook))
        .register("JobOffer", Arc::new(OfferHook))
        .register("AppraisalTemplate", Arc::new(TemplateHook))
        .register("Appraisal", Arc::new(AppraisalHook));
}

/// Maintenance: openings past their closing date close.
pub(crate) async fn close_openings(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    sqlx::query("update e_job_opening set status = 'Closed', updated_at = now() where tenant_id = $1 and status = 'Open' and closes_on < current_date")
        .bind(ctx.tenant)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

struct OpeningHook;

#[async_trait]
impl DocHook for OpeningHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "department", "Department").await
    }
}

struct InterviewHook;

#[async_trait]
impl DocHook for InterviewHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let feedback = rows(doc, "feedback");
        let ratings: Vec<i64> = feedback.iter().filter_map(|f| f.get("rating").and_then(Value::as_i64)).collect();
        let average = (!ratings.is_empty()).then(|| (Decimal::from(ratings.iter().sum::<i64>()) / Decimal::from(ratings.len() as u64)).round_dp(2));
        set(doc, "average_rating", average.map(money).unwrap_or_default());
        if !feedback.is_empty() && s(doc, "status") == Some("Pending") {
            set(doc, "status", "Under Review");
        }
        Ok(())
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let panel: HashSet<String> = rows(doc, "interviewers").iter().filter_map(|r| s(r, "interviewer").map(str::to_lowercase)).collect();
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        let feedback = rows(doc, "feedback");
        for (i, f) in feedback.iter().enumerate() {
            let who = s(f, "interviewer").unwrap_or_default().to_lowercase();
            if !panel.contains(&who) {
                errors.push(FieldError::new("interviewer", "is not on this interview's panel").in_row("feedback", i));
            } else if !seen.insert(who) {
                errors.push(FieldError::new("interviewer", "already gave feedback").in_row("feedback", i));
            }
        }
        if feedback.is_empty() && matches!(s(doc, "status"), Some("Cleared" | "Rejected")) {
            errors.push(FieldError::new("status", "record the interviewers' feedback before deciding"));
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }
}

struct OfferHook;

impl OfferHook {
    async fn decide(&self, hc: &mut HookCtx<'_>, doc: &Doc, status: &str) -> Result<Value> {
        let sctx = sys(hc.ctx);
        let me = id(doc, "id").ok_or_else(|| Error::internal("offer without id"))?;
        let mut v = Doc::new();
        set(&mut v, "status", status);
        hc.kernel.set_values_tx(&sctx, hc.tx, "JobOffer", me, v.clone()).await?;
        if let Some(app) = id(doc, "job_applicant") {
            hc.kernel.set_values_tx(&sctx, hc.tx, "JobApplicant", app, v).await?;
        }
        Ok(json!({"status": status}))
    }

    /// The employee record for an accepted offer; an offer makes at most one.
    async fn make_employee(&self, hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
        if let Some(existing) = lookup(hc, "Employee", id(doc, "employee")).await? {
            return Err(Error::invalid(format!(
                "employee {} ({}) was already created from this offer",
                s(&existing, "number").unwrap_or_default(),
                s(&existing, "employee_name").unwrap_or_default()
            )));
        }
        hc.ctx.require("Employee", Action::Create)?;
        let me = id(doc, "id").ok_or_else(|| Error::internal("offer without id"))?;
        let app = get(hc.kernel, hc.ctx, hc.tx, "JobApplicant", id(doc, "job_applicant").unwrap_or_default()).await?;
        let full = s(&app, "applicant_name").unwrap_or_default().trim().to_string();
        let (first, last) = full.split_once(' ').map_or((full.as_str(), ""), |(f, l)| (f, l.trim()));
        let joining = match params.get("date_of_joining").and_then(Value::as_str) {
            Some(v) => ferrum_kernel::value::parse_date(v).map_err(|m| Error::field("date_of_joining", m))?,
            None => opt_date(doc, "expected_joining_date").unwrap_or_else(today),
        };
        let mut e = Doc::new();
        set(&mut e, "first_name", first);
        set(&mut e, "last_name", person_name(&[Some(last)]));
        set(&mut e, "company", doc.get("company").cloned().unwrap_or_default());
        set(&mut e, "designation", doc.get("designation").cloned().unwrap_or_default());
        set(&mut e, "department", doc.get("department").cloned().unwrap_or_default());
        set(&mut e, "date_of_joining", joining.to_string());
        set(&mut e, "personal_email", app.get("email_id").cloned().unwrap_or_default());
        set(&mut e, "cell_number", app.get("phone_number").cloned().unwrap_or_default());
        if let Some(user) = params.get("user").and_then(Value::as_str) {
            set(&mut e, "user", user);
        }
        let created = hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "Employee", e).await?;
        let sctx = sys(hc.ctx);
        let mut v = Doc::new();
        v.insert("employee".into(), created.get("id").cloned().unwrap_or_default());
        hc.kernel.set_values_tx(&sctx, hc.tx, "JobOffer", me, v).await?;
        if let Some(opening) = lookup(hc, "JobOpening", id(doc, "job_opening")).await? {
            let hired = opening.get("hired").and_then(Value::as_i64).unwrap_or(0) + 1;
            let vacancies = opening.get("vacancies").and_then(Value::as_i64).unwrap_or(1);
            let mut v = Doc::new();
            v.insert("hired".into(), json!(hired));
            if hired >= vacancies {
                v.insert("status".into(), json!("Closed"));
            }
            hc.kernel.set_values_tx(&sctx, hc.tx, "JobOpening", id(&opening, "id").unwrap_or_default(), v).await?;
        }
        Ok(Value::Object(created))
    }
}

#[async_trait]
impl DocHook for OfferHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(opening) = lookup(hc, "JobOpening", id(doc, "job_opening")).await? {
            for f in ["designation", "department"] {
                if id(doc, f).is_none() {
                    doc.insert(f.into(), opening.get(f).cloned().unwrap_or_default());
                }
            }
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "department", "Department").await
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if id(doc, "employee").is_some() {
            return Err(Error::invalid("an employee was created from this offer; it cannot be cancelled"));
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "accept" => self.decide(hc, doc, "Accepted").await.map(Some),
            "reject" => self.decide(hc, doc, "Rejected").await.map(Some),
            "make_employee" => self.make_employee(hc, doc, params).await.map(Some),
            _ => Ok(None),
        }
    }
}

fn weight_errors(goals: &[Doc], table: &str) -> Option<FieldError> {
    if goals.is_empty() {
        return None;
    }
    let total: Decimal = goals.iter().map(|g| d(g, "per_weightage")).sum();
    (total != Decimal::ONE_HUNDRED).then(|| FieldError::new(table, format!("the weightages add up to {}%, not 100%", total.normalize())))
}

struct TemplateHook;

#[async_trait]
impl DocHook for TemplateHook {
    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        weight_errors(&rows(doc, "goals"), "goals").map_or(Ok(()), |e| Err(Error::Validation(vec![e])))
    }
}

struct AppraisalHook;

#[async_trait]
impl DocHook for AppraisalHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        let mut goals = rows(doc, "goals");
        if goals.is_empty()
            && let Some(t) = lookup(hc, "AppraisalTemplate", id(doc, "appraisal_template")).await?
        {
            goals = rows(&t, "goals")
                .iter()
                .map(|g| {
                    let mut r = Doc::new();
                    set(&mut r, "id", uuid::Uuid::now_v7().to_string());
                    set(&mut r, "kra", g.get("kra").cloned().unwrap_or_default());
                    set(&mut r, "per_weightage", g.get("per_weightage").cloned().unwrap_or_default());
                    r
                })
                .collect();
        }
        let (mut own, mut fin) = (Decimal::ZERO, Decimal::ZERO);
        for (i, g) in goals.iter_mut().enumerate() {
            let w = d(g, "per_weightage") / Decimal::ONE_HUNDRED;
            let earned = (d(g, "manager_score") * w).round_dp(2);
            set(g, "idx", i);
            set(g, "score_earned", money(earned));
            own += d(g, "self_score") * w;
            fin += earned;
        }
        set(doc, "goals", Value::Array(goals.into_iter().map(Value::Object).collect()));
        set(doc, "self_score", money(own.round_dp(2)));
        set(doc, "final_score", money(fin));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(e) = weight_errors(&rows(doc, "goals"), "goals") {
            return Err(Error::Validation(vec![e]));
        }
        same_company(hc, doc, "appraisal_cycle", "AppraisalCycle").await?;
        let dup: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_appraisal where tenant_id = $1 and employee = $2 and appraisal_cycle = $3 and docstatus < 2 and id <> $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "employee"))
        .bind(id(doc, "appraisal_cycle"))
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = dup {
            return Err(Error::field("appraisal_cycle", format!("this employee already has appraisal {n} in this cycle")));
        }
        Ok(())
    }

    async fn before_submit(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let goals = rows(doc, "goals");
        if goals.is_empty() {
            return Err(Error::field("goals", "add the goals to appraise"));
        }
        let missing: Vec<FieldError> = goals
            .iter()
            .enumerate()
            .filter(|(_, g)| g.get("manager_score").is_none_or(Value::is_null))
            .map(|(i, _)| FieldError::new("manager_score", "score every goal before submitting").in_row("goals", i))
            .collect();
        if missing.is_empty() { Ok(()) } else { Err(Error::Validation(missing)) }
    }
}
