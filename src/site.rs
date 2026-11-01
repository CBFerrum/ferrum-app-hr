//! HR's endpoints: `/api/app/hr/…` for signed-in people and `/api/site/{workspace}/hr/…` for the
//! public. For signed-in people, what the approvals page and Home's My work list (the requests
//! routed to them, with what a decision needs beside each), the team calendar and the people
//! directory; for anyone, the careers page: the openings that are published, and an application.
//!
//! Nothing here decides anything: decisions are the workflow's, through the records' own API. These
//! read what the person may see, and where a screen needs a fact their rights would not reach
//! (how many people are in a team, who in it is away) they read it as the system and say no more
//! than that fact.

use crate::calendar::holiday_list_for;
use crate::leave::{allocation_on, available, local_today};
use crate::opt_date;
use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use ferrum_domain::util::{b, d, id, money, rows, s, set, sys};
use ferrum_kernel::{Access, Action, Ctx, Doc, Error, FieldError, Filter, Kernel, ListQuery, Result, Site, SiteRequest, SiteResponse, Tx, with_retry};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;
use uuid::Uuid;

/// The longest span a calendar answers for.
const MOST_CALENDAR_DAYS: i64 = 62;
/// The most people one directory answer holds.
const MOST_PEOPLE: i64 = 5000;
/// The longest text an applicant may send.
const MAX_TEXT: usize = 5000;

pub struct HrSite;

#[async_trait]
impl Site for HrSite {
    async fn handle(&self, k: &Kernel, req: SiteRequest) -> Result<SiteResponse> {
        let segs: Vec<&str> = req.path.split('/').filter(|p| !p.is_empty()).collect();
        let (status, out) = match (req.method.as_str(), segs.as_slice()) {
            // Anyone may read the openings and apply to one.
            ("GET", ["careers"]) => (200, careers(k, req.tenant).await?),
            ("GET", ["careers", o]) => (200, opening(k, req.tenant, uuid(o, "opening")?).await?),
            ("POST", ["careers", o, "apply"]) => (201, apply(k, &req, uuid(o, "opening")?).await?),
            _ => {
                let user = req.user.as_ref().ok_or_else(|| Error::Unauthorized("sign in first".into()))?;
                match (req.method.as_str(), segs.as_slice()) {
                    ("GET", ["inbox"]) => (200, json!({"items": inbox(k, user, true).await?})),
                    ("GET", ["work"]) => (200, work(k, user).await?),
                    ("GET", ["calendar"]) => (200, calendar(k, user, &req).await?),
                    ("GET", ["directory"]) => (200, directory(k, user).await?),
                    _ => return Err(Error::NotFound(format!("hr endpoint {} {}", req.method, req.path))),
                }
            }
        };
        let mut r = SiteResponse::json(status, &out);
        r.headers.push(("cache-control".into(), "no-store".into()));
        Ok(r)
    }

    fn max_body(&self) -> usize {
        16 * 1024
    }
}

fn uuid(raw: &str, what: &str) -> Result<Uuid> {
    raw.parse().map_err(|_| Error::NotFound(what.into()))
}

fn query<'a>(req: &'a SiteRequest, name: &str) -> Option<&'a str> {
    req.query.get(name).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

fn date_param(req: &SiteRequest, name: &str) -> Result<NaiveDate> {
    let v = query(req, name).ok_or_else(|| Error::field(name, "is required (YYYY-MM-DD)"))?;
    ferrum_kernel::value::parse_date(v).map_err(|m| Error::field(name, m))
}

fn body(req: &SiteRequest) -> Result<Map<String, Value>> {
    if req.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(&req.body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(Error::invalid("the request body must be a JSON object")),
    }
}

/// Whether the caller reads every record of `entity`, not only their own.
fn reads_all(ctx: &Ctx, entity: &str) -> bool {
    ctx.is_admin || ctx.is_system || ctx.perms.access(entity, Action::Read) == Access::All
}

/// What the caller may not see at all reads as empty, not as an error.
fn or_none<T: Default>(r: Result<T>) -> Result<T> {
    match r {
        Err(Error::Forbidden(_)) => Ok(T::default()),
        other => other,
    }
}

