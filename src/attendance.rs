//! Holiday lists, shifts, check-ins and attendance. Attendance is marked from check-ins once a
//! shift day is over (every minute by `maintenance`, or on demand from the shift type), in the
//! tenant's time zone: punches inside the shift's window count, working hours decide between
//! present, half day and absent, and late entry / early exit are flagged against the grace periods.

use crate::calendar::{days, weekday, weekday_name};
use crate::{lookup, opt_date, stamp_company};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ferrum_domain::util::{b, d, find, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, Filter, HookCtx, HookRegistry, Kernel, Op, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use uuid::Uuid;

/// Days one maintenance round catches up per shift type (the next rounds continue).
const DAYS_PER_ROUND: i64 = 7;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("HolidayList", Arc::new(HolidayListHook))
        .register("ShiftType", Arc::new(ShiftTypeHook))
        .register("ShiftAssignment", Arc::new(ShiftAssignmentHook))
        .register("EmployeeCheckin", Arc::new(CheckinHook))
        .register("Attendance", Arc::new(AttendanceHook));
}

// ───────────────────────────── holiday lists ─────────────────────────────

struct HolidayListHook;

#[async_trait]
impl DocHook for HolidayListHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let mut days = rows(doc, "holidays");
        days.sort_by(|a, b| s(a, "holiday_date").cmp(&s(b, "holiday_date")));
        for (i, r) in days.iter_mut().enumerate() {
            set(r, "idx", i);
        }
        set(doc, "total_holidays", days.len());
        set(doc, "holidays", Value::Array(days.into_iter().map(Value::Object).collect()));
        Ok(())
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (from, to) = (opt_date(doc, "from_date"), opt_date(doc, "to_date"));
        let mut seen = BTreeSet::new();
        let mut errors = Vec::new();
        for (i, r) in rows(doc, "holidays").iter().enumerate() {
            let Some(day) = opt_date(r, "holiday_date") else { continue };
            if from.is_some_and(|f| day < f) || to.is_some_and(|t| day > t) {
                errors.push(FieldError::new("holiday_date", "is outside the list's dates").in_row("holidays", i));
            }
            if !seen.insert(day) {
                errors.push(FieldError::new("holiday_date", "appears twice").in_row("holidays", i));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    /// Adds every given weekday (`weekdays: ["Saturday", "Sunday"]`, else the list's weekly off)
    /// in the list's date range as a weekly off.
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if name != "add_weekly_offs" {
            return Ok(None);
        }
        let mut names: Vec<String> =
            params.get("weekdays").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if let Some(one) = params.get("weekday").and_then(Value::as_str) {
            names.push(one.to_string());
        }
        if names.is_empty()
            && let Some(w) = s(doc, "weekly_off")
        {
            names.push(w.to_string());
        }
        if names.is_empty() {
            return Err(Error::field("weekdays", "give the weekdays to add, e.g. [\"Saturday\", \"Sunday\"]"));
        }
        let mut wanted = Vec::new();
        for n in &names {
            wanted.push(weekday(n).ok_or_else(|| Error::field("weekdays", format!("'{n}' is not a weekday")))?);
        }
        let (from, to) = (crate::opt_date(doc, "from_date"), crate::opt_date(doc, "to_date"));
        let (Some(from), Some(to)) = (from, to) else { return Err(Error::invalid("set the list's from and to dates first")) };
        let mut list = rows(doc, "holidays");
        let have: BTreeSet<NaiveDate> = list.iter().filter_map(|r| opt_date(r, "holiday_date")).collect();
        let mut added = 0;
        for day in days(from, to).filter(|d| wanted.contains(&d.weekday()) && !have.contains(d)) {
            let mut r = Doc::new();
            set(&mut r, "holiday_date", day.to_string());
            set(&mut r, "description", weekday_name(day.weekday()));
            set(&mut r, "weekly_off", true);
            list.push(r);
            added += 1;
        }
        let record = id(doc, "id").ok_or_else(|| Error::internal("holiday list without id"))?;
        let mut patch = Doc::new();
        patch.insert("holidays".into(), Value::Array(list.into_iter().map(Value::Object).collect()));
        let mut updated = hc.kernel.update_tx(hc.ctx, hc.tx, "HolidayList", record, patch, None).await?;
        updated.insert("added".into(), json!(added));
        Ok(Some(Value::Object(updated)))
    }
}

