//! Recruitment (openings, applicants, interviews, offers that become employees) and appraisals.

use crate::people::{person_name, same_company};
use crate::{lookup, opt_date, stamp_company, stored};
use async_trait::async_trait;
use ferrum_domain::util::{d, elevated, find, get, id, money, rows, s, set, sys};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Action, Ctx, Doc, DocHook, Error, FieldError, Filter, HookCtx, HookRegistry, Kernel, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("JobOpening", Arc::new(OpeningHook))
        .register("JobRequisition", Arc::new(RequisitionHook))
        .register("JobApplicant", Arc::new(ApplicantHook))
        .register("Interview", Arc::new(InterviewHook))
        .register("JobOffer", Arc::new(OfferHook))
        .register("AppraisalTemplate", Arc::new(TemplateHook))
        .register("AppraisalCycle", Arc::new(CycleHook))
        .register("Appraisal", Arc::new(AppraisalHook))
        .register("TrainingEvent", Arc::new(TrainingHook))
        .register("TrainingProgram", Arc::new(ProgramHook))
        .register("Employee", Arc::new(SkillLines))
        .register("Designation", Arc::new(SkillLines));
}

pub struct TrainingHook;

/// Skills held (on an employee) or needed (on a designation): one line per skill.
struct SkillLines;

#[async_trait]
impl DocHook for SkillLines {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let table = if hc.entity.def.name == "Designation" { "required_skills" } else { "skills" };
        let mut seen = HashSet::new();
        for (i, row) in rows(doc, table).iter().enumerate() {
            if !seen.insert(id(row, "skill")) {
                return Err(Error::Validation(vec![FieldError::new("skill", "is on this list twice").in_row(table, i)]));
            }
        }
        Ok(())
    }
}

/// A program's "required for" lines: each names someone, and a department of the program's company.
struct ProgramHook;

#[async_trait]
impl DocHook for ProgramHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let company = id(doc, "company");
        let mut errors = Vec::new();
        for (i, line) in rows(doc, "required_for").iter().enumerate() {
            if ["department", "designation", "branch", "employment_type"].iter().all(|f| s(line, f).is_none()) {
                errors.push(FieldError::new("department", "say who: a department, a designation, a branch or an employment type").in_row("required_for", i));
            } else if let Some(dept) = lookup(hc, "Department", id(line, "department")).await?
                && id(&dept, "company").is_some_and(|c| Some(c) != company)
            {
                errors.push(FieldError::new("department", "belongs to another company").in_row("required_for", i));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }
}

/// When completing a program on `held` stops counting, for one valid for `months`.
pub(crate) fn valid_until(held: chrono::NaiveDate, months: Option<i64>) -> Option<chrono::NaiveDate> {
    months.and_then(|m| held.checked_add_months(chrono::Months::new(u32::try_from(m).ok()?)))
}

#[async_trait]
impl DocHook for TrainingHook {
    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut seen = HashSet::new();
        for (i, row) in rows(doc, "attendees").iter().enumerate() {
            if !seen.insert(id(row, "employee")) {
                return Err(Error::Validation(vec![FieldError::new("employee", "is on this session twice").in_row("attendees", i)]));
            }
        }
        Ok(())
    }

    async fn before_submit(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if rows(doc, "attendees").iter().any(|r| s(r, "result").unwrap_or("Pending") == "Pending") {
            return Err(Error::invalid("mark how everyone did first"));
        }
        Ok(())
    }

    /// What was passed goes on each person's record; cancelling the session takes it off.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let sctx = sys(hc.ctx);
        let program = get(hc.kernel, &sctx, hc.tx, "TrainingProgram", id(doc, "program").unwrap_or_default()).await?;
        let months = program.get("valid_for_months").and_then(Value::as_i64);
        let held = s(doc, "held_on").and_then(|d| d.parse().ok());
        for row in rows(doc, "attendees").iter().filter(|r| s(r, "result") == Some("Passed")) {
            let record = json!({
                "employee": row.get("employee"), "program": doc.get("program"), "program_name": program.get("program_name"), "company": doc.get("company"),
                "session": doc.get("id"), "completed_on": doc.get("held_on"), "valid_until": held.and_then(|h| valid_until(h, months)).map(|d| d.to_string()),
                "source": format!("session:{}", id(doc, "id").unwrap_or_default()),
            });
            hc.kernel.insert_tx(&sctx, hc.tx, "TrainingRecord", record.as_object().cloned().unwrap_or_default()).await?;
        }
        Ok(())
    }

    /// Each record the session made goes through the record path (audited, a deleted event); one that cannot go
    /// fails the cancel, which leaves them all.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let session = id(doc, "id").ok_or_else(|| Error::internal("session without id"))?;
        let made = find(hc.kernel, hc.ctx, hc.tx, "TrainingRecord", vec![Filter::eq("session", session.to_string())], None, u32::MAX).await?;
        let ectx = elevated(hc.ctx);
        for r in made {
            hc.kernel.delete_tx(&ectx, hc.tx, "TrainingRecord", id(&r, "id").unwrap_or_default(), None).await?;
        }
        Ok(())
    }
}