/// A date for a screen: `YYYY-MM-DD`.
fn day(doc: &Doc, k: &str) -> Value {
    json!(opt_date(doc, k).map(|d| d.to_string()))
}

fn title<'a>(doc: &'a Doc, field: &str) -> Option<&'a str> {
    doc.get("_titles").and_then(|t| t.get(field)).and_then(Value::as_str)
}

fn who(doc: &Doc) -> Value {
    json!({"id": s(doc, "employee"), "name": s(doc, "employee_name"), "department": title(doc, "department")})
}

// ───────────────────────────── approvals ─────────────────────────────

/// Everything waiting for the caller's decision, oldest first: the leave applications, attendance
/// requests and expense claims routed to them. With `context`, each carries the facts a decision
/// needs beside it.
async fn inbox(k: &Kernel, user: &Ctx, context: bool) -> Result<Vec<Value>> {
    if user.email.is_empty() {
        return Ok(vec![]);
    }
    let mut items = or_none(leave_items(k, user, context).await)?;
    items.extend(or_none(attendance_items(k, user, context).await)?);
    items.extend(or_none(expense_items(k, user).await)?);
    items.sort_by(|a, b| a["waiting_since"].as_str().cmp(&b["waiting_since"].as_str()));
    Ok(items)
}

/// Routed to the caller and not decided yet, as the caller sees them.
async fn routed(k: &Kernel, user: &Ctx, entity: &str, approver: &str, status: &str) -> Result<Vec<Doc>> {
    if user.schema.entity(entity).is_none() {
        return Ok(vec![]);
    }
    let q = ListQuery {
        filters: vec![Filter::eq(approver, user.email.to_lowercase()), Filter::eq("status", status)],
        sort: Some("created_at asc".into()),
        limit: Some(100),
        with_titles: true,
        ..Default::default()
    };
    Ok(k.list(user, entity, &q).await?.rows)
}

async fn leave_items(k: &Kernel, user: &Ctx, context: bool) -> Result<Vec<Value>> {
    let applications = routed(k, user, "LeaveApplication", "leave_approver", "Open").await?;
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let mut out = Vec::with_capacity(applications.len());
    for a in &applications {
        let facts = if context { leave_context(&mut tx, user.tenant, a).await? } else { Value::Null };
        out.push(json!({
            "kind": "leave", "entity": "LeaveApplication", "id": s(a, "id"), "version": a.get("version"), "employee": who(a),
            "summary": {"leave_type": title(a, "leave_type"), "from_date": day(a, "from_date"), "to_date": day(a, "to_date"),
                        "half_day": b(a, "half_day"), "days": a.get("total_leave_days")},
            "reason": s(a, "reason"), "waiting_since": s(a, "created_at"), "context": facts,
        }));
    }
    tx.commit().await?;
    Ok(out)
}

/// The balance the leave would leave, and who else in the team is away (or has asked to be).
async fn leave_context(tx: &mut Tx, tenant: Uuid, a: &Doc) -> Result<Value> {
    let (Some(emp), Some(lt), Some(from), Some(to)) = (id(a, "employee"), id(a, "leave_type"), opt_date(a, "from_date"), opt_date(a, "to_date")) else {
        return Ok(Value::Null);
    };
    // Counted now, not as it was when the application was written; none for leave without pay.
    let before = match allocation_on(tx, tenant, emp, lt, from).await? {
        Some(alloc) => Some(available(tx, tenant, emp, lt, alloc, id(a, "id")).await?),
        None => None,
    };
    let after = before.map(|v| v - d(a, "total_leave_days"));
    Ok(json!({"balance_before": before.map(money), "balance_after": after.map(money), "team": team_away(tx, tenant, emp, from, to).await?}))
}

/// The people an employee works with, to see who is away when they would be.
struct Team {
    label: String,
    ids: Vec<Uuid>,
}