// ───────────────────────────── shifts ─────────────────────────────

/// How a shift type turns punches into attendance.
#[derive(Debug, Clone)]
pub struct Rules {
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// Minutes before the start and after the end in which punches belong to the shift.
    pub before: i64,
    pub after: i64,
    pub late_grace: i64,
    pub early_grace: i64,
    /// Working hours below these mark a half day / absence (zero = off).
    pub half_day_hours: Decimal,
    pub absent_hours: Decimal,
    /// Sum every check-in → check-out pair instead of first check-in to last check-out.
    pub every_pair: bool,
}

#[derive(Debug, Clone)]
pub struct Punch {
    pub id: Uuid,
    /// Local time in the tenant's zone.
    pub at: NaiveDateTime,
    pub utc: DateTime<Utc>,
    pub log_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Marked {
    pub status: &'static str,
    pub working_hours: Decimal,
    pub late_entry: bool,
    pub early_exit: bool,
    /// Indexes of the check-in and check-out punches.
    pub first: usize,
    pub last: Option<usize>,
}

fn hhmm(v: Option<&str>, field: &str) -> Result<NaiveTime> {
    v.and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok()).ok_or_else(|| Error::field(field, "must look like 09:00"))
}

impl Rules {
    pub fn from_doc(doc: &Doc) -> Result<Rules> {
        let int = |k: &str| doc.get(k).and_then(Value::as_i64).unwrap_or(0).max(0);
        Ok(Rules {
            start: hhmm(s(doc, "start_time"), "start_time")?,
            end: hhmm(s(doc, "end_time"), "end_time")?,
            before: int("begin_check_in_before_shift_start_time"),
            after: int("allow_check_out_after_shift_end_time"),
            late_grace: int("late_entry_grace_period"),
            early_grace: int("early_exit_grace_period"),
            half_day_hours: d(doc, "working_hours_threshold_for_half_day"),
            absent_hours: d(doc, "working_hours_threshold_for_absent"),
            every_pair: s(doc, "working_hours_calculation") == Some("Every Valid Check-in and Check-out"),
        })
    }

    /// Shift start and end on `day`; an overnight shift ends the next day.
    pub fn bounds(&self, day: NaiveDate) -> (NaiveDateTime, NaiveDateTime) {
        let end_day = if self.end <= self.start { day.succ_opt().unwrap_or(day) } else { day };
        (day.and_time(self.start), end_day.and_time(self.end))
    }

    /// The span in which punches belong to the shift of `day`.
    pub fn window(&self, day: NaiveDate) -> (NaiveDateTime, NaiveDateTime) {
        let (start, end) = self.bounds(day);
        (start - Duration::minutes(self.before), end + Duration::minutes(self.after))
    }

