//! Overtime asked for by the employee and approved through a workflow, turned into time off of the leave type its rule
//! names. What counts is what was recorded: a working day's attendance beyond the shift, or a day off's check-ins.

use crate::leave::{LedgerEntry, allocation_on, post, remaining, reverse};
use crate::roster::{shift_on, tell};
use crate::{default_approver, require_approver, stamp_company, stored};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use ferrum_domain::util::{d, id, money, s, set, sys};
use ferrum_kernel::{Doc, DocHook, Error, HookCtx, HookRegistry, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("OvertimeRequest", Arc::new(OvertimeHook));
}

struct OvertimeHook;

/// The enabled rule of the company for the kind of day, a rule naming the employee's shift that day before one naming none.
async fn rule_for(hc: &mut HookCtx<'_>, company: Uuid, employee: Uuid, day: NaiveDate) -> Result<Option<Uuid>> {
    let off = crate::calendar::employee_holidays(hc.tx, hc.ctx.tenant, employee, day, day).await?.contains(&day);
    let shift = shift_on(hc, employee, day).await?;
    Ok(sqlx::query_scalar(
        "select id from e_overtime_rule where tenant_id = $1 and company = $2 and not coalesce(disabled, false) and day_kind = $3 \
         and (shift_type is null or shift_type = $4) order by shift_type nulls last, rule_name limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(company)
    .bind(if off { "Holiday or Weekly Off" } else { "Working Day" })
    .bind(shift)
    .fetch_optional(&mut **hc.tx)
    .await?)
}

/// Hours of a shift, end after start (overnight: the next day).
fn shift_hours(start: &str, end: &str) -> Option<Decimal> {
    let (a, b) = (NaiveTime::parse_from_str(start, "%H:%M").ok()?, NaiveTime::parse_from_str(end, "%H:%M").ok()?);
    let mut minutes = (b - a).num_minutes();
    if minutes <= 0 {
        minutes += 24 * 60;
    }
    Some(Decimal::from(minutes) / Decimal::from(60))
}

/// The overtime that was recorded for the employee on the day under the rule, in hours.
async fn recorded(hc: &mut HookCtx<'_>, rule: &Doc, employee: Uuid, day: NaiveDate) -> Result<Decimal> {
    if s(rule, "day_kind") == Some("Working Day") {
        let att: Option<(Option<Decimal>, Option<Uuid>)> = sqlx::query_as(
            "select working_hours, shift from e_attendance where tenant_id = $1 and employee = $2 and attendance_date = $3 and docstatus = 1 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(employee)
        .bind(day)
        .fetch_optional(&mut **hc.tx)
        .await?;
        let Some((worked, shift)) = att else { return Err(Error::field("work_date", "no attendance is recorded that day")) };
        let shift = match shift {
            Some(s) => Some(s),
            None => shift_on(hc, employee, day).await?,
        };
        let length: Option<(String, String)> = match shift {
            Some(s) => {
                sqlx::query_as("select start_time, end_time from e_shift_type where tenant_id = $1 and id = $2")
                    .bind(hc.ctx.tenant)
                    .bind(s)
                    .fetch_optional(&mut **hc.tx)
                    .await?
            }
            None => None,
        };
        let Some(length) = length.and_then(|(a, b)| shift_hours(&a, &b)) else { return Err(Error::field("work_date", "no shift is set for that day")) };
        let grace = Decimal::from(rule.get("grace_minutes").and_then(Value::as_i64).unwrap_or(0)) / Decimal::from(60);
        return Ok((worked.unwrap_or_default() - length - grace).max(Decimal::ZERO));
    }
    // The day is the workspace's: check-ins are kept as instants.
    let span: Option<(Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, i64)> = sqlx::query_as(
        "select min(log_time), max(log_time), count(*) from e_employee_checkin where tenant_id = $1 and employee = $2 \
         and (log_time at time zone $4)::date = $3",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(day)
    .bind(hc.ctx.schema.timezone.clone())
    .fetch_optional(&mut **hc.tx)
    .await?;
    match span {
        Some((Some(a), Some(b), n)) if n >= 2 => Ok(Decimal::from((b - a).num_minutes()) / Decimal::from(60)),
        _ => Err(Error::field("work_date", "fewer than two check-ins are recorded that day")),
    }
}

#[async_trait]
impl DocHook for OvertimeHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let emp = stamp_company(hc, doc).await?;
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        set(doc, "status", "Open");
        let Some(emp) = emp else { return Ok(()) };
        if s(doc, "approver").is_none() || crate::self_service(hc.ctx, "OvertimeRequest") {
            let approver = default_approver(hc, &emp, "leave_approver").await?;
            set(doc, "approver", approver.map(Value::String).unwrap_or_default());
        }
        let (Some(e), Some(c), Some(day)) = (id(&emp, "id"), id(&emp, "company"), crate::opt_date(doc, "work_date")) else { return Ok(()) };
        let rule = rule_for(hc, c, e, day).await?;
        set(doc, "rule", rule.map(|r| Value::String(r.to_string())).unwrap_or_default());
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        let (Some(emp), Some(day)) = (id(doc, "employee"), crate::opt_date(doc, "work_date")) else { return Ok(()) };
        if day > hc.ctx.today() {
            return Err(Error::field("work_date", "overtime is asked for after it is worked"));
        }
        let Some(rule) = crate::lookup(hc, "OvertimeRule", id(doc, "rule")).await? else {
            return Err(Error::field("work_date", "no overtime rule covers that day"));
        };
        let hours = d(doc, "hours");
        let worked = recorded(hc, &rule, emp, day).await?;
        if hours > worked {
            return Err(Error::field("hours", format!("{} hours of overtime are recorded that day", worked.round_dp(2).normalize())));
        }
        let cap = d(&rule, "max_hours_per_month");
        if cap > Decimal::ZERO {
            let first = day.with_day(1).unwrap_or(day);
            let others: Decimal = sqlx::query_scalar(
                "select coalesce(sum(hours), 0) from e_overtime_request where tenant_id = $1 and employee = $2 and rule = $3 and docstatus < 2 \
                 and status in ('Open', 'Approved') and id <> $4 and work_date >= $5 and work_date < ($5::date + interval '1 month')",
            )
            .bind(hc.ctx.tenant)
            .bind(emp)
            .bind(id(&rule, "id"))
            .bind(id(doc, "id"))
            .bind(first)
            .fetch_one(&mut **hc.tx)
            .await?;
            if others + hours > cap {
                return Err(Error::field(
                    "hours",
                    format!("at most {} hours a month under this rule; {} are already asked", cap.normalize(), others.normalize()),
                ));
            }
        }
        Ok(())
    }

    /// A new request, or a new approver, is put to the approver.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_some_and(|o| s(o, "approver") == s(doc, "approver")) {
            return Ok(());
        }
        tell(hc, "OvertimeRequest", doc, "hr.overtime_to_approve", &["field:approver"], Default::default()).await
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        require_approver(hc.ctx, doc, s(doc, "approver"), "overtime")
    }

    /// Approval credits the time off (onto the allocation of the leave type covering the day, else a new one); a
    /// rejection only says so. The workflow submits the request in either case.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("overtime request without id"))?;
        let rejected = s(doc, "workflow_state") == Some("Rejected");
        let mut v = Doc::new();
        let mut values = serde_json::Map::new();
        if rejected {
            set(&mut v, "status", "Rejected");
            values.insert("outcome".into(), json!("rejected"));
        } else {
            let emp = id(doc, "employee").ok_or_else(|| Error::internal("overtime request without employee"))?;
            let day = crate::opt_date(doc, "work_date").ok_or_else(|| Error::internal("overtime request without day"))?;
            let sctx = sys(hc.ctx);
            hc.kernel.get_tx(&sctx, hc.tx, "Employee", emp, true).await?;
            let rule = crate::lookup(hc, "OvertimeRule", id(doc, "rule")).await?.ok_or_else(|| Error::field("rule", "the rule is gone"))?;
            let leave_type = id(&rule, "leave_type").ok_or_else(|| Error::field("rule", "names no leave type"))?;
            let per_day = d(&rule, "hours_per_day").max(Decimal::new(5, 1));
            let days = (d(doc, "hours") * d(&rule, "multiplier").max(Decimal::ONE) / per_day).round_dp(2);
            let allocation = match allocation_on(hc.tx, hc.ctx.tenant, emp, leave_type, day).await? {
                Some((a, _, to)) => {
                    let entry = LedgerEntry {
                        employee: emp,
                        leave_type,
                        kind: "Leave Allocation".into(),
                        transaction: me,
                        number: s(doc, "number").map(str::to_string),
                        allocation: Some(a),
                        leaves: days,
                        from: day,
                        to,
                        carry_forward: false,
                        lwp: false,
                        reversal: false,
                    };
                    post(hc, entry).await?;
                    a
                }
                None => {
                    let valid = rule.get("valid_for_days").and_then(Value::as_i64).unwrap_or(90).max(1);
                    let a = json!({"employee": emp, "leave_type": leave_type, "from_date": day.to_string(),
                        "to_date": (day + chrono::Duration::days(valid - 1)).to_string(), "new_leaves_allocated": money(days),
                        "description": format!("Time off in lieu of overtime {}", s(doc, "number").unwrap_or_default())});
                    let made = hc.kernel.insert_tx(&sctx, hc.tx, "LeaveAllocation", a.as_object().cloned().unwrap_or_default()).await?;
                    let a = id(&made, "id").ok_or_else(|| Error::internal("allocation without id"))?;
                    hc.kernel.submit_tx(&sctx, hc.tx, "LeaveAllocation", a, None).await?;
                    a
                }
            };
            set(&mut v, "status", "Approved");
            set(&mut v, "days_credited", money(days));
            set(&mut v, "leave_allocation", allocation.to_string());
            values.insert("outcome".into(), json!("approved"));
            values.insert("days".into(), json!(days.normalize().to_string()));
        }
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "OvertimeRequest", me, v).await?;
        tell(hc, "OvertimeRequest", doc, "hr.overtime_decided", &["field:employee_user"], values).await
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if hc.ctx.has_role("HR User") {
            return Ok(());
        }
        require_approver(hc.ctx, doc, s(doc, "approver"), "overtime")
    }

    /// Takes the credited days back, refused once they were taken.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("overtime request without id"))?;
        let now = stored(hc, doc).await?.unwrap_or_else(|| doc.clone());
        if s(&now, "status") == Some("Approved")
            && let Some(a) = id(&now, "leave_allocation")
        {
            let sctx = sys(hc.ctx);
            hc.kernel.get_tx(&sctx, hc.tx, "LeaveAllocation", a, true).await?;
            if remaining(hc.tx, hc.ctx.tenant, a, None).await? < d(&now, "days_credited") {
                return Err(Error::invalid("the days were already taken: cancel the leave first"));
            }
            let own: bool = sqlx::query_scalar("select exists(select 1 from e_leave_ledger_entry where tenant_id = $1 and transaction_id = $2)")
                .bind(hc.ctx.tenant)
                .bind(me.to_string())
                .fetch_one(&mut **hc.tx)
                .await?;
            if own {
                reverse(hc, me).await?;
            } else {
                hc.kernel.cancel_tx(&sctx, hc.tx, "LeaveAllocation", a, None).await?;
            }
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "OvertimeRequest", me, v).await
    }
}

#[cfg(test)]
mod tests {
    use super::shift_hours;
    use rust_decimal::Decimal;

    #[test]
    fn a_shift_is_as_long_as_its_hours() {
        assert_eq!(shift_hours("09:00", "17:00"), Some(Decimal::from(8)));
        assert_eq!(shift_hours("21:00", "05:00"), Some(Decimal::from(8)), "overnight");
        assert_eq!(shift_hours("9", "17:00"), None);
    }
}