/// `hr.employee.record_training`: another app, a process or an agent says `employee` completed a training. One record
/// per employee and `source`, whatever the key: a second call with the same source answers the first record. The
/// program of that name is used, or made in the employee's company. The kernel has locked the employee's row.
pub(crate) async fn record_training(hc: &mut HookCtx<'_>, employee: &Doc, params: &Value) -> Result<Value> {
    let text = |k: &str| params.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let name = text("program_name");
    if name.is_empty() || name.chars().count() > 140 {
        return Err(Error::field("program_name", "is required, at most 140 characters"));
    }
    let source = text("source");
    if source.is_empty() || source.chars().count() > 200 || source.chars().any(char::is_whitespace) {
        return Err(Error::field("source", "is required: at most 200 characters, without spaces"));
    }
    let date = |k: &str| -> Result<Option<chrono::NaiveDate>> {
        match text(k) {
            "" => Ok(None),
            v => ferrum_kernel::value::parse_date(v).map(Some).map_err(|_| Error::field(k, "is not a date")),
        }
    };
    let today = hc.ctx.today();
    let done = date("completed_on")?.unwrap_or(today);
    if done > today {
        return Err(Error::field("completed_on", "cannot be after today"));
    }
    let emp = id(employee, "id").ok_or_else(|| Error::internal("employee without id"))?;
    let mine = vec![Filter::eq("employee", emp.to_string()), Filter::eq("source", source)];
    if let Some(r) = find(hc.kernel, hc.ctx, hc.tx, "TrainingRecord", mine, None, 1).await?.first() {
        return Ok(json!({"training_record": r.get("id"), "program": r.get("program"), "created": false}));
    }
    // Two first uses of one new name at once would otherwise both make the program, and the second would fail.
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("hr:training_program:{}:{}", hc.ctx.tenant, name.to_lowercase()))
        .execute(&mut **hc.tx)
        .await?;
    let program = match find(hc.kernel, hc.ctx, hc.tx, "TrainingProgram", vec![Filter::eq("program_name", name)], None, 1).await?.into_iter().next() {
        Some(p) => p,
        None => {
            let p = json!({"program_name": name, "company": employee.get("company")});
            hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "TrainingProgram", p.as_object().cloned().unwrap_or_default()).await?
        }
    };
    let until = match date("valid_until")? {
        Some(u) if u < done => return Err(Error::field("valid_until", "cannot be before the day it was completed")),
        Some(u) => Some(u),
        None => valid_until(done, program.get("valid_for_months").and_then(Value::as_i64)),
    };
    let record = json!({
        "employee": emp, "program": program.get("id"), "program_name": name, "company": employee.get("company"),
        "completed_on": done.to_string(), "valid_until": until.map(|d| d.to_string()), "source": source,
    });
    let made = hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "TrainingRecord", record.as_object().cloned().unwrap_or_default()).await?;
    Ok(json!({"training_record": made.get("id"), "program": program.get("id"), "created": true}))
}

