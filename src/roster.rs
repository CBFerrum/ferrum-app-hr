//! Rosters (plans of who works which shift, published in one operation) and same-day shift swaps between two
//! colleagues, accepted by the colleague and approved by the approver.

use crate::people::same_company;
use crate::{default_approver, full_access, lookup, opt_date, require_approver, stamp_company, stored};
use async_trait::async_trait;
use chrono::NaiveDate;
use ferrum_domain::util::{elevated, id, s, set, sys};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Action, Doc, DocHook, Error, HookCtx, HookRegistry, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// The most days a roster covers, the most people one fills, and about how many assignments one publish submits.
const ROSTER_DAYS: i64 = 62;
const FILL_PEOPLE: usize = 1000;
const PUBLISH_BATCH: usize = 500;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("ShiftRoster", Arc::new(RosterHook)).register("ShiftSwapRequest", Arc::new(SwapHook));
}

/// The shift an employee works on `day`: their latest submitted assignment covering it, else their default shift
/// (the rule `mark_day` applies in SQL).
pub(crate) async fn shift_on(hc: &mut HookCtx<'_>, employee: Uuid, day: NaiveDate) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "select coalesce((select sa.shift_type from e_shift_assignment sa where sa.tenant_id = $1 and sa.employee = $2 and sa.docstatus = 1 \
           and sa.start_date <= $3 and (sa.end_date is null or sa.end_date >= $3) order by sa.start_date desc limit 1), \
           (select e.default_shift from e_employee e where e.tenant_id = $1 and e.id = $2))",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(day)
    .fetch_one(&mut **hc.tx)
    .await?)
}

/// A message template to the recipients `specs` name on `doc` (of `entity`).
pub(crate) async fn tell(hc: &mut HookCtx<'_>, entity: &str, doc: &Doc, template: &str, specs: &[&str], values: serde_json::Map<String, Value>) -> Result<()> {
    let sctx = sys(hc.ctx);
    let value = Value::Object(doc.clone());
    let mut to: Vec<Recipient> = Vec::new();
    for spec in specs {
        for r in messages::recipients(hc.kernel, hc.tx, &sctx, entity, &value, spec).await? {
            if !to.contains(&r) {
                to.push(r);
            }
        }
    }
    let m = Message { template: template.into(), entity: Some(entity.into()), record: id(doc, "id"), to, values, ..Default::default() };
    messages::send(hc.kernel, hc.tx, &sctx, &m).await?;
    Ok(())
}

async fn now(hc: &mut HookCtx<'_>) -> Result<String> {
    Ok(sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>("select now()").fetch_one(&mut **hc.tx).await?.to_rfc3339())
}

// ───────────────────────────── rosters ─────────────────────────────

struct RosterHook;

/// The people of a roster: active employees of its company (its department and branch when set) employed on at least
/// one of its days, by id.
async fn people_of(hc: &mut HookCtx<'_>, roster: &Doc, limit: usize) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "select e.id from e_employee e where e.tenant_id = $1 and e.company = $2 and e.status = 'Active' \
         and ($3::uuid is null or e.department = $3) and ($4::uuid is null or e.branch = $4) \
         and e.date_of_joining <= $6 and (e.relieving_date is null or e.relieving_date >= $5) order by e.id limit $7",
    )
    .bind(hc.ctx.tenant)
    .bind(id(roster, "company"))
    .bind(id(roster, "department"))
    .bind(id(roster, "branch"))
    .bind(opt_date(roster, "from_date"))
    .bind(opt_date(roster, "to_date"))
    .bind(limit as i64)
    .fetch_all(&mut **hc.tx)
    .await?)
}

/// A date parameter, refused on its name when it is not a date.
fn date_param(params: &Value, k: &str) -> Result<Option<NaiveDate>> {
    match params.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => ferrum_kernel::value::parse_date(&v[..v.len().min(10)]).map(Some).map_err(|_| Error::field(k, "is not a date")),
    }
}

#[async_trait]
impl DocHook for RosterHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if s(doc, "status").is_none() {
            set(doc, "status", "Draft");
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        same_company(hc, doc, "department", "Department").await?;
        if let (Some(from), Some(to)) = (opt_date(doc, "from_date"), opt_date(doc, "to_date"))
            && (to - from).num_days() >= ROSTER_DAYS
        {
            return Err(Error::field("to_date", format!("a roster covers at most {ROSTER_DAYS} days; make another for the rest")));
        }
        if let Some(old) = stored(hc, doc).await? {
            let planned: bool = sqlx::query_scalar("select exists(select 1 from e_shift_assignment where tenant_id = $1 and roster = $2)")
                .bind(hc.ctx.tenant)
                .bind(id(doc, "id"))
                .fetch_one(&mut **hc.tx)
                .await?;
            if planned && let Some(f) = ["company", "department", "branch", "from_date", "to_date"].into_iter().find(|f| s(doc, f) != s(&old, f)) {
                return Err(Error::field(f, "cannot change once shifts are planned in this roster"));
            }
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "fill" => fill(hc, doc, params).await.map(Some),
            "publish" => publish(hc, doc).await.map(Some),
            _ => Ok(None),
        }
    }
}

