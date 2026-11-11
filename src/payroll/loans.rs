//! Employee loans (with the connector to ERP). A loan is paid from the bank on its day (the loan
//! account, an asset, debited against the loan) and repaid by each monthly salary slip from its
//! first month: over a number of months or an amount a month, with interest on the balance left
//! each month when a yearly rate is set.
//!
//! What a slip deducts is worked out from what is submitted every time: the plan's balance after
//! the month (the loan less each month's planned instalment, re-spread over the months left after a
//! repayment outside salary, a deferred month skipped) against the balance actually left (the loan
//! less what submitted slips, repayments, a write-off and a full and final statement took). The
//! difference is the principal due, with a month's interest on the balance left. So a slip that
//! could not take it all leaves the rest to the next one, and a slip corrected or cancelled gives
//! its instalment back to the next slip that is made: nothing lost, nothing deducted twice.

use crate::expense::bank_param;
use crate::people::company_account;
use crate::{lookup, opt_date, stamp_company, today};
use async_trait::async_trait;
use chrono::{Datelike, Duration, Months, NaiveDate};
use ferrum_app_erp::ledger::{self, Entry, Line};
use ferrum_domain::util::{company, d, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Doc, DocHook, Error, HookCtx, Result};
use rust_decimal::{Decimal, MathematicalOps};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) struct LoanHook;

const ENTITY: &str = "EmployeeLoan";
/// Events that take from the balance.
const REDUCING: [&str; 3] = ["Repayment", "Write-off", "Settled by Statement"];