/// Maintenance: tells the holder (and, for a mandatory program, the HR managers) once before a training record lapses.
/// The mark and the sends commit together, and the mark is taken by an update only one run can win for a row.
pub(crate) async fn warn_training_expiring(k: &Kernel, ctx: &Ctx) -> Result<()> {
    if ctx.schema.entity("TrainingRecord").is_none() {
        return Ok(());
    }
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let due: Vec<(Uuid, bool)> = sqlx::query_as(
        "with due as (select r.id, coalesce(p.mandatory, false) mandatory from e_training_record r \
           join e_training_program p on p.tenant_id = r.tenant_id and p.id = r.program \
           where r.tenant_id = $1 and r.expiry_warned_on is null and r.valid_until is not null \
           and r.valid_until >= $2 and r.valid_until - coalesce(p.warn_days_before::int, 30) <= $2 \
           and exists (select 1 from e_employee e where e.tenant_id = r.tenant_id and e.id = r.employee and e.status = 'Active') \
           and not exists (select 1 from e_training_record n where n.tenant_id = r.tenant_id and n.employee = r.employee and n.program = r.program \
             and n.id <> r.id and (n.valid_until is null or n.valid_until > r.valid_until)) \
           order by r.valid_until, r.id limit 200 for update of r skip locked) \
         update e_training_record t set expiry_warned_on = $2 from due where t.tenant_id = $1 and t.id = due.id returning t.id, due.mandatory",
    )
    .bind(ctx.tenant)
    .bind(ctx.today())
    .fetch_all(&mut *tx)
    .await?;
    let sctx = sys(ctx);
    for (record, mandatory) in due {
        let doc = Value::Object(get(k, &sctx, &mut tx, "TrainingRecord", record).await?);
        let mut to: Vec<Recipient> = Vec::new();
        let specs: &[&str] = if mandatory { &["field:employee_user", "role:HR Manager"] } else { &["field:employee_user"] };
        for spec in specs {
            for r in messages::recipients(k, &mut tx, &sctx, "TrainingRecord", &doc, spec).await? {
                if !to.contains(&r) {
                    to.push(r);
                }
            }
        }
        let m = Message { template: "hr.training_expiring".into(), entity: Some("TrainingRecord".into()), record: Some(record), to, ..Default::default() };
        messages::send(k, &mut tx, &sctx, &m).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Maintenance: openings past their closing date, the workspace's day, close through the record path. At most 200 a
/// round; one that cannot close is logged and tried again next round.
pub(crate) async fn close_openings(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let due: Vec<Uuid> = sqlx::query_scalar("select id from e_job_opening where tenant_id = $1 and status = 'Open' and closes_on < $2 order by id limit 200")
        .bind(ctx.tenant)
        .bind(ctx.today())
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    for opening in due {
        let mut v = Doc::new();
        set(&mut v, "status", "Closed");
        if let Err(e) = k.update(ctx, "JobOpening", opening, v, None).await {
            tracing::warn!(tenant = %ctx.tenant, %opening, "job opening not closed: {e}");
        }
    }
    Ok(())
}

struct OpeningHook;

#[async_trait]
impl DocHook for OpeningHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "department", "Department").await
    }
}

/// A requisition, once submitted (after whatever approval the workspace set on it), opens its job.
struct RequisitionHook;

#[async_trait]
impl DocHook for RequisitionHook {
    /// The sign-in of the hiring manager's manager, which the package's approval policy asks (none: an HR Manager does).
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let boss = match lookup(hc, "Employee", id(doc, "hiring_manager")).await? {
            Some(m) => lookup(hc, "Employee", id(&m, "reports_to")).await?.and_then(|b| s(&b, "user").map(str::to_string)),
            None => None,
        };
        set(doc, "manager_approver", boss.map(Value::String).unwrap_or_default());
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "department", "Department").await
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut o = Doc::new();
        for f in ["job_title", "company", "designation", "department", "employment_type", "branch", "vacancies", "lower_range", "upper_range"] {
            o.insert(f.into(), doc.get(f).cloned().unwrap_or_default());
        }
        set(&mut o, "requisition", doc.get("id").cloned().unwrap_or_default());
        let opening = hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "JobOpening", o).await?;
        let mut v = Doc::new();
        v.insert("job_opening".into(), opening.get("id").cloned().unwrap_or_default());
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "JobRequisition", id(doc, "id").unwrap_or_default(), v).await?;
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(o) = lookup(hc, "JobOpening", id(doc, "job_opening")).await? {
            return Err(Error::invalid(format!("it opened job opening {}; close that instead", s(&o, "number").unwrap_or_default())));
        }
        Ok(())
    }
}