    /// Attendance from the punches of one shift day (in time order); `None` without punches.
    pub fn mark(&self, day: NaiveDate, punches: &[Punch]) -> Option<Marked> {
        if punches.is_empty() {
            return None;
        }
        let typed = punches.iter().all(|p| p.log_type.is_some());
        let is = |p: &Punch, t: &str| p.log_type.as_deref() == Some(t);
        let first = if typed { punches.iter().position(|p| is(p, "IN")).unwrap_or(0) } else { 0 };
        let last = if typed { punches.iter().rposition(|p| is(p, "OUT")) } else { Some(punches.len() - 1) }.filter(|l| *l > first);
        let minutes = if self.every_pair {
            let mut total = 0;
            if typed {
                let mut open: Option<NaiveDateTime> = None;
                for p in punches {
                    match (is(p, "IN"), open) {
                        (true, None) => open = Some(p.at),
                        (false, Some(o)) => {
                            total += (p.at - o).num_minutes();
                            open = None;
                        }
                        _ => {}
                    }
                }
            } else {
                for pair in punches.chunks(2) {
                    if let [a, b] = pair {
                        total += (b.at - a.at).num_minutes();
                    }
                }
            }
            total
        } else {
            last.map(|l| (punches[l].at - punches[first].at).num_minutes()).unwrap_or(0)
        };
        let working_hours = (Decimal::from(minutes.max(0)) / Decimal::from(60)).round_dp(2);
        let (start, end) = self.bounds(day);
        let late_entry = punches[first].at > start + Duration::minutes(self.late_grace);
        let early_exit = last.is_some_and(|l| punches[l].at < end - Duration::minutes(self.early_grace));
        let status = if self.absent_hours > Decimal::ZERO && working_hours < self.absent_hours {
            "Absent"
        } else if self.half_day_hours > Decimal::ZERO && working_hours < self.half_day_hours {
            "Half Day"
        } else {
            "Present"
        };
        Some(Marked { status, working_hours, late_entry, early_exit, first, last })
    }
}

struct ShiftTypeHook;

#[async_trait]
impl DocHook for ShiftTypeHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let (Some(a), Some(b)) = (s(doc, "start_time"), s(doc, "end_time")) {
            let overnight = b <= a;
            set(doc, "is_overnight", overnight);
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if name != "mark_attendance" {
            return Ok(None);
        }
        let shift = id(doc, "id").ok_or_else(|| Error::internal("shift type without id"))?;
        let up_to = match params.get("up_to").and_then(Value::as_str) {
            Some(v) => Some(ferrum_kernel::value::parse_date(v).map_err(|m| Error::field("up_to", m))?),
            None => None,
        };
        Ok(Some(process_shift(hc, shift, up_to, 366).await?))
    }
}

/// The tenant's time zone (tenant setting `timezone`, else UTC): shift times are local to it.
pub(crate) async fn timezone(tx: &mut Tx, tenant: Uuid) -> Result<String> {
    let tz: Option<String> = sqlx::query_scalar(
        "select tz from (select nullif(settings->>'timezone', '') as tz from tenant where id = $1) t \
         where exists (select 1 from pg_timezone_names where name = t.tz)",
    )
    .bind(tenant)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    Ok(tz.unwrap_or_else(|| "UTC".into()))
}

/// Marks attendance for the completed shift days of one shift type that have none yet, from
/// the day after the last processed one (or `process_attendance_after`) up to the last day whose
/// check-out window has passed (and, when set, whose shift ended before the last device sync).
pub(crate) async fn process_shift(hc: &mut HookCtx<'_>, shift: Uuid, up_to: Option<NaiveDate>, max_days: i64) -> Result<Value> {
    let sctx = sys(hc.ctx);
    // The row lock serializes this with a concurrent run for the same shift type.
    let doc = hc.kernel.get_tx(&sctx, hc.tx, "ShiftType", shift, true).await?;
    let rules = Rules::from_doc(&doc)?;
    let after =
        opt_date(&doc, "process_attendance_after").ok_or_else(|| Error::field("process_attendance_after", "set the date from which attendance is marked"))?;
    let tz = timezone(hc.tx, hc.ctx.tenant).await?;
    let (now_local, sync_local): (NaiveDateTime, Option<NaiveDateTime>) =
        sqlx::query_as("select (now() at time zone $1)::timestamp, ($2::timestamptz at time zone $1)::timestamp")
            .bind(&tz)
            .bind(s(&doc, "last_sync_of_checkin"))
            .fetch_one(&mut **hc.tx)
            .await?;
    let cutoff = sync_local.map_or(now_local, |s| s.min(now_local));
    let mut last_done = cutoff.date();
    while rules.window(last_done).1 > cutoff {
        last_done = last_done.pred_opt().unwrap_or(last_done);
    }
    let from = opt_date(&doc, "last_processed_date").and_then(|d| d.succ_opt()).map_or(after, |d| d.max(after));
    let mut to = up_to.map_or(last_done, |u| u.min(last_done));
    if (to - from).num_days() >= max_days {
        to = from + Duration::days(max_days - 1);
    }
    let (mut marked, mut absent) = (0, 0);
    for day in days(from, to) {
        let (m, a) = mark_day(hc, &doc, &rules, &tz, day).await?;
        marked += m;
        absent += a;
    }
    if to >= from {
        let mut v = Doc::new();
        v.insert("last_processed_date".into(), json!(to.to_string()));
        hc.kernel.set_values_tx(&sctx, hc.tx, "ShiftType", shift, v).await?;
    }
    Ok(json!({"from": from, "to": to, "marked": marked, "absent": absent}))
}

