//! The full and final statement of an employee who leaves: what is paid to them (their open
//! expense claims, and lines added such as gratuity or notice pay) and what is recovered from them
//! (their unsettled advances, and lines added), gathered from their records. One journal entry
//! settles it: it debits what is paid, credits what is recovered and pays the difference from the
//! bank (or takes it in), and submitting it settles each claim and advance the statement names, as
//! their own payments do; cancelling it undoes that.

use crate::expense::{advance_moved, bank_param, claim_paid, net_on, payment_entry};
use crate::{lookup, stamp_company};
use async_trait::async_trait;
use chrono::NaiveDate;
use ferrum_domain::util::{company, d, elevated, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, HookCtx, HookRegistry, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// The journal entry's `hr_reference_type` for a statement.
pub(crate) const KIND: &str = "Full and Final Statement";
const ENTITY: &str = "FullAndFinalStatement";
const CLAIM: &str = "Expense Claim";
const ADVANCE: &str = "Employee Advance";

pub(crate) fn register(h: &mut HookRegistry) {
    h.register(ENTITY, Arc::new(StatementHook)).register("EmployeeSeparation", Arc::new(FromSeparation));
}

fn docstatus(doc: &Doc) -> i64 {
    doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0)
}

/// The document a gathered line names, read for settling (locked when `write`), with what it is
/// called in messages. It must be submitted and the statement's employee's.
async fn referenced(hc: &mut HookCtx<'_>, st: &Doc, line: &Doc, write: bool) -> Result<Option<(Doc, String)>> {
    let entity = match s(line, "reference_type") {
        Some(CLAIM) => "ExpenseClaim",
        Some(ADVANCE) => "EmployeeAdvance",
        _ => return Ok(None),
    };
    let target =
        id(line, "reference").ok_or_else(|| Error::invalid(format!("a line for {} names no document", s(line, "reference_number").unwrap_or("a document"))))?;
    let doc = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, entity, target, write).await?;
    let label = format!("{} {}", s(line, "reference_type").unwrap_or_default(), s(&doc, "number").unwrap_or_default());
    if docstatus(&doc) != 1 {
        return Err(Error::invalid(format!("{label} is not submitted")));
    }
    if id(&doc, "employee") != id(st, "employee") {
        return Err(Error::invalid(format!("{label} is another employee's")));
    }
    Ok(Some((doc, label)))
}

/// Settles (`sign` 1) or unsettles (−1) every claim and advance the statement names: a claim is
/// paid its line's amount, an advance has it returned. Checks only, unless `write`.
async fn settle_lines(hc: &mut HookCtx<'_>, st: &Doc, sign: Decimal, write: bool) -> Result<()> {
    for line in rows(st, "payables").iter().chain(rows(st, "receivables").iter()) {
        let Some((doc, label)) = referenced(hc, st, line, write).await? else { continue };
        let delta = sign * d(line, "amount");
        if s(line, "reference_type") == Some(CLAIM) {
            claim_paid(hc, &doc, &label, delta, write).await?;
        } else {
            advance_moved(hc, &doc, &label, "return_amount", delta, write).await?;
        }
    }
    Ok(())
}

/// A journal entry referencing a statement: on each of the statement's accounts it posts what the
/// statement says (what is paid debited, what is recovered credited), the rest being the bank.
/// Submitting it settles the statement's claims and advances and marks it paid; cancelling it
/// undoes both.
pub(crate) async fn settle(hc: &mut HookCtx<'_>, je: &Doc, target: Uuid, sign: Decimal, write: bool) -> Result<()> {
    let st = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, ENTITY, target, write).await?;
    let label = format!("{KIND} {}", s(&st, "number").unwrap_or_default());
    if docstatus(&st) != 1 {
        return Err(Error::invalid(format!("{label} is not submitted")));
    }
    if id(&st, "company") != id(je, "company") {
        return Err(Error::invalid(format!("{label} belongs to another company")));
    }
    let paying = sign > Decimal::ZERO;
    if s(&st, "status") != Some(if paying { "Unpaid" } else { "Paid" }) {
        return Err(Error::invalid(if paying { format!("{label} is already paid") } else { format!("{label} is not paid") }));
    }
    let mut want: HashMap<Uuid, Decimal> = HashMap::new();
    for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
        for r in rows(&st, table) {
            let account = id(&r, "account").ok_or_else(|| Error::invalid(format!("a line of {label} has no account")))?;
            *want.entry(account).or_default() += side * d(&r, "amount");
        }
    }
    if want.iter().any(|(account, net)| net_on(je, Some(*account)) != *net) {
        return Err(Error::invalid(format!("the entry must debit what {label} pays and credit what it recovers, on their accounts")));
    }
    settle_lines(hc, &st, sign, write).await?;
    if write {
        let mut v = Doc::new();
        set(&mut v, "status", if paying { "Paid" } else { "Unpaid" });
        set(&mut v, "journal_entry", if paying { json!(s(je, "id").unwrap_or_default()) } else { Value::Null });
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, ENTITY, target, v).await?;
    }
    Ok(())
}