/// The first stage of `job` of `category` (any category when `None`): the job's own stages, else those for no job.
pub(crate) async fn first_stage(hc: &mut HookCtx<'_>, job: Option<Uuid>, category: Option<&str>) -> Result<Option<Uuid>> {
    if hc.ctx.schema.entity("JobStage").is_none() {
        return Ok(None);
    }
    Ok(sqlx::query_scalar(
        "select id from e_job_stage where tenant_id = $1 and ($3::text is null or category = $3) \
         and (job_opening = $2 or (job_opening is null and not exists (select 1 from e_job_stage o where o.tenant_id = $1 and o.job_opening = $2))) \
         order by coalesce(sequence, 0), stage_name limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(job)
    .bind(category)
    .fetch_optional(&mut **hc.tx)
    .await?)
}

/// An applicant starts at their job's first stage; each move is kept in their stage history, and a Rejected or
/// Hired stage (by category, never by name) rejects or accepts them.
struct ApplicantHook;

#[async_trait]
impl DocHook for ApplicantHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if hc.entity.field("stage").is_none() {
            return Ok(());
        }
        let old = stored(hc, doc).await?;
        if old.is_none()
            && id(doc, "stage").is_none()
            && let Some(first) = first_stage(hc, id(doc, "job_opening"), None).await?
        {
            set(doc, "stage", first.to_string());
        }
        let mut history = old.as_ref().map(|o| rows(o, "stage_history")).unwrap_or_default();
        let moved = old.as_ref().is_none_or(|o| id(o, "stage") != id(doc, "stage"));
        if moved && let Some(stage) = lookup(hc, "JobStage", id(doc, "stage")).await? {
            let category = s(&stage, "category").unwrap_or_default().to_string();
            match category.as_str() {
                "Rejected" => set(doc, "status", "Rejected"),
                "Hired" => set(doc, "status", "Accepted"),
                _ => {}
            }
            let now = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>("select now()").fetch_one(&mut **hc.tx).await?;
            history.push(
                json!({"id": uuid::Uuid::now_v7().to_string(), "idx": history.len(), "stage": stage.get("id"), "category": category,
                       "entered_on": now.to_rfc3339(), "moved_by": hc.ctx.email})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            );
        }
        set(doc, "stage_history", Value::Array(history.into_iter().map(Value::Object).collect()));
        Ok(())
    }
}

struct InterviewHook;