fn month_of(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

fn next_month(m: NaiveDate) -> NaiveDate {
    m.checked_add_months(Months::new(1)).unwrap_or(m)
}

/// A loan's repayment terms.
pub(crate) struct Terms {
    pub amount: Decimal,
    /// The first month repaid (its first day).
    pub first: NaiveDate,
    /// Interest a month, as a fraction.
    pub rate: Decimal,
    /// Months to repay over, or else the amount a month.
    pub periods: Option<i64>,
    pub per_period: Decimal,
    pub precision: u32,
}

impl Terms {
    fn of(loan: &Doc, precision: u32) -> Terms {
        let periods =
            (s(loan, "repayment_method") != Some("Amount per Period")).then(|| loan.get("repayment_periods").and_then(Value::as_i64).unwrap_or(1).max(1));
        Terms {
            amount: d(loan, "loan_amount"),
            first: month_of(opt_date(loan, "repayment_start_date").unwrap_or_default()),
            rate: d(loan, "interest_rate") / Decimal::from(1200),
            periods,
            per_period: d(loan, "repayment_amount"),
            precision,
        }
    }

    fn interest(&self, balance: Decimal) -> Decimal {
        round(balance * self.rate, self.precision)
    }
}

/// The balance the plan leaves after month `m`: each month repays its instalment (over the months
/// left, or the amount a month, less that month's interest), a deferred month repays nothing, and
/// what was taken outside salary by the end of a month comes off before its instalment.
pub(crate) fn expected_after(t: &Terms, reductions: &[(NaiveDate, Decimal)], deferred: &[NaiveDate], m: NaiveDate) -> Decimal {
    let (mut e, mut used, mut month, mut i) = (t.amount, 0i64, t.first, 0);
    while month <= m && e > Decimal::ZERO {
        let end = next_month(month) - Duration::days(1);
        while i < reductions.len() && reductions[i].0 <= end {
            e = (e - reductions[i].1).max(Decimal::ZERO);
            i += 1;
        }
        if !deferred.contains(&month) {
            let interest = t.interest(e);
            let principal = match t.periods {
                Some(n) if n - used <= 1 => e,
                Some(n) if t.rate.is_zero() => round(e / Decimal::from(n - used), t.precision),
                Some(n) => {
                    let factor = Decimal::ONE - Decimal::ONE / (Decimal::ONE + t.rate).powi(n - used);
                    round(e * t.rate / factor, t.precision) - interest
                }
                None => (t.per_period - interest).max(Decimal::ZERO),
            };
            e -= principal.min(e).max(Decimal::ZERO);
            used += 1;
        }
        month = next_month(month);
    }
    e
}

/// Repayments outside salary by day, and the months deferred.
fn events_of(loan: &Doc) -> (Vec<(NaiveDate, Decimal)>, Vec<NaiveDate>) {
    let mut reductions = Vec::new();
    let mut deferred = Vec::new();
    for e in rows(loan, "events") {
        match s(&e, "event_type") {
            Some("Deferral") => deferred.extend(opt_date(&e, "period_start").map(month_of)),
            Some(k) if REDUCING.contains(&k) => reductions.push((opt_date(&e, "event_date").unwrap_or_default(), d(&e, "amount"))),
            _ => {}
        }
    }
    reductions.sort();
    (reductions, deferred)
}

/// The principal still owed: the loan less what submitted slips (other than `except`) and what was
/// taken outside salary repaid.
async fn balance(hc: &mut HookCtx<'_>, loan: &Doc, except: Option<Uuid>) -> Result<Decimal> {
    let by_slips: Decimal = sqlx::query_scalar(
        "select coalesce(sum(r.principal), 0) from e_salary_slip__loans r join e_salary_slip p on p.tenant_id = r.tenant_id and p.id = r.parent_id \
         where r.tenant_id = $1 and r.loan = $2 and p.docstatus = 1 and p.id is distinct from $3",
    )
    .bind(hc.ctx.tenant)
    .bind(id(loan, "id"))
    .bind(except)
    .fetch_one(&mut **hc.tx)
    .await?;
    let outside: Decimal = events_of(loan).0.iter().map(|r| r.1).sum();
    Ok(d(loan, "loan_amount") - by_slips - outside)
}

/// What each of the employee's loans asks of the slip `me` for the month its period ends in:
/// (loan, principal, interest). None for a month deferred, before the first or once repaid.
pub(crate) async fn due(hc: &mut HookCtx<'_>, employee: Uuid, me: Option<Uuid>, end: NaiveDate, precision: u32) -> Result<Vec<(Uuid, Decimal, Decimal)>> {
    let loans: Vec<Uuid> = sqlx::query_scalar(
        "select id from e_employee_loan where tenant_id = $1 and employee = $2 and docstatus = 1 and status in ('Repaying', 'Repaid') \
         and posting_date <= $3 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .bind(end)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut out = Vec::new();
    for l in loans {
        let loan = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, ENTITY, l, false).await?;
        let terms = Terms::of(&loan, precision);
        let m = month_of(end);
        let (reductions, deferred) = events_of(&loan);
        if m < terms.first || deferred.contains(&m) {
            continue;
        }
        let left = balance(hc, &loan, me).await?;
        if left <= Decimal::ZERO {
            continue;
        }
        let principal = (left - expected_after(&terms, &reductions, &deferred, m)).max(Decimal::ZERO);
        out.push((l, principal, terms.interest(left)));
    }
    Ok(out)
}

/// The slip's loan rows: each loan's instalment, interest first, never more than `available`.
/// Returns the rows and their total.
pub(crate) async fn slip_rows(
    hc: &mut HookCtx<'_>,
    employee: Uuid,
    me: Option<Uuid>,
    end: NaiveDate,
    precision: u32,
    available: Decimal,
) -> Result<(Vec<Value>, Decimal)> {
    let mut left = available.max(Decimal::ZERO);
    let (mut out, mut total) = (Vec::new(), Decimal::ZERO);
    for (i, (loan, principal, interest)) in due(hc, employee, me, end, precision).await?.into_iter().enumerate() {
        let owed = principal + interest;
        if owed.is_zero() {
            continue;
        }
        let take = owed.min(left);
        let paid_interest = interest.min(take);
        left -= take;
        total += take;
        let note = if take < owed {
            format!("The pay allowed {} of the {} due; the rest is due with the next instalment", take.normalize(), owed.normalize())
        } else {
            String::new()
        };
        out.push(json!({"id": Uuid::now_v7().to_string(), "idx": i, "loan": loan.to_string(), "principal": money(take - paid_interest),
                        "interest": money(paid_interest), "total": money(take), "due": money(owed), "note": note}));
    }
    Ok((out, total))
}

/// The ledger lines of a slip's loan repayments (`sign` −1: taken back): the loan account credited
/// with the principal against the loan, the interest account with the interest.
pub(crate) async fn accrual_lines(hc: &mut HookCtx<'_>, slip: &Doc, currency: &str, sign: Decimal, name: &str) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for r in rows(slip, "loans") {
        let Some(loan) = lookup(hc, ENTITY, id(&r, "loan")).await? else { continue };
        let lid = id(&loan, "id").unwrap_or_default();
        let number = s(&loan, "number").unwrap_or_default().to_string();
        let account = id(&loan, "loan_account").ok_or_else(|| Error::invalid(format!("employee loan {number} has no loan account")))?;
        lines.push(Line::new(account, -sign * d(&r, "principal"), currency).against(ENTITY, lid).remarks(format!("Loan {number} repaid by {name}")));
        if !d(&r, "interest").is_zero() {
            let interest = id(&loan, "interest_income_account").ok_or_else(|| Error::invalid(format!("employee loan {number} has no interest account")))?;
            lines.push(Line::new(interest, -sign * d(&r, "interest"), currency).remarks(format!("Interest on loan {number} from {name}")));
        }
    }
    Ok(lines)
}