/// Their department; failing that, everyone under the same manager.
async fn team_of(tx: &mut Tx, tenant: Uuid, emp: Uuid) -> Result<Option<Team>> {
    let me: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as("select department, reports_to from e_employee where tenant_id = $1 and id = $2")
        .bind(tenant)
        .bind(emp)
        .fetch_optional(&mut **tx)
        .await?;
    let Some((department, manager)) = me else { return Ok(None) };
    if let Some(dep) = department {
        let label: Option<String> = sqlx::query_scalar("select department_name from e_department where tenant_id = $1 and id = $2")
            .bind(tenant)
            .bind(dep)
            .fetch_optional(&mut **tx)
            .await?;
        let ids = sqlx::query_scalar("select id from e_employee where tenant_id = $1 and department = $2 and status = 'Active'")
            .bind(tenant)
            .bind(dep)
            .fetch_all(&mut **tx)
            .await?;
        return Ok(Some(Team { label: label.unwrap_or_default(), ids }));
    }
    if let Some(m) = manager {
        let name: Option<String> =
            sqlx::query_scalar("select employee_name from e_employee where tenant_id = $1 and id = $2").bind(tenant).bind(m).fetch_optional(&mut **tx).await?;
        let ids = sqlx::query_scalar("select id from e_employee where tenant_id = $1 and reports_to = $2 and status = 'Active'")
            .bind(tenant)
            .bind(m)
            .fetch_all(&mut **tx)
            .await?;
        let first = name.as_deref().and_then(|n| n.split_whitespace().next()).unwrap_or("their manager");
        return Ok(Some(Team { label: format!("{first}'s team"), ids }));
    }
    Ok(None)
}

/// Who else in the employee's team is away on `[from, to]`, and how many have asked to be.
async fn team_away(tx: &mut Tx, tenant: Uuid, emp: Uuid, from: NaiveDate, to: NaiveDate) -> Result<Value> {
    let Some(team) = team_of(tx, tenant, emp).await? else { return Ok(Value::Null) };
    let found: Vec<(Uuid, Option<String>, NaiveDate, NaiveDate, i16)> = sqlx::query_as(
        "select employee, employee_name, from_date, to_date, docstatus from e_leave_application where tenant_id = $1 and employee = any($2) \
         and employee <> $3 and from_date <= $5 and to_date >= $4 and docstatus < 2 and status in ('Open', 'Approved') order by from_date, employee_name",
    )
    .bind(tenant)
    .bind(&team.ids)
    .bind(emp)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await?;
    let mut away: Vec<Value> = Vec::new();
    let (mut away_people, mut asking_people) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for (person, name, f, t, docstatus) in found {
        if docstatus == 1 {
            if away_people.insert(person) && away.len() < 6 {
                away.push(json!({"id": person, "name": name, "from_date": f.to_string(), "to_date": t.to_string()}));
            }
        } else {
            asking_people.insert(person);
        }
    }
    Ok(json!({"label": team.label, "size": team.ids.len(), "away": away, "away_count": away_people.len(), "asking_count": asking_people.len()}))
}

async fn attendance_items(k: &Kernel, user: &Ctx, context: bool) -> Result<Vec<Value>> {
    let requests = routed(k, user, "AttendanceRequest", "approver", "Open").await?;
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let tz = crate::attendance::timezone(&mut tx, user.tenant).await?;
    let mut out = Vec::with_capacity(requests.len());
    for r in &requests {
        let facts = match (context, id(r, "employee"), opt_date(r, "from_date"), opt_date(r, "to_date")) {
            (true, Some(emp), Some(from), Some(to)) => attendance_context(&mut tx, user.tenant, &tz, emp, from, to).await?,
            _ => Value::Null,
        };
        out.push(json!({
            "kind": "attendance", "entity": "AttendanceRequest", "id": s(r, "id"), "version": r.get("version"), "employee": who(r),
            "summary": {"reason": s(r, "reason"), "from_date": day(r, "from_date"), "to_date": day(r, "to_date"), "half_day": b(r, "half_day"),
                        "in_time": s(r, "in_time"), "out_time": s(r, "out_time")},
            "reason": s(r, "explanation"), "waiting_since": s(r, "created_at"), "context": facts,
        }));
    }
    tx.commit().await?;
    Ok(out)
}