/// What is open each way for the statement's employee: their approved claims not yet paid, and
/// their paid advances not yet claimed or returned. Lines added by hand are kept as they are.
async fn outstanding(hc: &mut HookCtx<'_>, st: &Doc) -> Result<(Vec<Value>, Vec<Value>)> {
    let employee = id(st, "employee").ok_or_else(|| Error::field("employee", "is required"))?;
    let claims: Vec<(Uuid, Option<String>, Option<Uuid>, Decimal, NaiveDate)> = sqlx::query_as(
        "select id, number, payable_account, outstanding_amount, posting_date from e_expense_claim \
         where tenant_id = $1 and employee = $2 and docstatus = 1 and status = 'Unpaid' and outstanding_amount > 0 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?;
    let advances: Vec<(Uuid, Option<String>, Option<Uuid>, Decimal, Option<String>)> = sqlx::query_as(
        "select id, number, advance_account, open, purpose from (select id, number, advance_account, purpose, posting_date, created_at, \
         coalesce(paid_amount, 0) - coalesce(claimed_amount, 0) - coalesce(return_amount, 0) as open from e_employee_advance \
         where tenant_id = $1 and employee = $2 and docstatus = 1) a where open > 0 order by posting_date, created_at",
    )
    .bind(hc.ctx.tenant)
    .bind(employee)
    .fetch_all(&mut **hc.tx)
    .await?;
    let by_hand = |table: &str| rows(st, table).into_iter().filter(|r| s(r, "reference_type").is_none()).map(Value::Object);
    let mut payables: Vec<Value> = claims
        .into_iter()
        .map(|(claim, number, account, amount, posted)| {
            json!({"component": "Expense claim", "reference_type": CLAIM, "reference": claim.to_string(), "reference_number": number,
                   "account": account.map(|a| a.to_string()), "amount": money(amount), "remark": format!("Approved, posted {posted}")})
        })
        .collect();
    payables.extend(by_hand("payables"));
    let mut receivables: Vec<Value> = advances
        .into_iter()
        .map(|(advance, number, account, amount, purpose)| {
            json!({"component": "Employee advance", "reference_type": ADVANCE, "reference": advance.to_string(), "reference_number": number,
                   "account": account.map(|a| a.to_string()), "amount": money(amount), "remark": purpose})
        })
        .collect();
    receivables.extend(by_hand("receivables"));
    Ok((payables, receivables))
}

/// Fills a draft statement's lines with what is open now. Written with engine rights: what a line
/// refers to is the engine's to set, never a person's.
async fn gather(hc: &mut HookCtx<'_>, st: &Doc) -> Result<Doc> {
    let me = id(st, "id").ok_or_else(|| Error::internal("statement without id"))?;
    let (payables, receivables) = outstanding(hc, st).await?;
    let mut patch = Doc::new();
    patch.insert("payables".into(), Value::Array(payables));
    patch.insert("receivables".into(), Value::Array(receivables));
    hc.kernel.update_tx(&elevated(hc.ctx), hc.tx, ENTITY, me, patch, None).await
}

struct StatementHook;

#[async_trait]
impl DocHook for StatementHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if docstatus(doc) == 0 {
            set(doc, "status", "Draft");
        }
        let precision = match id(doc, "company") {
            Some(cid) => {
                let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
                set(doc, "currency", co.currency_id.to_string());
                co.precision
            }
            None => 2,
        };
        let mut totals = [Decimal::ZERO; 2];
        for (i, table) in ["payables", "receivables"].into_iter().enumerate() {
            let mut lines = rows(doc, table);
            for r in lines.iter_mut() {
                let amount = round(d(r, "amount"), precision);
                r.insert("amount".into(), money(amount));
                totals[i] += amount;
            }
            doc.insert(table.into(), Value::Array(lines.into_iter().map(Value::Object).collect()));
        }
        set(doc, "total_payable", money(totals[0]));
        set(doc, "total_receivable", money(totals[1]));
        set(doc, "net_payable", money(totals[0] - totals[1]));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        match lookup(hc, "EmployeeSeparation", id(doc, "separation")).await? {
            Some(sep) if id(&sep, "employee") != id(doc, "employee") => errors.push(FieldError::new("separation", "is another employee's")),
            Some(sep) if docstatus(&sep) != 1 => errors.push(FieldError::new("separation", "is not submitted")),
            Some(_) => {
                let other: Option<String> = sqlx::query_scalar(
                    "select coalesce(number, 'a draft') from e_full_and_final_statement where tenant_id = $1 and separation = $2 and docstatus < 2 and id <> $3",
                )
                .bind(hc.ctx.tenant)
                .bind(id(doc, "separation"))
                .bind(id(doc, "id").unwrap_or_default())
                .fetch_optional(&mut **hc.tx)
                .await?;
                if let Some(n) = other {
                    errors.push(FieldError::new("separation", format!("already has its statement ({n})")));
                }
            }
            None => {}
        }
        let co = id(doc, "company");
        for table in ["payables", "receivables"] {
            for (i, r) in rows(doc, table).iter().enumerate() {
                if let Some(a) = lookup(hc, "Account", id(r, "account")).await?
                    && id(&a, "company") != co
                {
                    errors.push(FieldError::new("account", "belongs to another company").in_row(table, i));
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        // What it names is still open for what it says.
        settle_lines(hc, doc, Decimal::ONE, false).await?;
        set(doc, "status", "Unpaid");
        Ok(())
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if s(doc, "status") == Some("Paid") {
            return Err(Error::invalid("this statement has been paid; cancel its journal entry first"));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, ENTITY, id(doc, "id").unwrap_or_default(), v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "get_outstanding" => Ok(Some(Value::Object(gather(hc, doc).await?))),
            "make_payment_entry" => {
                let mut lines = Vec::new();
                for (table, side) in [("payables", Decimal::ONE), ("receivables", -Decimal::ONE)] {
                    for r in rows(doc, table) {
                        let account = id(&r, "account").ok_or_else(|| Error::invalid("a line has no account"))?;
                        lines.push((account, side * d(&r, "amount")));
                    }
                }
                let net = d(doc, "net_payable");
                if !net.is_zero() {
                    let bank = bank_param(hc, params, id(doc, "company").unwrap_or_default(), None).await?;
                    lines.push((bank, -net));
                }
                let remark = format!("Full and final settlement {} with {}", s(doc, "number").unwrap_or_default(), s(doc, "employee_name").unwrap_or_default());
                payment_entry(hc, doc, KIND, &lines, params, remark).await.map(Some)
            }
            _ => Ok(None),
        }
    }
}

/// A submitted separation's Create Full and Final Statement: its statement, made now with what is
/// open, or the one it has.
struct FromSeparation;

#[async_trait]
impl DocHook for FromSeparation {
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "make_full_and_final" {
            return Ok(None);
        }
        let sep = id(doc, "id").ok_or_else(|| Error::internal("separation without id"))?;
        let existing: Option<Uuid> = sqlx::query_scalar("select id from e_full_and_final_statement where tenant_id = $1 and separation = $2 and docstatus < 2")
            .bind(hc.ctx.tenant)
            .bind(sep)
            .fetch_optional(&mut **hc.tx)
            .await?;
        if let Some(st) = existing {
            return Ok(Some(json!({"entity": ENTITY, "id": st.to_string(), "message": "This separation already has its statement"})));
        }
        hc.ctx.require(ENTITY, Action::Create)?;
        let mut st = Doc::new();
        set(&mut st, "employee", s(doc, "employee").unwrap_or_default());
        set(&mut st, "separation", sep.to_string());
        let made = hc.kernel.insert_tx(hc.ctx, hc.tx, ENTITY, st).await?;
        let made = gather(hc, &made).await?;
        let lines = rows(&made, "payables").len() + rows(&made, "receivables").len();
        Ok(Some(json!({
            "entity": ENTITY,
            "id": s(&made, "id").unwrap_or_default(),
            "message": match lines {
                0 => "The statement is made: nothing is open either way; add what is owed".to_string(),
                1 => "The statement is made with the one amount open".to_string(),
                n => format!("The statement is made with the {n} amounts open"),
            },
        })))
    }
}