/// Brings the loans a slip repaid up to date after it is submitted or cancelled.
pub(crate) async fn slip_changed(hc: &mut HookCtx<'_>, slip: &Doc) -> Result<()> {
    if hc.ctx.schema.entity(ENTITY).is_none() {
        return Ok(());
    }
    for r in rows(slip, "loans") {
        if let Some(loan) = id(&r, "loan") {
            refresh(hc, loan).await?;
        }
    }
    Ok(())
}

/// The loan's repaid amounts, balance, status and schedule: the instalments submitted slips
/// deducted, the months deferred, and the instalments still due as the plan has them now.
pub(crate) async fn refresh(hc: &mut HookCtx<'_>, loan_id: Uuid) -> Result<()> {
    let sctx = sys(hc.ctx);
    let loan = hc.kernel.get_tx(&sctx, hc.tx, ENTITY, loan_id, false).await?;
    if d(&loan, "docstatus") != Decimal::ONE {
        return Ok(());
    }
    let co = company(hc.kernel, hc.ctx, hc.tx, id(&loan, "company").unwrap_or_default()).await?;
    let terms = Terms::of(&loan, co.precision);
    let deducted: Vec<(NaiveDate, Uuid, Decimal, Decimal, Option<String>)> = sqlx::query_as(
        "select p.end_date, p.id, r.principal, r.interest, r.note from e_salary_slip__loans r join e_salary_slip p on p.tenant_id = r.tenant_id \
         and p.id = r.parent_id where r.tenant_id = $1 and r.loan = $2 and p.docstatus = 1 order by p.end_date, p.number",
    )
    .bind(hc.ctx.tenant)
    .bind(loan_id)
    .fetch_all(&mut **hc.tx)
    .await?;
    let (reductions, deferred) = events_of(&loan);
    let repaid: Decimal = deducted.iter().map(|r| r.2).sum();
    let interest_paid: Decimal = deducted.iter().map(|r| r.3).sum();
    let left = terms.amount - repaid - reductions.iter().map(|r| r.1).sum::<Decimal>();
    let written_off = rows(&loan, "events").iter().any(|e| s(e, "event_type") == Some("Write-off"));
    let mut schedule: Vec<(NaiveDate, Value)> = Vec::new();
    for (end, slip, principal, interest, note) in &deducted {
        schedule.push((
            month_of(*end),
            json!({"period_start": month_of(*end).to_string(), "principal": money(*principal), "interest": money(*interest),
            "total": money(principal + interest), "status": "Deducted", "salary_slip": slip.to_string(), "note": note}),
        ));
    }
    for e in rows(&loan, "events").iter().filter(|e| s(e, "event_type") == Some("Deferral")) {
        let m = opt_date(e, "period_start").map(month_of).unwrap_or_default();
        schedule.push((m, json!({"period_start": m.to_string(), "status": "Deferred", "note": e.get("reason")})));
    }
    if !written_off {
        // From the month after the last one deducted or paid by a slip, the instalments still due.
        let paid_to = super::paid_through(hc, id(&loan, "employee")).await?;
        let mut m = terms.first.max(paid_to.map_or(terms.first, |p| month_of(p + Duration::days(1))));
        if let Some(last) = deducted.last() {
            m = m.max(next_month(month_of(last.0)));
        }
        let mut b = left;
        while b > Decimal::ZERO && schedule.len() < 600 {
            if !deferred.contains(&m) {
                let principal = (b - expected_after(&terms, &reductions, &deferred, m)).max(Decimal::ZERO).min(b);
                let interest = terms.interest(b);
                b -= principal;
                schedule.push((
                    m,
                    json!({"period_start": m.to_string(), "principal": money(principal), "interest": money(interest),
                    "total": money(principal + interest), "balance_after": money(b), "status": "Due"}),
                ));
            }
            m = next_month(m);
        }
    }
    schedule.sort_by_key(|r| r.0);
    let list: Vec<Doc> = schedule.into_iter().filter_map(|r| r.1.as_object().cloned()).collect();
    hc.kernel.set_child_rows_tx(&sctx, hc.tx, ENTITY, "schedule", loan_id, list).await?;
    let mut v = Doc::new();
    set(&mut v, "repaid_amount", money(repaid));
    set(&mut v, "interest_paid", money(interest_paid));
    set(&mut v, "balance", money(left));
    set(
        &mut v,
        "status",
        if written_off {
            "Written Off"
        } else if left <= Decimal::ZERO {
            "Repaid"
        } else {
            "Repaying"
        },
    );
    hc.kernel.set_values_tx(&sctx, hc.tx, ENTITY, loan_id, v).await
}