/// What the days asked about hold now, and the punches on them (on the tenant's wall clock).
async fn attendance_context(tx: &mut Tx, tenant: Uuid, tz: &str, emp: Uuid, from: NaiveDate, to: NaiveDate) -> Result<Value> {
    let marked: Vec<(NaiveDate, String)> = sqlx::query_as(
        "select attendance_date, status from e_attendance where tenant_id = $1 and employee = $2 and docstatus = 1 and attendance_date between $3 and $4 \
         order by attendance_date",
    )
    .bind(tenant)
    .bind(emp)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await?;
    let punches: Vec<(NaiveDateTime, Option<String>)> = sqlx::query_as(
        "select (log_time at time zone $2)::timestamp, log_type from e_employee_checkin where tenant_id = $1 and employee = $3 \
         and log_time >= ($4::date::timestamp at time zone $2) and log_time < (($5::date + 1)::timestamp at time zone $2) order by log_time limit 40",
    )
    .bind(tenant)
    .bind(tz)
    .bind(emp)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await?;
    Ok(json!({
        "recorded": marked.iter().map(|(date, status)| json!({"date": date.to_string(), "status": status})).collect::<Vec<_>>(),
        "punches": punches.iter().map(|(at, kind)| json!({"at": at.format("%Y-%m-%dT%H:%M").to_string(), "log_type": kind})).collect::<Vec<_>>(),
    }))
}

/// Expense claims routed to the caller and not decided: the approver is told when a claim is first
/// saved and can decide it from then on, so a claim with a line to decide on is waiting.
async fn expense_items(k: &Kernel, user: &Ctx) -> Result<Vec<Value>> {
    let claims = routed(k, user, "ExpenseClaim", "expense_approver", "Draft").await?;
    let sctx = sys(user);
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let mut types: HashMap<Uuid, String> = HashMap::new();
    let mut out = Vec::with_capacity(claims.len());
    for c in &claims {
        // The list leaves out a claim's tables.
        let Some(claim_id) = id(c, "id") else { continue };
        let claim = k.get_tx(user, &mut tx, "ExpenseClaim", claim_id, false).await?;
        let mut lines = Vec::new();
        for line in rows(&claim, "expenses") {
            let kind = match id(&line, "expense_type") {
                Some(t) => match types.entry(t) {
                    Entry::Occupied(known) => Some(known.get().clone()),
                    Entry::Vacant(unknown) => {
                        let doc = k.get_tx(&sctx, &mut tx, "ExpenseClaimType", t, false).await?;
                        Some(unknown.insert(s(&doc, "expense_type").unwrap_or_default().to_string()).clone())
                    }
                },
                None => None,
            };
            lines.push(json!({
                "id": s(&line, "id"), "date": day(&line, "expense_date"), "type": kind, "description": s(&line, "description"),
                "amount": line.get("amount"), "sanctioned": line.get("sanctioned_amount"),
            }));
        }
        if lines.is_empty() {
            continue;
        }
        let (currency, precision) = match id(&claim, "currency") {
            Some(cur) => ferrum_domain::util::currency(k, &sctx, &mut tx, cur).await?,
            None => (String::new(), 2),
        };
        out.push(json!({
            "kind": "expense", "entity": "ExpenseClaim", "id": s(c, "id"), "version": c.get("version"), "employee": who(c),
            "summary": {"currency": currency, "precision": precision, "claimed": claim.get("total_claimed_amount"), "sanctioned": claim.get("total_sanctioned_amount"),
                        "taxes": claim.get("total_taxes_and_charges"), "advance": claim.get("total_advance_amount"), "total": claim.get("grand_total"), "lines": lines},
            "reason": s(&claim, "remark"), "waiting_since": s(c, "created_at"), "context": Value::Null,
        }));
    }
    tx.commit().await?;
    Ok(out)
}

