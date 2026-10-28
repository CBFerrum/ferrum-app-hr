//! Leave: allocations, applications through the approval workflow, and the leave ledger. Every
//! change to a balance is a ledger entry (allocation +, application −, encashment −, expiry −)
//! tied to the allocation it draws on, so a balance is a sum, never a recomputation; cancelling
//! posts reversing entries.

use crate::attendance::{mark_leave, unmark_leave};
use crate::calendar::{days, employee_holidays, leave_days, working_days};
use crate::{default_approver, employee_of, notify, opt_date, require_approver, stamp_company, today};
use async_trait::async_trait;
use chrono::NaiveDate;
use ferrum_domain::util::{b, d, elevated, find, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Action, Ctx, Doc, DocHook, Error, Filter, HookCtx, HookRegistry, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn register(h: &mut HookRegistry) {
    h.register("LeavePolicy", Arc::new(PolicyHook))
        .register("LeaveAllocation", Arc::new(AllocationHook))
        .register("LeaveApplication", Arc::new(ApplicationHook))
        .register("LeaveLedgerEntry", Arc::new(LedgerGuard));
}

// ───────────────────────────── ledger ─────────────────────────────

pub(crate) struct LedgerEntry {
    pub employee: Uuid,
    pub leave_type: Uuid,
    pub kind: String,
    pub transaction: Uuid,
    pub number: Option<String>,
    pub allocation: Option<Uuid>,
    pub leaves: Decimal,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub carry_forward: bool,
    pub lwp: bool,
    pub reversal: bool,
}

pub(crate) async fn post(hc: &mut HookCtx<'_>, e: LedgerEntry) -> Result<()> {
    if e.leaves.is_zero() {
        return Ok(());
    }
    let mut v = Doc::new();
    set(&mut v, "employee", e.employee.to_string());
    set(&mut v, "leave_type", e.leave_type.to_string());
    set(&mut v, "transaction_type", e.kind);
    set(&mut v, "transaction_id", e.transaction.to_string());
    set(&mut v, "transaction_number", e.number.map(Value::String).unwrap_or_default());
    set(&mut v, "allocation", e.allocation.map(|a| json!(a.to_string())).unwrap_or_default());
    set(&mut v, "leaves", money(e.leaves));
    set(&mut v, "from_date", e.from.to_string());
    set(&mut v, "to_date", e.to.to_string());
    set(&mut v, "is_carry_forward", e.carry_forward);
    set(&mut v, "is_lwp", e.lwp);
    set(&mut v, "is_reversal", e.reversal);
    hc.kernel.insert_tx(&sys(hc.ctx), hc.tx, "LeaveLedgerEntry", v).await?;
    Ok(())
}

/// Reverses whatever a transaction posted and still nets non-zero (so it is idempotent).
pub(crate) async fn reverse(hc: &mut HookCtx<'_>, transaction: Uuid) -> Result<()> {
    let open: Vec<(Uuid, Uuid, String, Option<String>, Option<Uuid>, bool, bool, NaiveDate, NaiveDate, Decimal)> = sqlx::query_as(
        "select employee, leave_type, transaction_type, max(transaction_number), allocation, is_carry_forward, is_lwp, min(from_date), \
         max(to_date), sum(leaves) from e_leave_ledger_entry where tenant_id = $1 and transaction_id = $2 \
         group by employee, leave_type, transaction_type, allocation, is_carry_forward, is_lwp having sum(leaves) <> 0",
    )
    .bind(hc.ctx.tenant)
    .bind(transaction.to_string())
    .fetch_all(&mut **hc.tx)
    .await?;
    for (employee, leave_type, kind, number, allocation, carry_forward, lwp, from, to, net) in open {
        let e = LedgerEntry { employee, leave_type, kind, transaction, number, allocation, leaves: -net, from, to, carry_forward, lwp, reversal: true };
        post(hc, e).await?;
    }
    Ok(())
}

/// The submitted allocation of a leave type covering `day`: (id, from, to).
pub(crate) async fn allocation_on(tx: &mut Tx, tenant: Uuid, employee: Uuid, leave_type: Uuid, day: NaiveDate) -> Result<Option<(Uuid, NaiveDate, NaiveDate)>> {
    Ok(sqlx::query_as(
        "select id, from_date, to_date from e_leave_allocation where tenant_id = $1 and employee = $2 and leave_type = $3 and docstatus = 1 \
         and from_date <= $4 and to_date >= $4 limit 1",
    )
    .bind(tenant)
    .bind(employee)
    .bind(leave_type)
    .bind(day)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Leaves left in an allocation, before year-end expiry (bookkeeping, not consumption) and
/// leaving out one transaction (the document being validated).
pub(crate) async fn remaining(tx: &mut Tx, tenant: Uuid, allocation: Uuid, exclude: Option<Uuid>) -> Result<Decimal> {
    Ok(sqlx::query_scalar(
        "select coalesce(sum(leaves), 0) from e_leave_ledger_entry where tenant_id = $1 and allocation = $2 and transaction_type <> 'Expiry' \
         and ($3::text is null or transaction_id <> $3)",
    )
    .bind(tenant)
    .bind(allocation)
    .bind(exclude.map(|u| u.to_string()))
    .fetch_one(&mut **tx)
    .await?)
}

/// Days asked for in open (not yet decided) applications within a period.
pub(crate) async fn pending(
    tx: &mut Tx,
    tenant: Uuid,
    employee: Uuid,
    leave_type: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    exclude: Option<Uuid>,
) -> Result<Decimal> {
    Ok(sqlx::query_scalar(
        "select coalesce(sum(total_leave_days), 0) from e_leave_application where tenant_id = $1 and employee = $2 and leave_type = $3 \
         and docstatus = 0 and from_date <= $5 and to_date >= $4 and ($6::uuid is null or id <> $6)",
    )
    .bind(tenant)
    .bind(employee)
    .bind(leave_type)
    .bind(from)
    .bind(to)
    .bind(exclude)
    .fetch_one(&mut **tx)
    .await?)
}

/// Leaves that can still be applied for from an allocation: remaining minus open applications.
pub(crate) async fn available(
    tx: &mut Tx,
    tenant: Uuid,
    employee: Uuid,
    leave_type: Uuid,
    alloc: (Uuid, NaiveDate, NaiveDate),
    exclude: Option<Uuid>,
) -> Result<Decimal> {
    let left = remaining(tx, tenant, alloc.0, exclude).await?;
    Ok(left - pending(tx, tenant, employee, leave_type, alloc.1, alloc.2, exclude).await?)
}

/// Keeps an ended allocation's expiry equal to what it has left: posts the difference (dated at
/// the allocation's end). Late approvals or cancellations in an ended period adjust it again.
pub(crate) async fn sync_expiry(hc: &mut HookCtx<'_>, allocation: Uuid, cause: Uuid) -> Result<()> {
    let a = get(hc.kernel, hc.ctx, hc.tx, "LeaveAllocation", allocation).await?;
    let Some(end) = opt_date(&a, "to_date") else { return Ok(()) };
    if d(&a, "docstatus") != Decimal::ONE || end >= today() {
        return Ok(());
    }
    let left = remaining(hc.tx, hc.ctx.tenant, allocation, None).await?;
    let expired: Decimal = sqlx::query_scalar(
        "select coalesce(sum(leaves), 0) from e_leave_ledger_entry where tenant_id = $1 and allocation = $2 and transaction_type = 'Expiry'",
    )
    .bind(hc.ctx.tenant)
    .bind(allocation)
    .fetch_one(&mut **hc.tx)
    .await?;
    let delta = -left.max(Decimal::ZERO) - expired;
    if delta.is_zero() {
        return Ok(());
    }
    let e = LedgerEntry {
        employee: id(&a, "employee").unwrap_or_default(),
        leave_type: id(&a, "leave_type").unwrap_or_default(),
        kind: "Expiry".into(),
        transaction: cause,
        number: s(&a, "number").map(str::to_string),
        allocation: Some(allocation),
        leaves: delta,
        from: end,
        to: end,
        carry_forward: false,
        lwp: false,
        reversal: delta > Decimal::ZERO,
    };
    post(hc, e).await
}

/// Maintenance: expires what is left of allocations that ended in the last two months. Later
/// changes to an ended allocation (late approvals, cancellations, carry forward) re-sync its
/// expiry in the same transaction, so older allocations never need scanning.
pub(crate) async fn expire_allocations(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let Some(ent) = ctx.schema.entity("LeaveAllocation").cloned() else { return Ok(()) };
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let ended: Vec<Uuid> = sqlx::query_scalar(
        "select a.id from e_leave_allocation a where a.tenant_id = $1 and a.docstatus = 1 \
         and a.to_date < current_date and a.to_date >= current_date - 62 \
         and (select coalesce(sum(l.leaves), 0) from e_leave_ledger_entry l where l.tenant_id = a.tenant_id and l.allocation = a.id) > 0 \
         order by a.to_date limit 200",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    let mut hc = HookCtx { kernel: k, ctx, tx: &mut tx, entity: &ent };
    for a in ended {
        sync_expiry(&mut hc, a, a).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Entries may only be written by the system and never removed.
struct LedgerGuard;

#[async_trait]
impl DocHook for LedgerGuard {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if !hc.ctx.is_system {
            return Err(Error::Forbidden("leave ledger entries are written by the system only".into()));
        }
        stamp_company(hc, doc).await?;
        Ok(())
    }

    async fn before_delete(&self, _hc: &mut HookCtx<'_>, _doc: &Doc) -> Result<()> {
        Err(Error::Forbidden("the leave ledger is append-only".into()))
    }
}

// ───────────────────────────── allocations ─────────────────────────────

/// The latest submitted allocation of the type that ended before `before`.
async fn previous_allocation(tx: &mut Tx, tenant: Uuid, employee: Uuid, leave_type: Uuid, before: NaiveDate) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "select id from e_leave_allocation where tenant_id = $1 and employee = $2 and leave_type = $3 and docstatus = 1 and to_date < $4 \
         order by to_date desc limit 1",
    )
    .bind(tenant)
    .bind(employee)
    .bind(leave_type)
    .bind(before)
    .fetch_optional(&mut **tx)
    .await?)
}

struct AllocationHook;

#[async_trait]
impl DocHook for AllocationHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        let mut carried = Decimal::ZERO;
        if b(doc, "carry_forward")
            && let (Some(emp), Some(lt), Some(from)) = (id(doc, "employee"), id(doc, "leave_type"), opt_date(doc, "from_date"))
        {
            let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
            if b(&ltd, "is_carry_forward")
                && let Some(prev) = previous_allocation(hc.tx, hc.ctx.tenant, emp, lt, from).await?
            {
                carried = remaining(hc.tx, hc.ctx.tenant, prev, None).await?.max(Decimal::ZERO);
                let cap = d(&ltd, "maximum_carry_forwarded_leaves");
                if cap > Decimal::ZERO {
                    carried = carried.min(cap);
                }
            }
        }
        set(doc, "unused_leaves", money(carried));
        set(doc, "total_leaves_allocated", money(d(doc, "new_leaves_allocated") + carried));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(emp_id), Some(lt), Some(from), Some(to)) = (id(doc, "employee"), id(doc, "leave_type"), opt_date(doc, "from_date"), opt_date(doc, "to_date"))
        else {
            return Ok(());
        };
        let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
        let type_name = s(&ltd, "leave_type_name").unwrap_or_default().to_string();
        if b(&ltd, "is_lwp") {
            return Err(Error::field("leave_type", format!("{type_name} is leave without pay and needs no allocation")));
        }
        let cap = d(&ltd, "max_leaves_allowed");
        if cap > Decimal::ZERO && d(doc, "new_leaves_allocated") > cap {
            return Err(Error::field("new_leaves_allocated", format!("{type_name} allows at most {} leaves per allocation", cap.normalize())));
        }
        let emp = get(hc.kernel, hc.ctx, hc.tx, "Employee", emp_id).await?;
        if opt_date(&emp, "date_of_joining").is_some_and(|j| to < j) {
            return Err(Error::field("to_date", "the allocation ends before the employee joined"));
        }
        let clash: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_leave_allocation where tenant_id = $1 and employee = $2 and leave_type = $3 and docstatus = 1 \
             and id <> $4 and from_date <= $6 and to_date >= $5 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(emp_id)
        .bind(lt)
        .bind(id(doc, "id"))
        .bind(from)
        .bind(to)
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = clash {
            return Err(Error::field("from_date", format!("overlaps allocation {n} of {type_name} for this employee")));
        }
        Ok(())
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("allocation without id"))?;
        let (emp, lt) = (id(doc, "employee").unwrap_or_default(), id(doc, "leave_type").unwrap_or_default());
        let (from, to) = (crate::opt_date(doc, "from_date").unwrap_or_default(), crate::opt_date(doc, "to_date").unwrap_or_default());
        let number = s(doc, "number").map(str::to_string);
        let entry = |leaves: Decimal, carry_forward: bool| LedgerEntry {
            employee: emp,
            leave_type: lt,
            kind: "Leave Allocation".into(),
            transaction: me,
            number: number.clone(),
            allocation: Some(me),
            leaves,
            from,
            to,
            carry_forward,
            lwp: false,
            reversal: false,
        };
        post(hc, entry(d(doc, "new_leaves_allocated"), false)).await?;
        let carried = d(doc, "unused_leaves");
        post(hc, entry(carried, true)).await?;
        if carried > Decimal::ZERO
            && let Some(prev) = previous_allocation(hc.tx, hc.ctx.tenant, emp, lt, from).await?
        {
            sync_expiry(hc, prev, me).await?;
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let taken: bool = sqlx::query_scalar(
            "select exists (select 1 from e_leave_ledger_entry where tenant_id = $1 and allocation = $2 \
             and transaction_type in ('Leave Application', 'Leave Encashment') group by transaction_id having sum(leaves) <> 0)",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .fetch_one(&mut **hc.tx)
        .await?;
        if taken {
            return Err(Error::invalid("leaves were taken or encashed from this allocation; cancel those first"));
        }
        Ok(())
    }

    /// Reverses the allocation and any expiry its carry forward caused on the previous one.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        reverse(hc, id(doc, "id").ok_or_else(|| Error::internal("allocation without id"))?).await
    }
}