/// Adds an event to the loan (or, `remove`, takes off those naming `reference`), then brings it up to date.
pub(crate) async fn event(hc: &mut HookCtx<'_>, loan_id: Uuid, add: Option<Value>, remove: Option<&str>) -> Result<()> {
    let loan = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, ENTITY, loan_id, true).await?;
    const FIELDS: [&str; 8] = ["id", "event_date", "event_type", "amount", "period_start", "account", "reference", "reason"];
    let mut list: Vec<Doc> = rows(&loan, "events")
        .into_iter()
        .filter(|e| remove.is_none_or(|r| s(e, "reference") != Some(r)))
        .map(|e| e.into_iter().filter(|(k, _)| FIELDS.contains(&k.as_str())).collect())
        .collect();
    list.extend(add.and_then(|a| a.as_object().cloned()));
    hc.kernel.set_child_rows_tx(&sys(hc.ctx), hc.tx, ENTITY, "events", loan_id, list).await?;
    refresh(hc, loan_id).await
}

/// What is still owed on the employee's loans: (loan, number, loan account, balance).
pub(crate) async fn open_for(hc: &mut HookCtx<'_>, employee: Uuid) -> Result<Vec<(Uuid, Option<String>, Option<Uuid>, Decimal)>> {
    Ok(sqlx::query_as(
        "select id, number, loan_account, balance from e_employee_loan where tenant_id = $1 and employee = $2 and docstatus = 1 \
         and status = 'Repaying' and balance > 0 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?)
}

/// Posts one entry of the loan: `lines` are (account, signed amount, against the loan).
async fn post(hc: &mut HookCtx<'_>, loan: &Doc, on: NaiveDate, memo: String, lines: &[(Uuid, Decimal, bool)]) -> Result<()> {
    let co = company(hc.kernel, hc.ctx, hc.tx, id(loan, "company").unwrap_or_default()).await?;
    let lid = id(loan, "id").unwrap_or_default();
    let mut entry = Entry::new(co.id, on, ENTITY, lid, s(loan, "number").map(str::to_string));
    entry.memo = Some(memo);
    for (account, amount, against) in lines {
        let line = Line::new(*account, *amount, &co.currency_code);
        entry.lines.push(if *against { line.against(ENTITY, lid) } else { line });
    }
    ledger::post(hc.tx, hc.ctx, entry, co.precision).await?;
    Ok(())
}

fn date_param(params: &Value, name: &str) -> Result<Option<NaiveDate>> {
    match params.get(name).and_then(Value::as_str).filter(|v| !v.is_empty()) {
        Some(v) => Ok(Some(ferrum_kernel::value::parse_date(v).map_err(|m| Error::field(name, m))?)),
        None => Ok(None),
    }
}

fn text_param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty())
}