/// A line for each request in Home's My work, which links to the approvals page.
async fn work(k: &Kernel, user: &Ctx) -> Result<Value> {
    let items: Vec<Value> = inbox(k, user, false)
        .await?
        .iter()
        .map(|i| {
            let name = i["employee"]["name"].as_str().unwrap_or("Someone");
            let sum = &i["summary"];
            let span = |from: &Value, to: &Value| match (from.as_str(), to.as_str()) {
                (Some(f), Some(t)) if f != t => format!("{f} to {t}"),
                (Some(f), _) => f.to_string(),
                _ => String::new(),
            };
            let what = match i["kind"].as_str() {
                Some("leave") => format!("{}, {}", sum["leave_type"].as_str().unwrap_or("Leave"), span(&sum["from_date"], &sum["to_date"])),
                Some("attendance") => format!("{}, {}", sum["reason"].as_str().unwrap_or("Attendance"), span(&sum["from_date"], &sum["to_date"])),
                _ => format!("Expense claim, {} {}", sum["currency"].as_str().unwrap_or_default(), sum["claimed"].as_str().unwrap_or_default())
                    .trim()
                    .to_string(),
            };
            json!({"title": format!("{name}: {what}"), "subtitle": "Waiting for your approval", "url": "/people/approvals", "updated_at": i["waiting_since"]})
        })
        .collect();
    Ok(json!({"items": items}))
}

// ───────────────────────────── team calendar ─────────────────────────────