/// Allocates a policy's leaves to employees for a period (submitting each allocation), skipping
/// employees who already have an allocation of the type in that period.
struct PolicyHook;

#[async_trait]
impl DocHook for PolicyHook {
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if name != "assign_allocations" {
            return Ok(None);
        }
        hc.ctx.require("LeaveAllocation", Action::Create)?;
        hc.ctx.require("LeaveAllocation", Action::Submit)?;
        let param_date = |k: &str| -> Result<NaiveDate> {
            let v = params.get(k).and_then(Value::as_str).ok_or_else(|| Error::field(k, "is required (YYYY-MM-DD)"))?;
            ferrum_kernel::value::parse_date(v).map_err(|m| Error::field(k, m))
        };
        let (from, to) = (param_date("from_date")?, param_date("to_date")?);
        if to < from {
            return Err(Error::field("to_date", "must not be before the from date"));
        }
        let carry = params.get("carry_forward").and_then(Value::as_bool).unwrap_or(false);
        let employees: Vec<Uuid> = match params.get("employees").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect(),
            None => {
                let company = params
                    .get("company")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Uuid>().ok())
                    .or(hc.ctx.default_company)
                    .ok_or_else(|| Error::field("company", "give the company (or the employees)"))?;
                let filters = vec![Filter::eq("company", company.to_string()), Filter::eq("status", "Active")];
                find(hc.kernel, hc.ctx, hc.tx, "Employee", filters, None, 100_000).await?.iter().filter_map(|e| id(e, "id")).collect()
            }
        };
        let policy = id(doc, "id").ok_or_else(|| Error::internal("policy without id"))?;
        let user = elevated(hc.ctx);
        let (mut created, mut skipped) = (0, 0);
        for emp in employees {
            for row in rows(doc, "leave_policy_details") {
                let Some(lt) = id(&row, "leave_type") else { continue };
                let exists: bool = sqlx::query_scalar(
                    "select exists (select 1 from e_leave_allocation where tenant_id = $1 and employee = $2 and leave_type = $3 and docstatus = 1 \
                     and from_date <= $5 and to_date >= $4)",
                )
                .bind(hc.ctx.tenant)
                .bind(emp)
                .bind(lt)
                .bind(from)
                .bind(to)
                .fetch_one(&mut **hc.tx)
                .await?;
                if exists {
                    skipped += 1;
                    continue;
                }
                let mut a = Doc::new();
                set(&mut a, "employee", emp.to_string());
                set(&mut a, "leave_type", lt.to_string());
                set(&mut a, "from_date", from.to_string());
                set(&mut a, "to_date", to.to_string());
                set(&mut a, "new_leaves_allocated", money(d(&row, "annual_allocation")));
                set(&mut a, "carry_forward", carry);
                set(&mut a, "leave_policy", policy.to_string());
                let made = hc.kernel.insert_tx(&user, hc.tx, "LeaveAllocation", a).await?;
                hc.kernel.submit_tx(&user, hc.tx, "LeaveAllocation", id(&made, "id").unwrap_or_default(), None).await?;
                created += 1;
            }
        }
        Ok(Some(json!({"created": created, "skipped": skipped})))
    }
}