/// One shift day: every employee working this shift that day and without attendance gets it from
/// their punches, or is marked absent on a working day without punches (when the shift says so).
async fn mark_day(hc: &mut HookCtx<'_>, shift: &Doc, rules: &Rules, tz: &str, day: NaiveDate) -> Result<(usize, usize)> {
    let shift_id = id(shift, "id").ok_or_else(|| Error::internal("shift type without id"))?;
    let people: Vec<(Uuid, bool)> = sqlx::query_as(
        "select e.id, exists (select 1 from e_holiday_list__holidays h where h.tenant_id = e.tenant_id and h.holiday_date = $3 \
           and h.parent_id = coalesce(e.holiday_list, $4, st.holiday_list, nullif(c.ext->>'hr_default_holiday_list', '')::uuid)) \
         from e_employee e \
         left join e_shift_type st on st.tenant_id = e.tenant_id and st.id = e.default_shift \
         left join e_company c on c.tenant_id = e.tenant_id and c.id = e.company \
         where e.tenant_id = $1 and e.date_of_joining <= $3 and (e.relieving_date is null or e.relieving_date >= $3) \
           and (e.status = 'Active' or (e.status = 'Left' and e.relieving_date >= $3)) \
           and coalesce((select sa.shift_type from e_shift_assignment sa where sa.tenant_id = e.tenant_id and sa.employee = e.id \
               and sa.docstatus = 1 and sa.start_date <= $3 and (sa.end_date is null or sa.end_date >= $3) order by sa.start_date desc limit 1), \
               e.default_shift) = $2 \
           and not exists (select 1 from e_attendance a where a.tenant_id = e.tenant_id and a.employee = e.id \
               and a.attendance_date = $3 and a.docstatus < 2) \
         order by e.id",
    )
    .bind(hc.ctx.tenant)
    .bind(shift_id)
    .bind(day)
    .bind(id(shift, "holiday_list"))
    .fetch_all(&mut **hc.tx)
    .await?;
    if people.is_empty() {
        return Ok((0, 0));
    }
    let (from, to) = rules.window(day);
    let ids: Vec<Uuid> = people.iter().map(|p| p.0).collect();
    let found: Vec<(Uuid, Uuid, NaiveDateTime, DateTime<Utc>, Option<String>)> = sqlx::query_as(
        "select c.id, c.employee, (c.log_time at time zone $2)::timestamp, c.log_time, c.log_type from e_employee_checkin c \
         where c.tenant_id = $1 and c.employee = any($3) and c.attendance is null and not c.skip_auto_attendance \
           and c.log_time >= ($4::timestamp at time zone $2) and c.log_time <= ($5::timestamp at time zone $2) \
         order by c.employee, c.log_time",
    )
    .bind(hc.ctx.tenant)
    .bind(tz)
    .bind(&ids)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut punches: HashMap<Uuid, Vec<Punch>> = HashMap::new();
    for (pid, emp, at, utc, log_type) in found {
        punches.entry(emp).or_default().push(Punch { id: pid, at, utc, log_type });
    }
    let mark_absent = b(shift, "mark_absent_without_checkins");
    let sctx = sys(hc.ctx);
    let (mut marked, mut absent) = (0, 0);
    for (emp, holiday) in people {
        // Holidays are never marked (and punches on them stay free for overtime or comp-off use).
        if holiday {
            continue;
        }
        let list = punches.remove(&emp).unwrap_or_default();
        let mut a = Doc::new();
        set(&mut a, "employee", emp.to_string());
        set(&mut a, "attendance_date", day.to_string());
        set(&mut a, "shift", shift_id.to_string());
        match rules.mark(day, &list) {
            Some(m) => {
                set(&mut a, "status", m.status);
                set(&mut a, "working_hours", money(m.working_hours));
                set(&mut a, "late_entry", m.late_entry);
                set(&mut a, "early_exit", m.early_exit);
                set(&mut a, "in_time", list[m.first].utc.to_rfc3339());
                if let Some(l) = m.last {
                    set(&mut a, "out_time", list[l].utc.to_rfc3339());
                }
            }
            None if mark_absent => {
                set(&mut a, "status", "Absent");
                set(&mut a, "remarks", "No check-in");
            }
            None => continue,
        }
        let created = hc.kernel.insert_tx(&sctx, hc.tx, "Attendance", a).await?;
        let att = id(&created, "id").ok_or_else(|| Error::internal("attendance without id"))?;
        let created = hc.kernel.submit_tx(&sctx, hc.tx, "Attendance", att, None).await?;
        if s(&created, "status") == Some("Absent") {
            absent += 1;
        } else {
            marked += 1;
        }
        for p in &list {
            let mut v = Doc::new();
            v.insert("attendance".into(), json!(att.to_string()));
            v.insert("shift".into(), json!(shift_id.to_string()));
            hc.kernel.set_values_tx(&sctx, hc.tx, "EmployeeCheckin", p.id, v).await?;
        }
    }
    Ok((marked, absent))
}

