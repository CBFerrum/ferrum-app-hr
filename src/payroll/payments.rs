//! What has been paid of each salary slip of a payroll run, and what a bank payment pays.
//!
//! A slip's paid amount is the sum of its rows in `pay_salary_payment`, each naming what paid it: a
//! bank entry of a payroll entry, a full and final statement's entry, or a later slip that took back
//! an excess (negative). A correction takes over the rows of the slip it replaces, so what is still
//! owed on a slip is its net pay less what was paid, and the payroll payable account holds the sum of
//! those across a company's run slips.
//!
//! A payroll entry's bank payment pays, for each employee it lists, what is still owed on its own
//! slips and on their slips carried from earlier runs (a correction, a salary released from hold),
//! leaving out held salaries. The bank file lists the same, so their totals agree.

use ferrum_domain::util::{money, set, sys};
use ferrum_kernel::{Doc, HookCtx, Result};
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

/// What a bank payment pays one slip.
pub(crate) struct Due {
    pub slip: Uuid,
    pub employee: Uuid,
    pub number: Option<String>,
    pub name: Option<String>,
    pub amount: Decimal,
    /// The paying run's own slip, not one carried from an earlier run.
    pub own: bool,
}

/// What a bank payment of `run` pays now.
pub(crate) async fn due(hc: &mut HookCtx<'_>, run: Uuid) -> Result<Vec<Due>> {
    let rows: Vec<(Uuid, Uuid, Option<String>, Option<String>, Decimal, bool)> = sqlx::query_as(
        "select s.id, s.employee, s.number, s.employee_name, coalesce(s.net_pay, 0) - coalesce(s.paid_amount, 0), s.payroll_entry = $2 \
         from e_salary_slip s where s.tenant_id = $1 and s.docstatus = 1 and not coalesce(s.held, false) \
         and coalesce(s.net_pay, 0) - coalesce(s.paid_amount, 0) > 0 \
         and (s.payroll_entry = $2 or (s.payroll_entry is not null and (s.replaces is not null or s.released_on is not null) \
              and s.employee in (select r.employee from e_payroll_entry__employees r where r.tenant_id = $1 and r.parent_id = $2))) \
         order by s.employee_name, s.employee, s.start_date, s.number",
    )
    .bind(hc.ctx.tenant)
    .bind(run)
    .fetch_all(&mut **hc.tx)
    .await?;
    Ok(rows.into_iter().map(|(slip, employee, number, name, amount, own)| Due { slip, employee, number, name, amount, own }).collect())
}

/// Records what `source` paid each slip, then their paid amounts.
pub(crate) async fn allocate(hc: &mut HookCtx<'_>, source: Uuid, items: &[(Uuid, Decimal)]) -> Result<()> {
    for (slip, amount) in items.iter().filter(|i| !i.1.is_zero()) {
        sqlx::query(
            "insert into pay_salary_payment (tenant_id, source, salary_slip, amount) values ($1, $2, $3, $4) \
             on conflict (tenant_id, source, salary_slip) do update set amount = pay_salary_payment.amount + excluded.amount",
        )
        .bind(hc.ctx.tenant)
        .bind(source)
        .bind(slip)
        .bind(amount)
        .execute(&mut **hc.tx)
        .await?;
    }
    let slips: Vec<Uuid> = items.iter().map(|i| i.0).collect();
    refresh(hc, &slips).await
}

/// Undoes everything `source` paid, then the paid amounts of the slips it paid.
pub(crate) async fn unallocate(hc: &mut HookCtx<'_>, source: Uuid) -> Result<()> {
    let slips: Vec<Uuid> = sqlx::query_scalar("delete from pay_salary_payment where tenant_id = $1 and source = $2 returning salary_slip")
        .bind(hc.ctx.tenant)
        .bind(source)
        .fetch_all(&mut **hc.tx)
        .await?;
    refresh(hc, &slips).await
}

/// A correction takes over what was paid of the slip it replaces.
pub(crate) async fn take_over(hc: &mut HookCtx<'_>, from: Uuid, to: Uuid) -> Result<()> {
    sqlx::query("update pay_salary_payment set salary_slip = $3 where tenant_id = $1 and salary_slip = $2")
        .bind(hc.ctx.tenant)
        .bind(from)
        .bind(to)
        .execute(&mut **hc.tx)
        .await?;
    refresh(hc, &[from, to]).await
}