#[async_trait]
impl DocHook for LoanHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        set(doc, "status", "Draft");
        let Some(cid) = id(doc, "company") else { return Ok(()) };
        let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
        set(doc, "currency", co.currency_id.to_string());
        if id(doc, "payment_account").is_none() {
            set(doc, "payment_account", co.account("default_bank_account").map(|a| json!(a.to_string())).unwrap_or_default());
        }
        if id(doc, "loan_account").is_none() {
            set(doc, "loan_account", company_account(hc, cid, "pay_employee_loan_account", "employee loan account").await?.to_string());
        }
        if d(doc, "interest_rate") > Decimal::ZERO && id(doc, "interest_income_account").is_none() {
            set(doc, "interest_income_account", company_account(hc, cid, "pay_loan_interest_account", "loan interest account").await?.to_string());
        }
        set(doc, "balance", money(d(doc, "loan_amount")));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        for f in ["payment_account", "loan_account", "interest_income_account"] {
            crate::people::same_company(hc, doc, f, "Account").await?;
        }
        if let Some(a) = lookup(hc, "Account", id(doc, "loan_account")).await?
            && s(&a, "root_type") != Some("Asset")
        {
            return Err(Error::field("loan_account", "must be an asset account: the loan is owed to the company"));
        }
        if s(doc, "repayment_method") == Some("Amount per Period") {
            let first = round(d(doc, "loan_amount") * d(doc, "interest_rate") / Decimal::from(1200), 2);
            if d(doc, "repayment_amount") <= first {
                return Err(Error::field(
                    "repayment_amount",
                    format!("{} a month does not cover the first month's interest ({})", d(doc, "repayment_amount").normalize(), first.normalize()),
                ));
            }
        }
        Ok(())
    }

    /// The loan is paid: the loan account debited against it, the bank credited.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let bank = id(doc, "payment_account").ok_or_else(|| Error::field("payment_account", "set the account the loan is paid from"))?;
        let account = id(doc, "loan_account").ok_or_else(|| Error::field("loan_account", "is required"))?;
        let amount = d(doc, "loan_amount");
        let memo = format!("Loan to {}", s(doc, "employee_name").unwrap_or_default());
        post(hc, doc, opt_date(doc, "posting_date").unwrap_or_else(today), memo, &[(account, amount, true), (bank, -amount, false)]).await?;
        refresh(hc, id(doc, "id").unwrap_or_default()).await
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let repaid = d(&doc.clone(), "loan_amount") - balance(hc, doc, None).await?;
        if !repaid.is_zero() || d(doc, "interest_paid") > Decimal::ZERO {
            return Err(Error::invalid(format!(
                "{}'s loan {} has been repaid in part ({}); it can no longer be cancelled",
                s(doc, "employee_name").unwrap_or("the employee"),
                s(doc, "number").unwrap_or_default(),
                repaid.normalize()
            )));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").unwrap_or_default();
        let co = company(hc.kernel, hc.ctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
        ledger::reverse(hc.tx, hc.ctx, ENTITY, me, today(), co.precision).await?;
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, ENTITY, me, v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("loan without id"))?;
        let label = format!("{}'s loan {}", s(doc, "employee_name").unwrap_or("the employee"), s(doc, "number").unwrap_or_default());
        if !["defer_instalment", "repay", "write_off"].contains(&name) {
            return Ok(None);
        }
        if s(doc, "status") != Some("Repaying") {
            return Err(Error::invalid(format!("{label} is {}", s(doc, "status").unwrap_or("not open").to_lowercase())));
        }
        let left = balance(hc, doc, None).await?;
        let account = id(doc, "loan_account").ok_or_else(|| Error::field("loan_account", "is required"))?;
        let message = match name {
            "defer_instalment" => {
                let m = month_of(date_param(params, "period")?.ok_or_else(|| Error::field("period", "is required"))?);
                let reason = text_param(params, "reason").ok_or_else(|| Error::field("reason", "say why the instalment is deferred"))?;
                let first = month_of(opt_date(doc, "repayment_start_date").unwrap_or_default());
                if m < first {
                    return Err(Error::field("period", format!("{label} is repaid from {first}")));
                }
                if events_of(doc).1.contains(&m) {
                    return Err(Error::field("period", format!("the instalment of {m} is already deferred")));
                }
                let taken: Option<String> = sqlx::query_scalar(
                    "select coalesce(p.number, p.id::text) from e_salary_slip__loans r join e_salary_slip p on p.tenant_id = r.tenant_id and p.id = r.parent_id \
                     where r.tenant_id = $1 and r.loan = $2 and p.docstatus = 1 and date_trunc('month', p.end_date) = $3 limit 1",
                )
                .bind(hc.ctx.tenant)
                .bind(me)
                .bind(m.and_hms_opt(0, 0, 0))
                .fetch_optional(&mut **hc.tx)
                .await?;
                if let Some(n) = taken {
                    return Err(Error::field("period", format!("salary slip {n} already deducted the instalment of {m}; correct it to give it back")));
                }
                let e = json!({"event_date": today().to_string(), "event_type": "Deferral", "period_start": m.to_string(), "reason": reason});
                event(hc, me, Some(e), None).await?;
                format!("The instalment of {m} is deferred; {label} ends a month later")
            }
            "repay" => {
                let amount = match params.get("amount") {
                    None | Some(Value::Null) => left,
                    Some(Value::String(v)) if v.trim().is_empty() => left,
                    Some(v) => ferrum_kernel::value::parse_decimal(v).map_err(|m| Error::field("amount", m))?,
                };
                if amount <= Decimal::ZERO || amount > left {
                    return Err(Error::field("amount", format!("{label} has {} left to repay; give an amount up to it", left.normalize())));
                }
                let on = date_param(params, "posting_date")?.unwrap_or_else(today);
                let bank = bank_param(hc, params, id(doc, "company").unwrap_or_default(), None).await?;
                post(hc, doc, on, format!("{label} repaid outside salary"), &[(bank, amount, false), (account, -amount, true)]).await?;
                let e = json!({"event_date": on.to_string(), "event_type": "Repayment", "amount": money(amount), "account": bank.to_string(),
                               "reason": text_param(params, "reason")});
                event(hc, me, Some(e), None).await?;
                if amount == left { format!("{label} is paid off") } else { format!("{} repaid; {} left", amount.normalize(), (left - amount).normalize()) }
            }
            _ => {
                let to = params
                    .get("account")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<Uuid>().ok())
                    .ok_or_else(|| Error::field("account", "choose the account that bears it"))?;
                let reason = text_param(params, "reason").ok_or_else(|| Error::field("reason", "say why the loan is written off"))?;
                let on = date_param(params, "posting_date")?.unwrap_or_else(today);
                post(hc, doc, on, format!("{label} written off: {reason}"), &[(to, left, false), (account, -left, true)]).await?;
                let e = json!({"event_date": on.to_string(), "event_type": "Write-off", "amount": money(left), "account": to.to_string(), "reason": reason});
                event(hc, me, Some(e), None).await?;
                format!("{label} is written off: {} no longer owed", left.normalize())
            }
        };
        Ok(Some(json!({"message": message})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_repays_over_its_months() {
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let t =
            Terms { amount: Decimal::from(60000), first: day("2026-09-01"), rate: Decimal::ZERO, periods: Some(6), per_period: Decimal::ZERO, precision: 2 };
        assert_eq!(expected_after(&t, &[], &[], day("2026-09-01")), Decimal::from(50000));
        assert_eq!(expected_after(&t, &[], &[], day("2027-02-01")), Decimal::ZERO);
        // A month deferred repays nothing; the loan ends a month later.
        assert_eq!(expected_after(&t, &[], &[day("2026-10-01")], day("2026-10-01")), Decimal::from(50000));
        assert_eq!(expected_after(&t, &[], &[day("2026-10-01")], day("2027-02-01")), Decimal::from(10000));
        // 20,000 repaid in October: the 30,000 left after it over the five months left, October included.
        assert_eq!(expected_after(&t, &[(day("2026-10-15"), Decimal::from(20000))], &[], day("2026-11-01")), Decimal::from(18000));
        // With interest at 12% a year the instalments are equal and the last clears it.
        let i = Terms { rate: Decimal::new(1, 2), ..t };
        let after_one = expected_after(&i, &[], &[], day("2026-09-01"));
        assert_eq!(after_one, Decimal::new(5024710, 2), "an instalment of 10,352.90 less 600 interest");
        assert_eq!(expected_after(&i, &[], &[], day("2027-02-01")), Decimal::ZERO);
        let fixed = Terms { periods: None, per_period: Decimal::from(25000), rate: Decimal::ZERO, ..i };
        assert_eq!(expected_after(&fixed, &[], &[], day("2026-11-01")), Decimal::ZERO);
        assert_eq!(expected_after(&fixed, &[], &[], day("2026-10-01")), Decimal::from(10000));
    }
}