#[async_trait]
impl DocHook for InterviewHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let feedback = rows(doc, "feedback");
        let ratings: Vec<i64> = feedback.iter().filter_map(|f| f.get("rating").and_then(Value::as_i64)).collect();
        let average = (!ratings.is_empty()).then(|| (Decimal::from(ratings.iter().sum::<i64>()) / Decimal::from(ratings.len() as u64)).round_dp(2));
        set(doc, "average_rating", average.map(money).unwrap_or_default());
        if !feedback.is_empty() && s(doc, "status") == Some("Pending") {
            set(doc, "status", "Under Review");
        }
        // The scorecard weighted by the job's criteria (each 1 when the job names none).
        let scores = rows(doc, "scores");
        if hc.entity.field("weighted_score").is_some() {
            let job = match id(doc, "job_opening") {
                Some(j) => lookup(hc, "JobOpening", Some(j)).await?,
                None => match lookup(hc, "JobApplicant", id(doc, "job_applicant")).await? {
                    Some(a) => lookup(hc, "JobOpening", id(&a, "job_opening")).await?,
                    None => None,
                },
            };
            let weights: std::collections::HashMap<String, Decimal> = job
                .map(|j| rows(&j, "criteria"))
                .unwrap_or_default()
                .iter()
                .filter_map(|c| Some((s(c, "criterion")?.to_lowercase(), d(c, "weight"))))
                .collect();
            let (mut sum, mut total) = (Decimal::ZERO, Decimal::ZERO);
            for r in &scores {
                let w = weights.get(&s(r, "criterion").unwrap_or_default().to_lowercase()).copied().unwrap_or(Decimal::ONE);
                sum += d(r, "rating") * w;
                total += w;
            }
            set(doc, "weighted_score", if total.is_zero() { Value::Null } else { json!(money((sum / total).round_dp(2))) });
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
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
        // Scores: from the panel, on the job's criteria when it names some, one per interviewer and criterion.
        let job = lookup(hc, "JobOpening", id(doc, "job_opening")).await?;
        let criteria: HashSet<String> =
            job.map(|j| rows(&j, "criteria")).unwrap_or_default().iter().filter_map(|c| s(c, "criterion").map(str::to_lowercase)).collect();
        let mut scored = HashSet::new();
        for (i, r) in rows(doc, "scores").iter().enumerate() {
            let who = s(r, "interviewer").unwrap_or_default().to_lowercase();
            let what = s(r, "criterion").unwrap_or_default().to_lowercase();
            if !panel.contains(&who) {
                errors.push(FieldError::new("interviewer", "is not on this interview's panel").in_row("scores", i));
            } else if !criteria.is_empty() && !criteria.contains(&what) {
                errors.push(FieldError::new("criterion", "is not one of the job's criteria").in_row("scores", i));
            } else if !scored.insert((who, what)) {
                errors.push(FieldError::new("criterion", "was already scored by this interviewer").in_row("scores", i));
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
            None => opt_date(doc, "expected_joining_date").unwrap_or_else(|| hc.ctx.today()),
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
        // The applicant moves to their job's Hired stage, when it has one.
        if let Some(hired) = first_stage(hc, id(doc, "job_opening"), Some("Hired")).await? {
            let mut v = Doc::new();
            set(&mut v, "stage", hired.to_string());
            hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, "JobApplicant", id(&app, "id").unwrap_or_default(), v, None).await?;
        }
        // Their onboarding, when a checklist template fits them: its tasks are copied in as it is made.
        if let Some(template) = crate::people::choose_template(hc, "Onboarding", &created).await? {
            let mut o = Doc::new();
            for f in
                ["employee", "company", "department", "designation", "date_of_joining", "job_applicant", "job_offer", "employee_name", "checklist_template"]
            {
                let v = match f {
                    "employee" => created.get("id").cloned(),
                    "job_applicant" => doc.get("job_applicant").cloned(),
                    "job_offer" => doc.get("id").cloned(),
                    "checklist_template" => Some(json!(template)),
                    _ => created.get(f).cloned(),
                };
                o.insert(f.into(), v.unwrap_or_default());
            }
            hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "EmployeeOnboarding", o).await?;
        }
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
        // The page opens the new employee, saying who joins when.
        let message = format!(
            "Employee {} is made: {} joins on {}",
            s(&created, "number").unwrap_or_default(),
            s(&created, "employee_name").unwrap_or_default(),
            s(&created, "date_of_joining").unwrap_or_default()
        );
        let mut out = created;
        out.insert("entity".into(), json!("Employee"));
        out.insert("message".into(), json!(message));
        Ok(Value::Object(out))
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

/// Whether `a` and `b` hold the same value as a form sends it: empty, null and absent are one, and numbers are
/// compared as decimals ("4", 4 and "4.00").
fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    let empty = |v: Option<&Value>| v.is_none_or(|v| v.is_null() || v.as_str() == Some(""));
    if empty(a) || empty(b) {
        return empty(a) && empty(b);
    }
    let num = |v: Option<&Value>| v.and_then(|v| ferrum_kernel::value::parse_decimal(v).ok());
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Who may change what on an appraisal at its stage (old = the stored one). HR (full access) changes anything; the
/// employee only their self scores and comments at Self Review; the reviewer only manager scores, comments and remarks
/// at Manager Review. Nobody else, and only HR and the cycle's actions make appraisals.
fn turn(hc: &HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
    if crate::full_access(hc.ctx, "Appraisal", &[Action::Write]) {
        return Ok(());
    }
    let Some(old) = old else { return Err(Error::Forbidden("appraisals are made by HR or by starting the cycle".into())) };
    let stage = s(old, "stage").unwrap_or("Manager Review");
    let me = |f: &str| s(old, f).is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email));
    let not_yours = || Error::Forbidden("it is not your turn on this appraisal".into());
    let (goal_fields, own_fields): (&[&str], &[&str]) = if me("employee_user") {
        if stage != "Self Review" {
            return Err(not_yours());
        }
        (&["self_score", "self_comments"], &[])
    } else if me("reviewer") && stage == "Manager Review" {
        (&["manager_score", "manager_comments"], &["remarks"])
    } else {
        return Err(not_yours());
    };
    let changed = |f: &str| !same(doc.get(f), old.get(f));
    let def = &hc.entity.def;
    if def.fields.iter().any(|f| !f.read_only && f.name != "goals" && !own_fields.contains(&f.name.as_str()) && changed(&f.name)) {
        return Err(not_yours());
    }
    let (new_goals, old_goals) = (rows(doc, "goals"), rows(old, "goals"));
    if new_goals.iter().map(|g| s(g, "id")).ne(old_goals.iter().map(|g| s(g, "id"))) {
        return Err(Error::Forbidden("only HR adds, removes or reorders goals".into()));
    }
    let goal_def = hc.ctx.schema.entity("AppraisalGoal").ok_or_else(|| Error::internal("no AppraisalGoal"))?;
    for (n, o) in new_goals.iter().zip(&old_goals) {
        if goal_def.def.fields.iter().any(|f| !f.read_only && !goal_fields.contains(&f.name.as_str()) && !same(n.get(&f.name), o.get(&f.name))) {
            return Err(not_yours());
        }
    }
    Ok(())
}