/// Maintenance: every shift type with automatic attendance, in its own transaction.
pub(crate) async fn auto_attendance(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let Some(ent) = ctx.schema.entity("ShiftType").cloned() else { return Ok(()) };
    let shifts: Vec<Uuid> = {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let ids = sqlx::query_scalar(
            "select id from e_shift_type where tenant_id = $1 and enable_auto_attendance and process_attendance_after is not null \
             and (last_processed_date is null or last_processed_date < current_date)",
        )
        .bind(ctx.tenant)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        ids
    };
    for shift in shifts {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let mut hc = HookCtx { kernel: k, ctx, tx: &mut tx, entity: &ent };
        process_shift(&mut hc, shift, None, DAYS_PER_ROUND).await?;
        tx.commit().await?;
    }
    Ok(())
}

// ───────────────────────────── assignments, check-ins, attendance ─────────────────────────────

struct ShiftAssignmentHook;

#[async_trait]
impl DocHook for ShiftAssignmentHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(emp), Some(start)) = (id(doc, "employee"), opt_date(doc, "start_date")) else { return Ok(()) };
        let clash: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_shift_assignment where tenant_id = $1 and employee = $2 and docstatus = 1 and id <> $3 \
             and daterange(start_date, end_date, '[]') && daterange($4, $5, '[]') limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(emp)
        .bind(id(doc, "id"))
        .bind(start)
        .bind(opt_date(doc, "end_date"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        match clash {
            Some(n) => Err(Error::field("start_date", format!("overlaps shift assignment {n} of this employee"))),
            None => Ok(()),
        }
    }
}

struct CheckinHook;

#[async_trait]
impl DocHook for CheckinHook {
    /// Devices know employees by their attendance device id.
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if id(doc, "employee").is_none()
            && let Some(dev) = s(doc, "employee_device_id")
        {
            let found = find(hc.kernel, hc.ctx, hc.tx, "Employee", vec![Filter::eq("attendance_device_id", dev)], None, 1).await?;
            match found.first() {
                Some(e) => {
                    doc.insert("employee".into(), e.get("id").cloned().unwrap_or_default());
                    doc.insert("employee_name".into(), e.get("employee_name").cloned().unwrap_or_default());
                    doc.insert("employee_user".into(), e.get("user").cloned().unwrap_or_default());
                }
                None => return Err(Error::field("employee_device_id", format!("no employee has attendance device id '{dev}'"))),
            }
        }
        stamp_company(hc, doc).await?;
        Ok(())
    }

    async fn before_delete(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if let Some(att) = lookup(hc, "Attendance", id(doc, "attendance")).await?
            && d(&att, "docstatus") == Decimal::ONE
        {
            return Err(Error::invalid(format!("this check-in counts for attendance {}; cancel that first", s(&att, "number").unwrap_or_default())));
        }
        Ok(())
    }
}