/// What the employee's run slips paid too much, oldest first: (slip, excess).
pub(crate) async fn excess(hc: &mut HookCtx<'_>, employee: Uuid, except: Option<Uuid>) -> Result<Vec<(Uuid, Decimal)>> {
    Ok(sqlx::query_as(
        "select id, coalesce(paid_amount, 0) - coalesce(net_pay, 0) from e_salary_slip where tenant_id = $1 and employee = $2 and docstatus = 1 \
         and payroll_entry is not null and coalesce(paid_amount, 0) > coalesce(net_pay, 0) and id is distinct from $3 order by start_date, number",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(except)
    .fetch_all(&mut **hc.tx)
    .await?)
}

/// Run slips of the employee a full and final statement settles: held, carried from an earlier run
/// (a correction, a release) with something still owed, or paid too much. (slip, number, from, to,
/// still owed (negative: to recover), held).
#[allow(clippy::type_complexity)]
pub(crate) async fn open_for_leaver(
    hc: &mut HookCtx<'_>,
    employee: Uuid,
) -> Result<Vec<(Uuid, Option<String>, chrono::NaiveDate, chrono::NaiveDate, Decimal, bool)>> {
    Ok(sqlx::query_as(
        "select id, number, start_date, end_date, coalesce(net_pay, 0) - coalesce(paid_amount, 0), coalesce(held, false) from e_salary_slip \
         where tenant_id = $1 and employee = $2 and docstatus = 1 and payroll_entry is not null and coalesce(net_pay, 0) <> coalesce(paid_amount, 0) \
         and (coalesce(held, false) or replaces is not null or released_on is not null or coalesce(paid_amount, 0) > coalesce(net_pay, 0)) \
         order by start_date, number",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?)
}

/// The paid amounts of `slips` from their rows, and the status of the runs they belong to: Paid
/// once every slip of the run is paid.
pub(crate) async fn refresh(hc: &mut HookCtx<'_>, slips: &[Uuid]) -> Result<()> {
    let sctx = sys(hc.ctx);
    let mut runs = BTreeSet::new();
    for slip in slips.iter().collect::<BTreeSet<_>>() {
        let (paid, run): (Decimal, Option<Uuid>) = sqlx::query_as(
            "select coalesce((select sum(amount) from pay_salary_payment p where p.tenant_id = s.tenant_id and p.salary_slip = s.id), 0), s.payroll_entry \
             from e_salary_slip s where s.tenant_id = $1 and s.id = $2",
        )
        .bind(hc.ctx.tenant)
        .bind(slip)
        .fetch_one(&mut **hc.tx)
        .await?;
        let mut v = Doc::new();
        set(&mut v, "paid_amount", money(paid));
        hc.kernel.set_values_tx(&sctx, hc.tx, "SalarySlip", *slip, v).await?;
        runs.extend(run);
    }
    for run in runs {
        run_status(hc, run).await?;
    }
    Ok(())
}

/// A run whose slips are submitted is Paid once each of them is paid (a held salary is not).
pub(crate) async fn run_status(hc: &mut HookCtx<'_>, run: Uuid) -> Result<()> {
    let (submitted, docstatus, settled): (Option<bool>, i16, Option<bool>) = sqlx::query_as(
        "select pe.salary_slips_submitted, pe.docstatus, (select bool_and(coalesce(s.net_pay, 0) <= coalesce(s.paid_amount, 0)) \
         from e_salary_slip s where s.tenant_id = pe.tenant_id and s.payroll_entry = pe.id and s.docstatus = 1) from e_payroll_entry pe where pe.tenant_id = $1 and pe.id = $2",
    )
    .bind(hc.ctx.tenant)
    .bind(run)
    .fetch_one(&mut **hc.tx)
    .await?;
    if docstatus != 1 || submitted != Some(true) {
        return Ok(());
    }
    let mut v = Doc::new();
    v.insert("status".into(), json!(if settled.unwrap_or(true) { "Paid" } else { "Slips Submitted" }));
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "PayrollEntry", run, v).await
}

/// "A and B", "A, B and C".
pub(crate) fn names(list: &[String]) -> String {
    match list {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