/// The cycle's status, and a refusal when it takes no work (`new`: an appraisal being made).
async fn cycle_open(hc: &mut HookCtx<'_>, doc: &Doc, new: bool) -> Result<()> {
    let status = lookup(hc, "AppraisalCycle", id(doc, "appraisal_cycle")).await?.and_then(|c| s(&c, "status").map(str::to_string));
    match status.as_deref() {
        Some("Completed") => Err(Error::field("appraisal_cycle", "this cycle is completed")),
        Some("Not Started") if new => Err(Error::field("appraisal_cycle", "start the cycle first")),
        _ => Ok(()),
    }
}

/// Every goal of `doc` has `field`, else the goals without it are refused.
fn scored(doc: &Doc, field: &str) -> Result<()> {
    let missing: Vec<FieldError> = rows(doc, "goals")
        .iter()
        .enumerate()
        .filter(|(_, g)| g.get(field).is_none_or(Value::is_null))
        .map(|(i, _)| FieldError::new(field, "score every goal first").in_row("goals", i))
        .collect();
    if rows(doc, "goals").is_empty() {
        return Err(Error::field("goals", "add the goals to appraise"));
    }
    if missing.is_empty() { Ok(()) } else { Err(Error::Validation(missing)) }
}

#[async_trait]
impl DocHook for AppraisalHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        // The stage moves only through the actions and submit: a save keeps the stored one.
        let old = stored(hc, doc).await?;
        match &old {
            Some(o) => {
                for f in ["stage", "self_submitted_on", "manager_submitted_on"] {
                    set(doc, f, o.get(f).cloned().unwrap_or_default());
                }
            }
            None => {
                set(doc, "stage", "Self Review");
                set(doc, "self_submitted_on", Value::Null);
                set(doc, "manager_submitted_on", Value::Null);
            }
        }
        // The reviewer: HR may name one; anyone else keeps what is stored, or the employee's manager.
        let full = crate::full_access(hc.ctx, "Appraisal", &[Action::Write]);
        let kept = if full { s(doc, "reviewer").map(str::to_string) } else { old.as_ref().and_then(|o| s(o, "reviewer").map(str::to_string)) };
        let reviewer = match kept {
            Some(r) => Some(r),
            None => match lookup(hc, "Employee", id(doc, "employee")).await? {
                Some(e) => lookup(hc, "Employee", id(&e, "reports_to")).await?.and_then(|m| s(&m, "user").map(str::to_string)),
                None => None,
            },
        };
        set(doc, "reviewer", reviewer.map(Value::String).unwrap_or_default());
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
        let old = stored(hc, doc).await?;
        cycle_open(hc, doc, old.is_none()).await?;
        turn(hc, doc, old.as_ref())?;
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

    /// A new appraisal tells its employee to start.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_none() {
            tell(hc, doc, "hr.appraisal_opened", &["field:employee_user"], Default::default()).await?;
        }
        Ok(())
    }

    async fn before_submit(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if s(doc, "stage").unwrap_or("Manager Review") != "Sign-off" {
            return Err(Error::invalid("the manager's scores come first: it is signed off once it reaches Sign-off"));
        }
        scored(doc, "manager_score")
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("appraisal without id"))?;
        let mut v = Doc::new();
        set(&mut v, "stage", "Done");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "Appraisal", me, v).await?;
        tell(hc, doc, "hr.appraisal_done", &["field:employee_user"], Default::default()).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if !["submit_self_review", "complete_manager_review", "send_back"].contains(&name) {
            return Ok(None);
        }
        cycle_open(hc, doc, false).await?;
        let me = id(doc, "id").ok_or_else(|| Error::internal("appraisal without id"))?;
        let full = crate::full_access(hc.ctx, "Appraisal", &[Action::Write]);
        let is = |f: &str| s(doc, f).is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email));
        let (is_self, is_reviewer) = (is("employee_user"), !is("employee_user") && is("reviewer"));
        let stage = s(doc, "stage").unwrap_or("Manager Review").to_string();
        let now = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>("select now()").fetch_one(&mut **hc.tx).await?.to_rfc3339();
        let no_reviewer = s(doc, "reviewer").is_none();
        let (next, stamp, template, to, message): (&str, Option<&str>, &str, &str, Option<&str>) = match name {
            "submit_self_review" => {
                if !(full || is_self) {
                    return Err(Error::Forbidden("the employee sends their own scores on".into()));
                }
                scored(doc, "self_score")?;
                if no_reviewer {
                    (
                        "Manager Review",
                        Some("self_submitted_on"),
                        "hr.appraisal_to_sign_off",
                        "role:HR Manager",
                        Some("Nobody is named to review it: an HR manager scores it"),
                    )
                } else {
                    ("Manager Review", Some("self_submitted_on"), "hr.appraisal_to_review", "field:reviewer", None)
                }
            }
            "complete_manager_review" => {
                if !(full || is_reviewer) {
                    return Err(Error::Forbidden("the manager, or HR, sends it for sign-off".into()));
                }
                scored(doc, "manager_score")?;
                ("Sign-off", Some("manager_submitted_on"), "hr.appraisal_to_sign_off", "role:HR Manager", None)
            }
            _ => match stage.as_str() {
                "Manager Review" if full || is_reviewer => ("Self Review", None, "hr.appraisal_sent_back", "field:employee_user", None),
                "Sign-off" if full => {
                    if no_reviewer {
                        ("Manager Review", None, "hr.appraisal_sent_back", "role:HR Manager", None)
                    } else {
                        ("Manager Review", None, "hr.appraisal_sent_back", "field:reviewer", None)
                    }
                }
                _ => return Err(Error::Forbidden("it is not yours to send back at this stage".into())),
            },
        };
        let mut v = Doc::new();
        set(&mut v, "stage", next);
        if let Some(f) = stamp {
            set(&mut v, f, now);
        }
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "Appraisal", me, v).await?;
        let mut values = serde_json::Map::new();
        if name == "send_back" {
            values.insert("note".into(), params.get("note").cloned().unwrap_or_default());
        }
        tell(hc, doc, template, &[to], values).await?;
        let mut out = json!({"stage": next});
        if let Some(m) = message {
            out["message"] = json!(m);
        }
        Ok(Some(out))
    }
}