// ───────────────────────────── applications ─────────────────────────────

struct ApplicationHook;

/// The days a leave marks on attendance (holidays only when the type counts them).
fn leave_marks(
    from: NaiveDate,
    to: NaiveDate,
    half: Option<NaiveDate>,
    holidays: &std::collections::BTreeSet<NaiveDate>,
    include: bool,
) -> Vec<(NaiveDate, &'static str)> {
    days(from, to).filter(|d| include || !holidays.contains(d)).map(|d| (d, if Some(d) == half { "Half Day" } else { "On Leave" })).collect()
}

#[async_trait]
impl DocHook for ApplicationHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let emp = stamp_company(hc, doc).await?;
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0 {
            set(doc, "status", "Open");
        }
        if !b(doc, "half_day") {
            set(doc, "half_day_date", Value::Null);
        } else if s(doc, "half_day_date").is_none() {
            let from = doc.get("from_date").cloned().unwrap_or_default();
            set(doc, "half_day_date", from);
        }
        let Some(emp) = emp else { return Ok(()) };
        if s(doc, "leave_approver").is_none() || crate::self_service(hc.ctx, "LeaveApplication") {
            let approver = default_approver(hc, &emp, "leave_approver").await?;
            set(doc, "leave_approver", approver.map(Value::String).unwrap_or_default());
        }
        let (Some(emp_id), Some(lt), Some(from), Some(to)) = (id(&emp, "id"), id(doc, "leave_type"), opt_date(doc, "from_date"), opt_date(doc, "to_date"))
        else {
            return Ok(());
        };
        if to < from {
            return Ok(());
        }
        let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
        let holidays = employee_holidays(hc.tx, hc.ctx.tenant, emp_id, from, to).await?;
        let total = leave_days(from, to, opt_date(doc, "half_day_date"), &holidays, b(&ltd, "include_holiday"));
        set(doc, "total_leave_days", money(total));
        let alloc = if b(&ltd, "is_lwp") { None } else { allocation_on(hc.tx, hc.ctx.tenant, emp_id, lt, from).await? };
        set(doc, "allocation", alloc.map(|a| json!(a.0.to_string())).unwrap_or_default());
        let balance = match alloc {
            Some(a) => Some(available(hc.tx, hc.ctx.tenant, emp_id, lt, a, id(doc, "id")).await?),
            None => None,
        };
        set(doc, "leave_balance", balance.map(money).unwrap_or_default());
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let (Some(lt), Some(from), Some(to)) = (id(doc, "leave_type"), opt_date(doc, "from_date"), opt_date(doc, "to_date")) else { return Ok(()) };
        if to < from {
            return Ok(());
        }
        let emp = employee_of(hc, doc).await?;
        let emp_id = id(&emp, "id").unwrap_or_default();
        let name = s(&emp, "employee_name").unwrap_or_default().to_string();
        let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
        let type_name = s(&ltd, "leave_type_name").unwrap_or_default().to_string();
        if b(&ltd, "disabled") {
            return Err(Error::field("leave_type", format!("{type_name} is disabled")));
        }
        let joined = opt_date(&emp, "date_of_joining");
        if joined.is_some_and(|j| from < j) || opt_date(&emp, "relieving_date").is_some_and(|r| to > r) {
            return Err(Error::field("from_date", format!("{name} is not employed on all of these days")));
        }
        let total = d(doc, "total_leave_days");
        if total <= Decimal::ZERO {
            return Err(Error::field("to_date", "every selected day is a holiday or weekly off; there is nothing to apply for"));
        }
        let clash: Option<String> = sqlx::query_scalar(
            "select coalesce(number, id::text) from e_leave_application where tenant_id = $1 and employee = $2 and id <> $3 \
             and docstatus < 2 and status in ('Open', 'Approved') and from_date <= $5 and to_date >= $4 limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(emp_id)
        .bind(id(doc, "id"))
        .bind(from)
        .bind(to)
        .fetch_optional(&mut **hc.tx)
        .await?;
        if let Some(n) = clash {
            return Err(Error::field("from_date", format!("{name} already applied for leave on these days ({n})")));
        }
        let after = doc_int(&ltd, "applicable_after");
        if after > 0
            && let Some(j) = joined
        {
            let worked_to = from.pred_opt().unwrap_or(from);
            let holidays = employee_holidays(hc.tx, hc.ctx.tenant, emp_id, j, worked_to).await?;
            let worked = if worked_to < j { 0 } else { working_days(j, worked_to, &holidays) };
            if worked < after {
                return Err(Error::field("leave_type", format!("{type_name} can be taken after {after} working days; {name} has worked {worked}")));
            }
        }
        let longest = doc_int(&ltd, "max_continuous_days_allowed");
        if longest > 0 && total > Decimal::from(longest) {
            return Err(Error::field("to_date", format!("{type_name} can be taken for at most {longest} days at a time")));
        }
        if b(&ltd, "is_lwp") {
            return Ok(());
        }
        let start = allocation_on(hc.tx, hc.ctx.tenant, emp_id, lt, from).await?;
        let end = allocation_on(hc.tx, hc.ctx.tenant, emp_id, lt, to).await?;
        match (start, end) {
            (None, _) | (_, None) => return Err(Error::field("leave_type", format!("{name} has no {type_name} allocated for these dates"))),
            (Some(a), Some(b)) if a.0 != b.0 => return Err(Error::field("to_date", "the leave spans two allocation periods; apply for each separately")),
            _ => {}
        }
        let balance = d(doc, "leave_balance");
        if total > balance && !b(&ltd, "allow_negative") {
            return Err(Error::field(
                "total_leave_days",
                format!("insufficient leave balance: {} {type_name} available, {} requested", balance.normalize(), total.normalize()),
            ));
        }
        Ok(())
    }

    /// New applications (and new approvers) are announced to the approver.
    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_some_and(|o| s(o, "leave_approver") == s(doc, "leave_approver")) {
            return Ok(());
        }
        let title = format!(
            "Leave application from {} ({} to {}) awaits your approval",
            s(doc, "employee_name").unwrap_or_default(),
            s(doc, "from_date").unwrap_or_default(),
            s(doc, "to_date").unwrap_or_default()
        );
        notify(hc, s(doc, "leave_approver"), &title, id(doc, "id").unwrap_or_default()).await
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        require_approver(hc.ctx, doc, s(doc, "leave_approver"), "leave")
    }

    /// Approval or rejection (the workflow submits the application in either case).
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("application without id"))?;
        let rejected = s(doc, "workflow_state") == Some("Rejected");
        let mut v = Doc::new();
        set(&mut v, "status", if rejected { "Rejected" } else { "Approved" });
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "LeaveApplication", me, v).await?;
        let title = format!("Your leave from {} was {}", s(doc, "from_date").unwrap_or_default(), if rejected { "rejected" } else { "approved" });
        notify(hc, s(doc, "employee_user"), &title, me).await?;
        if rejected {
            return Ok(());
        }
        let (emp, lt) = (id(doc, "employee").unwrap_or_default(), id(doc, "leave_type").unwrap_or_default());
        let (from, to) = (opt_date(doc, "from_date").unwrap_or_default(), opt_date(doc, "to_date").unwrap_or_default());
        let ltd = get(hc.kernel, hc.ctx, hc.tx, "LeaveType", lt).await?;
        let allocation = id(doc, "allocation");
        let e = LedgerEntry {
            employee: emp,
            leave_type: lt,
            kind: "Leave Application".into(),
            transaction: me,
            number: s(doc, "number").map(str::to_string),
            allocation,
            leaves: -d(doc, "total_leave_days"),
            from,
            to,
            carry_forward: false,
            lwp: b(&ltd, "is_lwp"),
            reversal: false,
        };
        post(hc, e).await?;
        let holidays = employee_holidays(hc.tx, hc.ctx.tenant, emp, from, to).await?;
        let marks = leave_marks(from, to, opt_date(doc, "half_day_date"), &holidays, b(&ltd, "include_holiday"));
        mark_leave(hc, doc, &marks).await?;
        if let Some(a) = allocation {
            sync_expiry(hc, a, me).await?;
        }
        Ok(())
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if hc.ctx.has_role("HR User") {
            return Ok(());
        }
        require_approver(hc.ctx, doc, s(doc, "leave_approver"), "leave")
    }

    /// Gives the days back and removes the leave from attendance.
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("application without id"))?;
        reverse(hc, me).await?;
        unmark_leave(hc, me).await?;
        if let Some(a) = id(doc, "allocation") {
            sync_expiry(hc, a, me).await?;
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "LeaveApplication", me, v).await
    }
}