async fn fill(hc: &mut HookCtx<'_>, roster: &Doc, params: &Value) -> Result<Value> {
    let me = id(roster, "id").ok_or_else(|| Error::internal("roster without id"))?;
    let (first, last) = (opt_date(roster, "from_date"), opt_date(roster, "to_date"));
    let (Some(first), Some(last)) = (first, last) else { return Err(Error::field("from_date", "the roster needs its days")) };
    let from = date_param(params, "from_date")?.unwrap_or(first);
    let to = date_param(params, "to_date")?.unwrap_or(last);
    if from < first || from > last {
        return Err(Error::field("from_date", "must be one of the roster's days"));
    }
    if to < from || to > last {
        return Err(Error::field("to_date", "must be one of the roster's days, not before From"));
    }
    let shift = params.get("shift_type").and_then(Value::as_str).ok_or_else(|| Error::field("shift_type", "is required"))?.to_string();
    let mut people = people_of(hc, roster, FILL_PEOPLE + 1).await?;
    if people.len() > FILL_PEOPLE {
        return Err(Error::invalid(format!("more than {FILL_PEOPLE} people: narrow the roster to a department or a branch")));
    }
    if let Some(one) = params.get("employee").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()) {
        if !people.contains(&one) {
            return Err(Error::field("employee", "is not one of this roster's people"));
        }
        people = vec![one];
    }
    let busy: Vec<Uuid> = sqlx::query_scalar(
        "select distinct employee from e_shift_assignment where tenant_id = $1 and employee = any($2) and docstatus < 2 \
         and daterange(start_date, end_date, '[]') && daterange($3, $4, '[]')",
    )
    .bind(hc.ctx.tenant)
    .bind(&people)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **hc.tx)
    .await?;
    let ectx = elevated(hc.ctx);
    let (mut created, mut skipped) = (0, 0);
    for emp in people {
        if busy.contains(&emp) {
            skipped += 1;
            continue;
        }
        let a = json!({"employee": emp, "shift_type": shift, "start_date": from.to_string(), "end_date": to.to_string(), "roster": me});
        hc.kernel.insert_tx(&ectx, hc.tx, "ShiftAssignment", a.as_object().cloned().unwrap_or_default()).await?;
        created += 1;
    }
    Ok(json!({"created": created, "skipped": skipped}))
}

async fn publish(hc: &mut HookCtx<'_>, roster: &Doc) -> Result<Value> {
    hc.ctx.require("ShiftAssignment", Action::Submit)?;
    let me = id(roster, "id").ok_or_else(|| Error::internal("roster without id"))?;
    let drafts: Vec<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "select a.id, a.employee, e.employee_name from e_shift_assignment a join e_employee e on e.tenant_id = a.tenant_id and e.id = a.employee \
         where a.tenant_id = $1 and a.roster = $2 and a.docstatus = 0 order by a.employee, a.start_date, a.id",
    )
    .bind(hc.ctx.tenant)
    .bind(me)
    .fetch_all(&mut **hc.tx)
    .await?;
    if drafts.is_empty() {
        return Err(Error::invalid("nothing to publish"));
    }
    // Whole people, in employee order (the lock order), until the batch is full.
    let mut take = 0;
    while take < drafts.len() && (take < PUBLISH_BATCH || drafts[take].1 == drafts[take - 1].1) {
        take += 1;
    }
    let ectx = elevated(hc.ctx);
    let mut people: Vec<Uuid> = Vec::new();
    for (a, emp, name) in &drafts[..take] {
        if let Err(e) = hc.kernel.submit_tx(&ectx, hc.tx, "ShiftAssignment", *a, None).await {
            let who = name.clone().unwrap_or_default();
            return Err(match e {
                Error::Validation(f) => Error::invalid(format!("{who}: {}", f.iter().map(|x| x.message.clone()).collect::<Vec<_>>().join("; "))),
                other => Error::invalid(format!("{who}: {other}")),
            });
        }
        if !people.contains(emp) {
            people.push(*emp);
        }
    }
    let sctx = sys(hc.ctx);
    let mut to: Vec<Recipient> = Vec::new();
    for emp in &people {
        let e = hc.kernel.get_tx(&sctx, hc.tx, "Employee", *emp, false).await?;
        for r in messages::recipients(hc.kernel, hc.tx, &sctx, "Employee", &Value::Object(e), "field:user").await? {
            if !to.contains(&r) {
                to.push(r);
            }
        }
    }
    if !to.is_empty() {
        let m = Message { template: "hr.roster_published".into(), entity: Some("ShiftRoster".into()), record: Some(me), to, ..Default::default() };
        messages::send(hc.kernel, hc.tx, &sctx, &m).await?;
    }
    let remaining = drafts.len() - take;
    if remaining == 0 {
        let mut v = Doc::new();
        set(&mut v, "status", "Published");
        set(&mut v, "published_on", now(hc).await?);
        set(&mut v, "published_by", hc.ctx.email.as_str());
        hc.kernel.set_values_tx(&sctx, hc.tx, "ShiftRoster", me, v).await?;
    }
    Ok(json!({"published": take, "remaining": remaining}))
}