/// Who is away between two dates, and the holidays: for the person's own team, or for everyone
/// when their rights reach every employee. Leave types, and requests not yet decided, are told
/// only to those who read every leave application.
async fn calendar(k: &Kernel, user: &Ctx, req: &SiteRequest) -> Result<Value> {
    let (from, to) = (date_param(req, "from")?, date_param(req, "to")?);
    if to < from || (to - from).num_days() > MOST_CALENDAR_DAYS {
        return Err(Error::invalid(format!("ask for at most {MOST_CALENDAR_DAYS} days, from the first to the last")));
    }
    let everyone_allowed = reads_all(user, "Employee");
    let everyone = everyone_allowed && query(req, "scope") == Some("all");
    let details = reads_all(user, "LeaveApplication");
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let mine: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(r#"select id, reports_to from e_employee where tenant_id = $1 and lower("user") = $2 limit 1"#)
        .bind(user.tenant)
        .bind(user.email.to_lowercase())
        .fetch_optional(&mut *tx)
        .await?;
    // Their team is themselves, their manager, the others under that manager, and their own reports.
    let (people, size): (Option<Vec<Uuid>>, i64) = if everyone {
        let n = sqlx::query_scalar("select count(*) from e_employee where tenant_id = $1 and status = 'Active'").bind(user.tenant).fetch_one(&mut *tx).await?;
        (None, n)
    } else {
        let ids: Vec<Uuid> = match mine {
            Some((me, manager)) => {
                sqlx::query_scalar(
                    "select id from e_employee where tenant_id = $1 and status = 'Active' \
                     and (id = $2 or id = $3 or reports_to = $3 or reports_to = $2)",
                )
                .bind(user.tenant)
                .bind(me)
                .bind(manager)
                .fetch_all(&mut *tx)
                .await?
            }
            None => vec![],
        };
        let n = ids.len() as i64;
        (Some(ids), n)
    };
    let found: Vec<(Uuid, Option<String>, Option<String>, NaiveDate, NaiveDate, Option<NaiveDate>, i16, String)> = sqlx::query_as(
        "select a.employee, a.employee_name, dep.department_name, a.from_date, a.to_date, a.half_day_date, a.docstatus, t.leave_type_name \
         from e_leave_application a join e_leave_type t on t.tenant_id = a.tenant_id and t.id = a.leave_type \
         left join e_employee e on e.tenant_id = a.tenant_id and e.id = a.employee \
         left join e_department dep on dep.tenant_id = a.tenant_id and dep.id = e.department \
         where a.tenant_id = $1 and a.from_date <= $3 and a.to_date >= $2 and a.docstatus < 2 and a.status in ('Open', 'Approved') \
         and ($4::uuid[] is null or a.employee = any($4)) order by a.from_date, a.employee_name limit 500",
    )
    .bind(user.tenant)
    .bind(from)
    .bind(to)
    .bind(people)
    .fetch_all(&mut *tx)
    .await?;
    let away: Vec<Value> = found
        .into_iter()
        .filter(|row| row.6 == 1 || details)
        .map(|(person, name, department, f, t, half, docstatus, kind)| {
            json!({"employee": person, "name": name, "department": department, "from_date": f.to_string(), "to_date": t.to_string(),
                   "half_day_date": half.map(|h| h.to_string()), "pending": docstatus == 0, "leave_type": details.then_some(kind)})
        })
        .collect();
    // The holidays of the person's own list; the company's default for someone with no employee record.
    let list: Option<Uuid> = match mine {
        Some((me, _)) => holiday_list_for(&mut tx, user.tenant, me, None).await?,
        None => {
            let default: Option<Option<Uuid>> =
                sqlx::query_scalar("select nullif(ext->>'hr_default_holiday_list', '')::uuid from e_company where tenant_id = $1 order by created_at limit 1")
                    .bind(user.tenant)
                    .fetch_optional(&mut *tx)
                    .await?;
            default.flatten()
        }
    };
    let holidays: Vec<(NaiveDate, String, bool)> = match list {
        Some(list) => {
            sqlx::query_as(
                "select holiday_date, description, weekly_off from e_holiday_list__holidays where tenant_id = $1 and parent_id = $2 \
                 and holiday_date between $3 and $4 order by holiday_date",
            )
            .bind(user.tenant)
            .bind(list)
            .bind(from)
            .bind(to)
            .fetch_all(&mut *tx)
            .await?
        }
        None => vec![],
    };
    tx.commit().await?;
    Ok(json!({
        "from": from.to_string(), "to": to.to_string(), "scope": if everyone { "all" } else { "team" }, "everyone_allowed": everyone_allowed,
        "size": size, "has_record": mine.is_some(), "away": away,
        "holidays": holidays.iter().map(|(date, name, off)| json!({"date": date.to_string(), "name": name, "weekly_off": off})).collect::<Vec<_>>(),
    }))
}

// ───────────────────────────── people directory ─────────────────────────────

/// Active employees with what a directory and an org chart show: name, role, department, place,
/// manager and work email. Phone numbers and joining dates only to those who read every employee.
async fn directory(k: &Kernel, user: &Ctx) -> Result<Value> {
    if !user.is_admin && !user.is_system && user.perms.access("Employee", Action::Read) == Access::None {
        return Err(Error::Forbidden("you cannot see the people directory".into()));
    }
    let detail = reads_all(user, "Employee");
    // A person limited to some companies sees those companies' people.
    let companies: Option<Vec<Uuid>> = user.scopes.get("Company").cloned();
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let found: Vec<(Uuid, Option<String>, Option<String>, Option<String>, Option<String>, Option<Uuid>, Option<String>, Option<String>, Option<NaiveDate>)> =
        sqlx::query_as(
            "select e.id, e.employee_name, g.designation_name, d.department_name, b.branch_name, e.reports_to, e.company_email, e.cell_number, e.date_of_joining \
             from e_employee e left join e_designation g on g.tenant_id = e.tenant_id and g.id = e.designation \
             left join e_department d on d.tenant_id = e.tenant_id and d.id = e.department \
             left join e_branch b on b.tenant_id = e.tenant_id and b.id = e.branch \
             where e.tenant_id = $1 and e.status = 'Active' and ($2::uuid[] is null or e.company = any($2)) order by e.employee_name limit $3",
        )
        .bind(user.tenant)
        .bind(companies)
        .bind(MOST_PEOPLE + 1)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let truncated = found.len() as i64 > MOST_PEOPLE;
    let people: Vec<Value> = found
        .into_iter()
        .take(MOST_PEOPLE as usize)
        .map(|(person, name, designation, department, branch, manager, email, phone, joined)| {
            json!({"id": person, "name": name, "designation": designation, "department": department, "branch": branch, "manager": manager, "email": email,
                   "phone": phone.filter(|_| detail), "joined": joined.filter(|_| detail).map(|j| j.to_string())})
        })
        .collect();
    Ok(json!({"people": people, "truncated": truncated, "detail": detail}))
}

// ───────────────────────────── careers ─────────────────────────────

/// A system context for a workspace's public requests.
async fn system(k: &Kernel, tenant: Uuid) -> Result<Ctx> {
    Ok(Ctx::system(tenant, k.registry.schema(tenant).await?))
}

type Opening =
    (Uuid, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<NaiveDate>, Option<NaiveDate>, Option<String>);

fn opening_json((open, title, designation, department, branch, kind, company, posted, closes, description): Opening) -> Value {
    json!({"id": open, "title": title, "designation": designation, "department": department, "branch": branch, "employment_type": kind, "company": company,
           "posted_on": posted.map(|d| d.to_string()), "closes_on": closes.map(|d| d.to_string()), "description": description})
}

/// The openings anyone may see: published, open, and not past their closing date. One opening
/// with its description, or all of them without.
async fn published(tx: &mut Tx, tenant: Uuid, only: Option<Uuid>, today: NaiveDate) -> Result<Vec<Opening>> {
    Ok(sqlx::query_as(
        "select o.id, o.job_title, g.designation_name, d.department_name, b.branch_name, t.employment_type_name, c.company_name, o.posted_on, o.closes_on, \
         case when $2::uuid is null then null else o.description end \
         from e_job_opening o left join e_designation g on g.tenant_id = o.tenant_id and g.id = o.designation \
         left join e_department d on d.tenant_id = o.tenant_id and d.id = o.department \
         left join e_branch b on b.tenant_id = o.tenant_id and b.id = o.branch \
         left join e_employment_type t on t.tenant_id = o.tenant_id and t.id = o.employment_type \
         left join e_company c on c.tenant_id = o.tenant_id and c.id = o.company \
         where o.tenant_id = $1 and o.publish and o.status = 'Open' and (o.closes_on is null or o.closes_on >= $3) and ($2::uuid is null or o.id = $2) \
         order by o.posted_on desc nulls last, o.created_at desc limit 100",
    )
    .bind(tenant)
    .bind(only)
    .bind(today)
    .fetch_all(&mut **tx)
    .await?)
}

async fn careers(k: &Kernel, tenant: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(tenant).await?;
    let today = local_today(&mut tx, tenant).await?;
    let list = published(&mut tx, tenant, None, today).await?;
    tx.commit().await?;
    Ok(json!({"openings": list.into_iter().map(opening_json).collect::<Vec<_>>()}))
}

async fn opening(k: &Kernel, tenant: Uuid, open: Uuid) -> Result<Value> {
    let mut tx = k.db.tenant_tx(tenant).await?;
    let today = local_today(&mut tx, tenant).await?;
    let found = published(&mut tx, tenant, Some(open), today).await?;
    tx.commit().await?;
    found.into_iter().next().map(opening_json).ok_or_else(|| Error::NotFound("opening".into()))
}

/// A required piece of text, trimmed, at most `max` characters.
fn text(body: &Map<String, Value>, key: &str, max: usize) -> Result<String> {
    let v = body.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if v.is_empty() {
        return Err(Error::field(key, "is required"));
    }
    if v.chars().count() > max {
        return Err(Error::field(key, format!("at most {max} characters")));
    }
    Ok(v.to_string())
}

/// An optional piece of text, trimmed, at most `max` characters.
fn maybe(body: &Map<String, Value>, key: &str, max: usize) -> Result<Option<String>> {
    match body.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v.chars().count() > max => Err(Error::field(key, format!("at most {max} characters"))),
        v => Ok(v.map(str::to_string)),
    }
}