// ───────────────────────────── appraisal cycles and stages ─────────────────────────────

/// At most this many appraisals are made by one Start or Create Appraisals; the rest wait for the next call.
const APPRAISALS_PER_CALL: usize = 500;

/// A message template on an appraisal, to the recipients of `specs` (`field:…`, `role:…`).
async fn tell(hc: &mut HookCtx<'_>, doc: &Doc, template: &str, specs: &[&str], values: serde_json::Map<String, Value>) -> Result<()> {
    let sctx = sys(hc.ctx);
    let me = id(doc, "id").ok_or_else(|| Error::internal("appraisal without id"))?;
    let value = Value::Object(doc.clone());
    let mut to: Vec<Recipient> = Vec::new();
    for spec in specs {
        for r in messages::recipients(hc.kernel, hc.tx, &sctx, "Appraisal", &value, spec).await? {
            if !to.contains(&r) {
                to.push(r);
            }
        }
    }
    let m = Message { template: template.into(), entity: Some("Appraisal".into()), record: Some(me), to, values, ..Default::default() };
    messages::send(hc.kernel, hc.tx, &sctx, &m).await?;
    Ok(())
}

/// A cycle's status is set by its actions: Start makes everyone's appraisal, Create Appraisals the missing ones,
/// Close ends it once everything in it is signed off.
struct CycleHook;