// ───────────────────────────── shift swaps ─────────────────────────────

struct SwapHook;

/// `hr.shift_swap_decided` to whom an outcome concerns.
async fn decided(hc: &mut HookCtx<'_>, doc: &Doc, outcome: &str) -> Result<()> {
    let mut values = serde_json::Map::new();
    values.insert("outcome".into(), json!(outcome));
    let to: &[&str] = if outcome == "declined" { &["field:employee_user"] } else { &["field:employee_user", "field:with_employee_user"] };
    tell(hc, "ShiftSwapRequest", doc, "hr.shift_swap_decided", to, values).await
}

/// Whether either person has attendance marked on the day.
async fn marked(hc: &mut HookCtx<'_>, people: [Uuid; 2], day: NaiveDate) -> Result<bool> {
    Ok(sqlx::query_scalar("select exists(select 1 from e_attendance where tenant_id = $1 and employee = any($2) and attendance_date = $3 and docstatus < 2)")
        .bind(hc.ctx.tenant)
        .bind(&people[..])
        .bind(day)
        .fetch_one(&mut **hc.tx)
        .await?)
}

/// The two people of a request and its day, their employee rows locked in ascending id order.
async fn lock_pair(hc: &mut HookCtx<'_>, req: &Doc) -> Result<(Uuid, Uuid, NaiveDate)> {
    let (Some(a), Some(b), Some(day)) = (id(req, "employee"), id(req, "with_employee"), opt_date(req, "swap_date")) else {
        return Err(Error::internal("swap request without its people or day"));
    };
    let sctx = sys(hc.ctx);
    for e in [a.min(b), a.max(b)] {
        hc.kernel.get_tx(&sctx, hc.tx, "Employee", e, true).await?;
    }
    Ok((a, b, day))
}