/// An address that could be one: something before and after a single `@`, a dot in the domain,
/// no spaces. Whether it reaches anyone is not ours to know.
fn clean_email(raw: &str) -> Option<String> {
    let e = raw.trim().to_lowercase();
    let (local, domain) = e.split_once('@')?;
    let ok = !local.is_empty() && !domain.contains('@') && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.');
    (ok && e.len() <= 254 && !e.chars().any(char::is_whitespace)).then_some(e)
}

/// A web address a person can follow.
fn clean_link(raw: &str) -> Option<String> {
    let l = raw.trim();
    ((l.starts_with("https://") || l.starts_with("http://")) && l.len() <= 500 && !l.chars().any(|c| c.is_whitespace() || c.is_control()))
        .then(|| l.to_string())
}

/// The application form's name for a field of the applicant record.
fn form_field(name: &str) -> &str {
    match name {
        "applicant_name" => "name",
        "email_id" => "email",
        "phone_number" => "phone",
        other => other,
    }
}

/// `POST careers/{opening}/apply`: an application for a published opening, from anyone. It keeps
/// the person's name, address, phone, a link to their CV and a few words; the recruiters take it
/// from there, as with any applicant. The same address applies to an opening once.
async fn apply(k: &Kernel, req: &SiteRequest, open: Uuid) -> Result<Value> {
    let input = body(req)?;
    // A field no person sees: whoever fills it in is a program, which is thanked and forgotten.
    if input.get("website").and_then(Value::as_str).is_some_and(|w| !w.trim().is_empty()) {
        return Ok(json!({"received": true}));
    }
    let name = text(&input, "name", 120)?;
    let email = clean_email(&text(&input, "email", 254)?).ok_or_else(|| Error::field("email", "does not look like an email address"))?;
    let phone = maybe(&input, "phone", 40)?;
    let letter = maybe(&input, "cover_letter", MAX_TEXT)?;
    let link = match maybe(&input, "resume_link", 500)? {
        Some(l) => Some(clean_link(&l).ok_or_else(|| Error::field("resume_link", "must be a web address starting with https://"))?),
        None => None,
    };
    k.rate_limit(
        &format!("hr|apply|{}|{}", req.tenant, req.client_ip),
        Duration::from_secs(3600),
        10,
        "too many applications from here in an hour; try again later",
    )
    .await?;
    let sctx = system(k, req.tenant).await?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(req.tenant).await?;
        let today = local_today(&mut tx, req.tenant).await?;
        // An opening that is not published, or is closed, reads as missing.
        if published(&mut tx, req.tenant, Some(open), today).await?.is_empty() {
            return Err(Error::NotFound("opening".into()));
        }
        let again: bool =
            sqlx::query_scalar("select exists (select 1 from e_job_applicant where tenant_id = $1 and job_opening = $2 and lower(email_id) = $3)")
                .bind(req.tenant)
                .bind(open)
                .bind(&email)
                .fetch_one(&mut *tx)
                .await?;
        // A second application from the same address is thanked and not kept twice, and does not say that the address applied before.
        if again {
            return Ok(json!({"received": true}));
        }
        let mut a = Doc::new();
        set(&mut a, "applicant_name", name.clone());
        set(&mut a, "email_id", email.clone());
        set(&mut a, "phone_number", phone.clone().map(Value::String).unwrap_or_default());
        set(&mut a, "job_opening", open.to_string());
        set(&mut a, "source", "Website Listing");
        set(&mut a, "cover_letter", letter.clone().map(Value::String).unwrap_or_default());
        set(&mut a, "resume_link", link.clone().map(Value::String).unwrap_or_default());
        match k.insert_tx(&sctx, &mut tx, "JobApplicant", a).await {
            Ok(_) => {}
            // What is wrong is told in the form's own names for its fields.
            Err(Error::Validation(found)) => {
                return Err(Error::Validation(found.into_iter().map(|e| FieldError::new(form_field(&e.field), e.message)).collect()));
            }
            Err(e) => return Err(e),
        }
        tx.commit().await?;
        Ok(json!({"received": true}))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applicants_are_taken_at_their_word_but_not_blindly() {
        assert_eq!(form_field("email_id"), "email");
        assert_eq!(form_field("job_opening"), "job_opening");
        assert_eq!(clean_email("  Asha.Rao@Example.com "), Some("asha.rao@example.com".into()));
        for bad in ["asha", "asha@", "@example.com", "a@b", "a b@example.com", "a@@example.com", "a@example.", "a@.example.com"] {
            assert_eq!(clean_email(bad), None, "{bad}");
        }
        assert_eq!(clean_link(" https://example.com/cv.pdf "), Some("https://example.com/cv.pdf".into()));
        for bad in ["example.com/cv", "javascript:alert(1)", "https://exa mple.com", "ftp://example.com"] {
            assert_eq!(clean_link(bad), None, "{bad}");
        }
        let mut input = Map::new();
        input.insert("name".into(), json!("  Asha  "));
        input.insert("phone".into(), json!("  "));
        input.insert("long".into(), json!("x".repeat(11)));
        assert_eq!(text(&input, "name", 10).unwrap(), "Asha");
        assert!(text(&input, "email", 10).is_err());
        assert!(text(&input, "long", 10).is_err());
        assert_eq!(maybe(&input, "phone", 10).unwrap(), None);
        assert!(maybe(&input, "long", 10).is_err());
    }
}