#[async_trait]
impl DocHook for CycleHook {
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        let cycle = id(doc, "id").ok_or_else(|| Error::internal("cycle without id"))?;
        match name {
            "start" | "create_appraisals" => {
                // In Progress first: an appraisal is made only in a started cycle.
                if name == "start" {
                    let mut v = Doc::new();
                    set(&mut v, "status", "In Progress");
                    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "AppraisalCycle", cycle, v).await?;
                }
                let mut out = make_appraisals(hc, doc, params).await?;
                if name == "start" {
                    out["status"] = json!("In Progress");
                }
                Ok(Some(out))
            }
            "close" => {
                let open: i64 = sqlx::query_scalar("select count(*) from e_appraisal where tenant_id = $1 and appraisal_cycle = $2 and docstatus = 0")
                    .bind(hc.ctx.tenant)
                    .bind(cycle)
                    .fetch_one(&mut **hc.tx)
                    .await?;
                if open > 0 {
                    return Err(Error::invalid(format!("{open} appraisals are not signed off: sign them off or delete them")));
                }
                let mut v = Doc::new();
                set(&mut v, "status", "Completed");
                hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "AppraisalCycle", cycle, v).await?;
                Ok(Some(json!({"status": "Completed"})))
            }
            _ => Ok(None),
        }
    }
}

/// An appraisal for each active employee the cycle covers who has none in it, up to [`APPRAISALS_PER_CALL`].
async fn make_appraisals(hc: &mut HookCtx<'_>, cycle: &Doc, params: &Value) -> Result<Value> {
    let me = id(cycle, "id").ok_or_else(|| Error::internal("cycle without id"))?;
    let company = id(cycle, "company");
    let param = |k: &str| params.get(k).and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok());
    let department = param("department");
    if let Some(d) = lookup(hc, "Department", department).await?
        && id(&d, "company") != company
    {
        return Err(Error::field("department", "belongs to another company"));
    }
    let (template, end) = (param("appraisal_template"), opt_date(cycle, "end_date"));
    let (todo, skipped): (Vec<Uuid>, i64) = {
        let todo: Vec<Uuid> = sqlx::query_scalar(
            "select e.id from e_employee e where e.tenant_id = $1 and e.company = $2 and e.status = 'Active' \
             and ($3::date is null or e.date_of_joining <= $3) and ($4::uuid is null or e.department = $4) \
             and not exists (select 1 from e_appraisal a where a.tenant_id = e.tenant_id and a.employee = e.id and a.appraisal_cycle = $5 and a.docstatus < 2) \
             order by e.id limit $6",
        )
        .bind(hc.ctx.tenant)
        .bind(company)
        .bind(end)
        .bind(department)
        .bind(me)
        .bind(APPRAISALS_PER_CALL as i64 + 1)
        .fetch_all(&mut **hc.tx)
        .await?;
        let skipped: i64 = sqlx::query_scalar(
            "select count(*) from e_employee e where e.tenant_id = $1 and e.company = $2 and e.status = 'Active' \
             and ($3::date is null or e.date_of_joining <= $3) and ($4::uuid is null or e.department = $4) \
             and exists (select 1 from e_appraisal a where a.tenant_id = e.tenant_id and a.employee = e.id and a.appraisal_cycle = $5 and a.docstatus < 2)",
        )
        .bind(hc.ctx.tenant)
        .bind(company)
        .bind(end)
        .bind(department)
        .bind(me)
        .fetch_one(&mut **hc.tx)
        .await?;
        (todo, skipped)
    };
    let remaining = usize::from(todo.len() > APPRAISALS_PER_CALL);
    let ectx = elevated(hc.ctx);
    let mut created = 0;
    for emp in todo.into_iter().take(APPRAISALS_PER_CALL) {
        let mut a = Doc::new();
        set(&mut a, "employee", emp.to_string());
        set(&mut a, "appraisal_cycle", me.to_string());
        if let Some(t) = template {
            set(&mut a, "appraisal_template", t.to_string());
        }
        hc.kernel.insert_tx(&ectx, hc.tx, "Appraisal", a).await?;
        created += 1;
    }
    Ok(json!({"created": created, "skipped": skipped, "remaining": remaining}))
}

#[cfg(test)]
mod training_tests {
    use super::valid_until;

    #[test]
    fn what_counts_until_when() {
        let d = |s: &str| s.parse::<chrono::NaiveDate>().unwrap();
        assert_eq!(valid_until(d("2026-01-31"), Some(1)), Some(d("2026-02-28")));
        assert_eq!(valid_until(d("2026-01-31"), Some(12)), Some(d("2027-01-31")));
        assert_eq!(valid_until(d("2026-01-31"), None), None, "for good");
    }
}