/// One day of a person's submitted shifts becomes `shift`: an assignment covering it is cancelled and its other days
/// put back around the day, then a one-day assignment for the day is submitted. The employee row is locked.
async fn set_day(hc: &mut HookCtx<'_>, emp: Uuid, day: NaiveDate, shift: Uuid, swap: Option<Uuid>, roster: Option<Uuid>) -> Result<()> {
    let sctx = sys(hc.ctx);
    let cover: Option<(Uuid, Uuid, NaiveDate, Option<NaiveDate>, Option<Uuid>)> = sqlx::query_as(
        "select id, shift_type, start_date, end_date, roster from e_shift_assignment where tenant_id = $1 and employee = $2 and docstatus = 1 \
         and start_date <= $3 and (end_date is null or end_date >= $3) order by start_date desc limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(emp)
    .bind(day)
    .fetch_optional(&mut **hc.tx)
    .await?;
    let mut roster = roster;
    let put = |v: Value| v.as_object().cloned().unwrap_or_default();
    let mut pieces = Vec::new();
    if let Some((old, kind, start, end, from_roster)) = cover {
        hc.kernel.cancel_tx(&sctx, hc.tx, "ShiftAssignment", old, None).await?;
        roster = roster.or(from_roster);
        if start < day {
            pieces.push(put(json!({"employee": emp, "shift_type": kind, "start_date": start.to_string(), "end_date": (day - chrono::Duration::days(1)).to_string(), "roster": from_roster})));
        }
        if end.is_none_or(|e| e > day) {
            pieces.push(put(json!({"employee": emp, "shift_type": kind, "start_date": (day + chrono::Duration::days(1)).to_string(), "end_date": end.map(|e| e.to_string()), "roster": from_roster})));
        }
    }
    pieces.push(put(
        json!({"employee": emp, "shift_type": shift, "start_date": day.to_string(), "end_date": day.to_string(), "roster": roster, "swap_request": swap}),
    ));
    for p in pieces {
        let made = hc.kernel.insert_tx(&sctx, hc.tx, "ShiftAssignment", p).await?;
        hc.kernel.submit_tx(&sctx, hc.tx, "ShiftAssignment", id(&made, "id").unwrap_or_default(), None).await?;
    }
    Ok(())
}

/// Approval: both people's shifts on the day are exchanged, or nothing changes.
async fn swap(hc: &mut HookCtx<'_>, req: &Doc) -> Result<()> {
    let me = id(req, "id");
    let (a, b, day) = lock_pair(hc, req).await?;
    if day < hc.ctx.today() {
        return Err(Error::invalid("the day has passed"));
    }
    let (sa, sb) = (shift_on(hc, a, day).await?, shift_on(hc, b, day).await?);
    if sa != id(req, "shift_type") || sb != id(req, "with_shift_type") {
        return Err(Error::invalid("the shifts changed since this was asked; ask again"));
    }
    if marked(hc, [a, b], day).await? {
        return Err(Error::invalid("attendance is already marked for that day"));
    }
    let (Some(sa), Some(sb)) = (sa, sb) else { return Err(Error::invalid("one of them has no shift that day")) };
    set_day(hc, a, day, sb, me, None).await?;
    set_day(hc, b, day, sa, me, None).await
}

/// Cancelling an approved swap: each person's one-day shift from it goes back to what they had.
async fn undo(hc: &mut HookCtx<'_>, req: &Doc) -> Result<()> {
    let me = id(req, "id").ok_or_else(|| Error::internal("swap request without id"))?;
    let (a, b, day) = lock_pair(hc, req).await?;
    if day < hc.ctx.today() || marked(hc, [a, b], day).await? {
        return Err(Error::invalid("the day has passed or is marked; set the shifts by hand"));
    }
    let sctx = sys(hc.ctx);
    for (person, original) in [(a, id(req, "shift_type")), (b, id(req, "with_shift_type"))] {
        let one: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "select id, roster from e_shift_assignment where tenant_id = $1 and employee = $2 and swap_request = $3 and docstatus = 1 \
             and start_date = $4 and end_date = $4",
        )
        .bind(hc.ctx.tenant)
        .bind(person)
        .bind(me)
        .bind(day)
        .fetch_optional(&mut **hc.tx)
        .await?;
        let (Some((one, roster)), Some(original)) = (one, original) else {
            return Err(Error::invalid("the roster changed since; set the shifts by hand"));
        };
        hc.kernel.cancel_tx(&sctx, hc.tx, "ShiftAssignment", one, None).await?;
        set_day(hc, person, day, original, None, roster).await?;
    }
    Ok(())
}