struct AttendanceHook;

#[async_trait]
impl DocHook for AttendanceHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if !matches!(s(doc, "status"), Some("On Leave" | "Half Day")) {
            set(doc, "leave_type", Value::Null);
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(emp_id), Some(day)) = (id(doc, "employee"), opt_date(doc, "attendance_date")) else { return Ok(()) };
        let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", emp_id).await?;
        let name = s(&emp, "employee_name").unwrap_or_default().to_string();
        if opt_date(&emp, "date_of_joining").is_some_and(|j| day < j) || opt_date(&emp, "relieving_date").is_some_and(|r| day > r) {
            return Err(Error::field("attendance_date", format!("{name} was not employed on {day}")));
        }
        if id(doc, "leave_application").is_none() && crate::is_future(hc, day).await? {
            return Err(Error::field("attendance_date", "attendance cannot be marked for a future date"));
        }
        let dup: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_attendance where tenant_id = $1 and employee = $2 and attendance_date = $3 \
             and docstatus < 2 and id <> $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(emp_id)
        .bind(day)
        .bind(id(doc, "id"))
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = dup {
            return Err(Error::field("attendance_date", format!("attendance of {name} on {day} already exists ({n})")));
        }
        Ok(())
    }

    /// Frees the punches so the day can be marked again.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        sqlx::query("update e_employee_checkin set attendance = null where tenant_id = $1 and attendance = $2")
            .bind(hc.ctx.tenant)
            .bind(id(doc, "id"))
            .execute(&mut **hc.tx)
            .await?;
        Ok(())
    }
}

/// Engine-made attendance for each day of an approved leave, replacing what was there.
pub(crate) async fn mark_leave(hc: &mut HookCtx<'_>, app: &Doc, days_off: &[(NaiveDate, &'static str)]) -> Result<()> {
    let sctx = sys(hc.ctx);
    let emp = id(app, "employee").ok_or_else(|| Error::internal("leave without employee"))?;
    let app_id = id(app, "id").ok_or_else(|| Error::internal("leave without id"))?;
    for (day, status) in days_off {
        let existing = find(
            hc.kernel,
            hc.ctx,
            hc.tx,
            "Attendance",
            vec![
                Filter::eq("employee", emp.to_string()),
                Filter::eq("attendance_date", day.to_string()),
                Filter { field: "docstatus".into(), op: Op::Lt, value: json!(2) },
            ],
            None,
            10,
        )
        .await?;
        for a in existing {
            let aid = id(&a, "id").ok_or_else(|| Error::internal("attendance without id"))?;
            if d(&a, "docstatus") == Decimal::ONE {
                hc.kernel.cancel_tx(&sctx, hc.tx, "Attendance", aid, None).await?;
            } else {
                hc.kernel.delete_tx(&sctx, hc.tx, "Attendance", aid, None).await?;
            }
        }
        let mut a = Doc::new();
        set(&mut a, "employee", emp.to_string());
        set(&mut a, "attendance_date", day.to_string());
        set(&mut a, "status", *status);
        set(&mut a, "leave_type", app.get("leave_type").cloned().unwrap_or_default());
        set(&mut a, "leave_application", app_id.to_string());
        let created = hc.kernel.insert_tx(&sctx, hc.tx, "Attendance", a).await?;
        hc.kernel.submit_tx(&sctx, hc.tx, "Attendance", id(&created, "id").unwrap_or_default(), None).await?;
    }
    Ok(())
}