fn doc_int(doc: &Doc, k: &str) -> i64 {
    doc.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// Approved leave-without-pay days of an employee within `[from, to]`, counted on the days that
/// are working days for payroll (`working_holidays`: holidays are paid working days).
pub(crate) async fn lwp_days(
    hc: &mut HookCtx<'_>,
    employee: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    holidays: &std::collections::BTreeSet<NaiveDate>,
    working_holidays: bool,
) -> Result<Decimal> {
    let apps: Vec<(NaiveDate, NaiveDate, Option<NaiveDate>, bool)> = sqlx::query_as(
        "select a.from_date, a.to_date, a.half_day_date, t.include_holiday from e_leave_application a \
         join e_leave_type t on t.tenant_id = a.tenant_id and t.id = a.leave_type \
         where a.tenant_id = $1 and a.employee = $2 and a.docstatus = 1 and a.status = 'Approved' and t.is_lwp \
         and a.from_date <= $4 and a.to_date >= $3",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut total = Decimal::ZERO;
    for (a, b_, half, include) in apps {
        for day in days(a.max(from), b_.min(to)) {
            let holiday = holidays.contains(&day);
            if holiday && (!include || !working_holidays) {
                continue;
            }
            total += if Some(day) == half { Decimal::new(5, 1) } else { Decimal::ONE };
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leave_marks_follow_holidays_and_half_days() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let hol = [d("2026-10-03"), d("2026-10-04")].into_iter().collect();
        let marks = leave_marks(d("2026-10-02"), d("2026-10-05"), Some(d("2026-10-05")), &hol, false);
        assert_eq!(marks, vec![(d("2026-10-02"), "On Leave"), (d("2026-10-05"), "Half Day")]);
        assert_eq!(leave_marks(d("2026-10-02"), d("2026-10-05"), None, &hol, true).len(), 4);
    }
}