#[async_trait]
impl DocHook for SwapHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let emp = stamp_company(hc, doc).await?;
        let draft = doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0;
        if draft {
            set(doc, "status", "Open");
        }
        if let Some(emp) = &emp
            && (s(doc, "approver").is_none() || crate::self_service(hc.ctx, "ShiftSwapRequest"))
        {
            let approver = default_approver(hc, emp, "leave_approver").await?;
            set(doc, "approver", approver.map(Value::String).unwrap_or_default());
        }
        let old = stored(hc, doc).await?;
        let same_ask = old.as_ref().is_some_and(|o| ["employee", "with_employee", "swap_date"].iter().all(|f| s(o, f) == s(doc, f)));
        for f in ["colleague_response", "colleague_responded_on"] {
            let kept = if same_ask { old.as_ref().and_then(|o| o.get(f).cloned()) } else { None };
            set(doc, f, kept.unwrap_or(if f == "colleague_response" { json!("Waiting") } else { Value::Null }));
        }
        // The shifts given and taken are those of the day when it is asked; approval compares them with the day's then.
        if draft
            && !same_ask
            && let Some(day) = opt_date(doc, "swap_date")
        {
            for (who, field) in [("employee", "shift_type"), ("with_employee", "with_shift_type")] {
                let shift = match id(doc, who) {
                    Some(e) => shift_on(hc, e, day).await?,
                    None => None,
                };
                set(doc, field, shift.map(|u| Value::String(u.to_string())).unwrap_or_default());
            }
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        let (Some(a), Some(b), Some(day)) = (id(doc, "employee"), id(doc, "with_employee"), opt_date(doc, "swap_date")) else { return Ok(()) };
        if a == b {
            return Err(Error::field("with_employee", "is the employee asking"));
        }
        let colleague = lookup(hc, "Employee", Some(b)).await?;
        if colleague.as_ref().and_then(|c| id(c, "company")) != id(doc, "company") {
            return Err(Error::field("with_employee", "belongs to another company"));
        }
        if day < hc.ctx.today() {
            return Err(Error::field("swap_date", "has passed"));
        }
        if s(doc, "shift_type").is_none() {
            return Err(Error::field("employee", "has no shift on that day"));
        }
        if s(doc, "with_shift_type").is_none() {
            return Err(Error::field("with_employee", "has no shift on that day"));
        }
        if s(doc, "shift_type") == s(doc, "with_shift_type") {
            return Err(Error::field("with_employee", "you both work the same shift that day"));
        }
        let people = [a, b];
        let on_leave: bool = sqlx::query_scalar(
            "select exists(select 1 from e_leave_application where tenant_id = $1 and employee = any($2) and status = 'Approved' and docstatus = 1 \
             and from_date <= $3 and to_date >= $3)",
        )
        .bind(hc.ctx.tenant)
        .bind(&people[..])
        .bind(day)
        .fetch_one(&mut **hc.tx)
        .await?;
        if on_leave {
            return Err(Error::field("swap_date", "one of you is on leave that day"));
        }
        if marked(hc, people, day).await? {
            return Err(Error::field("swap_date", "attendance is already marked for that day"));
        }
        let other: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_shift_swap_request where tenant_id = $1 and id <> $2 and docstatus < 2 and status in ('Open', 'Approved') \
             and swap_date = $3 and (employee = any($4) or with_employee = any($4)) limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .bind(day)
        .bind(&people[..])
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = other {
            return Err(Error::field("swap_date", format!("one of you already has a swap for that day ({n})")));
        }
        Ok(())
    }

    /// A new request, or one asking someone else, is put to the colleague.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_some_and(|o| s(o, "with_employee") == s(doc, "with_employee")) {
            return Ok(());
        }
        tell(hc, "ShiftSwapRequest", doc, "hr.shift_swap_asked", &["field:with_employee_user"], Default::default()).await
    }

    async fn before_delete(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if full_access(hc.ctx, "ShiftSwapRequest", &[Action::Delete]) || s(doc, "employee_user").is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email)) {
            return Ok(());
        }
        Err(Error::Forbidden("only the employee who asked, or HR, deletes a swap request".into()))
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        let response = match name {
            "accept" => "Accepted",
            "decline" => "Declined",
            _ => return Ok(None),
        };
        let colleague = s(doc, "with_employee_user").is_some_and(|u| u.eq_ignore_ascii_case(&hc.ctx.email));
        if !colleague && !full_access(hc.ctx, "ShiftSwapRequest", &[Action::Write]) {
            return Err(Error::Forbidden("only the colleague asked accepts or declines".into()));
        }
        let me = id(doc, "id").ok_or_else(|| Error::internal("swap request without id"))?;
        let mut v = Doc::new();
        set(&mut v, "colleague_response", response);
        set(&mut v, "colleague_responded_on", now(hc).await?);
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ShiftSwapRequest", me, v).await?;
        if response == "Accepted" {
            tell(hc, "ShiftSwapRequest", doc, "hr.shift_swap_to_approve", &["field:approver"], Default::default()).await?;
        } else {
            decided(hc, doc, "declined").await?;
        }
        Ok(Some(json!({"colleague_response": response})))
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        require_approver(hc.ctx, doc, s(doc, "approver"), "shift swap")
    }

    /// Approval or rejection (the workflow submits the request in either case).
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("swap request without id"))?;
        let rejected = s(doc, "workflow_state") == Some("Rejected");
        if !rejected {
            if s(doc, "colleague_response") != Some("Accepted") {
                return Err(Error::invalid("the colleague has not accepted"));
            }
            swap(hc, doc).await?;
        }
        let mut v = Doc::new();
        set(&mut v, "status", if rejected { "Rejected" } else { "Approved" });
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ShiftSwapRequest", me, v).await?;
        decided(hc, doc, if rejected { "rejected" } else { "approved" }).await
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if hc.ctx.has_role("HR User") {
            return Ok(());
        }
        require_approver(hc.ctx, doc, s(doc, "approver"), "shift swap")
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("swap request without id"))?;
        if s(doc, "status") == Some("Approved") {
            undo(hc, doc).await?;
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ShiftSwapRequest", me, v).await
    }
}