/// Cancels the attendance an approved leave created.
pub(crate) async fn unmark_leave(hc: &mut HookCtx<'_>, app: Uuid) -> Result<()> {
    let sctx = sys(hc.ctx);
    let made =
        find(hc.kernel, hc.ctx, hc.tx, "Attendance", vec![Filter::eq("leave_application", app.to_string()), Filter::eq("docstatus", 1)], None, 1000).await?;
    for a in made {
        hc.kernel.cancel_tx(&sctx, hc.tx, "Attendance", id(&a, "id").unwrap_or_default(), None).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(start: &str, end: &str) -> Rules {
        Rules {
            start: NaiveTime::parse_from_str(start, "%H:%M").unwrap(),
            end: NaiveTime::parse_from_str(end, "%H:%M").unwrap(),
            before: 60,
            after: 60,
            late_grace: 15,
            early_grace: 10,
            half_day_hours: Decimal::from(6),
            absent_hours: Decimal::from(3),
            every_pair: false,
        }
    }

    fn punch(at: &str, t: Option<&str>) -> Punch {
        let at = NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M").unwrap();
        Punch { id: Uuid::nil(), at, utc: at.and_utc(), log_type: t.map(str::to_string) }
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn statuses_from_working_hours_and_grace() {
        let r = rules("09:00", "18:00");
        let on_time = r.mark(day("2026-10-05"), &[punch("2026-10-05 09:10", Some("IN")), punch("2026-10-05 18:05", Some("OUT"))]).unwrap();
        assert_eq!((on_time.status, on_time.late_entry, on_time.early_exit), ("Present", false, false));
        assert_eq!(on_time.working_hours, Decimal::new(892, 2));
        let late = r.mark(day("2026-10-05"), &[punch("2026-10-05 09:16", None), punch("2026-10-05 17:45", None)]).unwrap();
        assert_eq!((late.status, late.late_entry, late.early_exit), ("Present", true, true));
        let half = r.mark(day("2026-10-05"), &[punch("2026-10-05 09:00", Some("IN")), punch("2026-10-05 14:00", Some("OUT"))]).unwrap();
        assert_eq!(half.status, "Half Day");
        let absent = r.mark(day("2026-10-05"), &[punch("2026-10-05 09:00", Some("IN")), punch("2026-10-05 11:00", Some("OUT"))]).unwrap();
        assert_eq!(absent.status, "Absent");
        let single = r.mark(day("2026-10-05"), &[punch("2026-10-05 09:00", Some("IN"))]).unwrap();
        assert_eq!((single.working_hours, single.last), (Decimal::ZERO, None));
        assert!(r.mark(day("2026-10-05"), &[]).is_none());
    }

    #[test]
    fn overnight_shifts_end_the_next_day() {
        let r = rules("22:00", "06:00");
        assert_eq!(r.window(day("2026-10-05")), (punch("2026-10-05 21:00", None).at, punch("2026-10-06 07:00", None).at));
        let m = r.mark(day("2026-10-05"), &[punch("2026-10-05 21:55", Some("IN")), punch("2026-10-06 06:02", Some("OUT"))]).unwrap();
        assert_eq!((m.status, m.late_entry, m.early_exit), ("Present", false, false));
        assert_eq!(m.working_hours, Decimal::new(812, 2));
    }

    #[test]
    fn every_pair_sums_the_time_in() {
        let mut r = rules("09:00", "18:00");
        r.every_pair = true;
        let ps = [
            punch("2026-10-05 09:00", Some("IN")),
            punch("2026-10-05 13:00", Some("OUT")),
            punch("2026-10-05 14:00", Some("IN")),
            punch("2026-10-05 16:00", Some("OUT")),
        ];
        let m = r.mark(day("2026-10-05"), &ps).unwrap();
        assert_eq!((m.working_hours, m.status, m.early_exit), (Decimal::from(6), "Present", true));
        let untyped: Vec<Punch> = ps.iter().map(|p| Punch { log_type: None, ..p.clone() }).collect();
        assert_eq!(r.mark(day("2026-10-05"), &untyped).unwrap().working_hours, Decimal::from(6));
    }
}
