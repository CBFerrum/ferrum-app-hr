//! Expense claims and employee advances. An approved claim posts its sanctioned expenses (and
//! taxes) against the employee payable account and the advances it settles. Payments are journal
//! entries that reference the claim, advance or payroll entry (`hr_reference`): submitting or
//! cancelling one moves the referenced document's paid amounts and status, so payments stay
//! ordinary, reversible vouchers.

use crate::people::company_account;
use crate::{default_approver, lookup, notify, opt_date, require_approver, stamp_company, stored, today};
use async_trait::async_trait;
use ferrum_domain::ledger::{self, Entry, Line};
use ferrum_domain::util::{company, d, elevated, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, HookCtx, HookRegistry, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// Expense claims and advances are the `hr_erp` connector's (they post to the ledger): these
/// hooks run only where it is installed.
pub(crate) fn register(h: &mut HookRegistry) {
    h.scoped(&[crate::HR_ERP.to_string()], |h| {
        h.register("ExpenseClaimType", Arc::new(ClaimTypeHook))
            .register("ExpenseClaim", Arc::new(ClaimHook))
            .register("EmployeeAdvance", Arc::new(AdvanceHook))
            .register("JournalEntry", Arc::new(PaymentHook));
    });
}

struct ClaimTypeHook;

#[async_trait]
impl DocHook for ClaimTypeHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        for (i, r) in rows(doc, "accounts").iter().enumerate() {
            let (Some(co), Some(acc)) = (id(r, "company"), id(r, "default_account")) else { continue };
            if !seen.insert(co) {
                errors.push(FieldError::new("company", "has two accounts").in_row("accounts", i));
            }
            if let Some(a) = lookup(hc, "Account", Some(acc)).await?
                && id(&a, "company") != Some(co)
            {
                errors.push(FieldError::new("default_account", "belongs to another company").in_row("accounts", i));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }
}

/// The expense account of a claim type for a company: its own, else the company's default.
async fn expense_account(
    hc: &mut HookCtx<'_>,
    cache: &mut HashMap<Uuid, Option<Uuid>>,
    expense_type: Uuid,
    co: &ferrum_domain::util::Company,
) -> Result<Option<Uuid>> {
    if let Some(a) = cache.get(&expense_type) {
        return Ok(*a);
    }
    let t = get(hc.kernel, hc.ctx, hc.tx, "ExpenseClaimType", expense_type).await?;
    let own = rows(&t, "accounts").iter().find(|r| id(r, "company") == Some(co.id)).and_then(|r| id(r, "default_account"));
    let account = own.or(co.account("default_expense_account"));
    cache.insert(expense_type, account);
    Ok(account)
}

/// An account field must point at an account of the given root type.
async fn root_type_is(hc: &mut HookCtx<'_>, doc: &Doc, field: &str, root: &str) -> Result<()> {
    match lookup(hc, "Account", id(doc, field)).await? {
        Some(a) if s(&a, "root_type") != Some(root) => {
            Err(Error::field(field, format!("must be a{} {} account", if root == "Asset" { "n" } else { "" }, root.to_lowercase())))
        }
        _ => Ok(()),
    }
}

/// What an employee advance can still settle: paid, less claimed and returned.
fn unsettled(adv: &Doc) -> Decimal {
    d(adv, "paid_amount") - d(adv, "claimed_amount") - d(adv, "return_amount")
}

fn advance_status(adv: &Doc) -> &'static str {
    let (paid, claimed, returned) = (d(adv, "paid_amount"), d(adv, "claimed_amount"), d(adv, "return_amount"));
    let settled = claimed + returned;
    if paid.is_zero() {
        "Unpaid"
    } else if settled.is_zero() || settled < paid {
        "Paid"
    } else if returned.is_zero() {
        "Claimed"
    } else if claimed.is_zero() {
        "Returned"
    } else {
        "Partly Claimed and Returned"
    }
}

/// Writes new paid/claimed/returned amounts onto an advance with its pending amount and status.
async fn update_advance(hc: &mut HookCtx<'_>, adv: &Doc, field: &str, value: Decimal) -> Result<()> {
    let mut a = adv.clone();
    set(&mut a, field, money(value));
    let advance = d(&a, "advance_amount");
    if d(&a, "paid_amount") > advance || unsettled(&a) < Decimal::ZERO || value < Decimal::ZERO {
        return Err(Error::invalid(format!(
            "employee advance {}: paid {}, claimed {} and returned {} do not add up (advance {})",
            s(&a, "number").unwrap_or_default(),
            d(&a, "paid_amount").normalize(),
            d(&a, "claimed_amount").normalize(),
            d(&a, "return_amount").normalize(),
            advance.normalize()
        )));
    }
    let mut v = Doc::new();
    v.insert(field.into(), money(value));
    v.insert("pending_amount".into(), money(unsettled(&a)));
    v.insert("status".into(), json!(advance_status(&a)));
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EmployeeAdvance", id(adv, "id").unwrap_or_default(), v).await
}

struct ClaimHook;

#[async_trait]
impl DocHook for ClaimHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let emp = stamp_company(hc, doc).await?;
        let draft = doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0;
        if draft {
            set(doc, "status", "Draft");
        }
        let (Some(emp), Some(cid)) = (emp, id(doc, "company")) else { return Ok(()) };
        let own = crate::self_service(hc.ctx, "ExpenseClaim");
        if s(doc, "expense_approver").is_none() || own {
            let approver = default_approver(hc, &emp, "expense_approver").await?;
            set(doc, "expense_approver", approver.map(Value::String).unwrap_or_default());
        }
        let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
        set(doc, "currency", co.currency_id.to_string());
        if id(doc, "payable_account").is_none() || own {
            let acc = company_account(hc, cid, "hr_employee_payable_account", "employee payable account").await?;
            set(doc, "payable_account", acc.to_string());
        }
        if id(doc, "cost_center").is_none() {
            set(doc, "cost_center", co.account("default_cost_center").map(|c| json!(c.to_string())).unwrap_or_default());
        }
        let p = co.precision;
        // A sanction that followed the claimed amount keeps following it until someone sets it.
        let old: HashMap<String, Doc> = match stored(hc, doc).await? {
            Some(o) => rows(&o, "expenses").into_iter().filter_map(|r| s(&r, "id").map(|i| (i.to_string(), r.clone()))).collect(),
            None => HashMap::new(),
        };
        let mut cache = HashMap::new();
        let mut expenses = rows(doc, "expenses");
        let (mut claimed, mut sanctioned) = (Decimal::ZERO, Decimal::ZERO);
        for r in expenses.iter_mut() {
            let amount = round(d(r, "amount"), p);
            let set_by_hand = r.get("sanctioned_amount").is_some_and(|v| !v.is_null());
            let following = s(r, "id")
                .and_then(|i| old.get(i))
                .is_some_and(|o| d(o, "sanctioned_amount") == d(o, "amount") && d(r, "sanctioned_amount") == d(o, "sanctioned_amount"));
            let sanction = if !set_by_hand || following { amount } else { round(d(r, "sanctioned_amount"), p) };
            set(r, "amount", money(amount));
            set(r, "sanctioned_amount", money(sanction));
            let account = match id(r, "expense_type") {
                Some(t) => expense_account(hc, &mut cache, t, &co).await?,
                None => None,
            };
            set(r, "default_account", account.map(|a| json!(a.to_string())).unwrap_or_default());
            claimed += amount;
            sanctioned += sanction;
        }
        let mut taxes = rows(doc, "taxes");
        let mut total_taxes = Decimal::ZERO;
        for t in taxes.iter_mut() {
            let rate = d(t, "rate");
            if rate > Decimal::ZERO {
                set(t, "tax_amount", money(round(sanctioned * rate / Decimal::ONE_HUNDRED, p)));
            }
            total_taxes += d(t, "tax_amount");
        }
        let mut advances = rows(doc, "advances");
        let mut allocated = Decimal::ZERO;
        for a in advances.iter_mut() {
            if draft && let Some(adv) = lookup(hc, "EmployeeAdvance", id(a, "employee_advance")).await? {
                set(a, "posting_date", adv.get("posting_date").cloned().unwrap_or_default());
                set(a, "advance_paid", money(d(&adv, "paid_amount")));
                set(a, "unclaimed_amount", money(unsettled(&adv)));
            }
            allocated += round(d(a, "allocated_amount"), p);
        }
        let grand = sanctioned + total_taxes;
        set(doc, "expenses", Value::Array(expenses.into_iter().map(Value::Object).collect()));
        set(doc, "taxes", Value::Array(taxes.into_iter().map(Value::Object).collect()));
        set(doc, "advances", Value::Array(advances.into_iter().map(Value::Object).collect()));
        set(doc, "total_claimed_amount", money(claimed));
        set(doc, "total_sanctioned_amount", money(sanctioned));
        set(doc, "total_taxes_and_charges", money(total_taxes));
        set(doc, "grand_total", money(grand));
        set(doc, "total_advance_amount", money(allocated));
        set(doc, "outstanding_amount", money(grand - allocated - d(doc, "total_amount_reimbursed")));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        for (i, r) in rows(doc, "expenses").iter().enumerate() {
            if id(r, "expense_type").is_some() && id(r, "default_account").is_none() {
                errors.push(
                    FieldError::new(
                        "expense_type",
                        "has no account for this company; set one on the expense claim type or the company's default expense account",
                    )
                    .in_row("expenses", i),
                );
            }
        }
        crate::people::same_company(hc, doc, "payable_account", "Account").await?;
        root_type_is(hc, doc, "payable_account", "Liability").await?;
        let mut seen = HashSet::new();
        for (i, r) in rows(doc, "advances").iter().enumerate() {
            let Some(adv) = lookup(hc, "EmployeeAdvance", id(r, "employee_advance")).await? else { continue };
            if !seen.insert(id(&adv, "id")) {
                errors.push(FieldError::new("employee_advance", "is listed twice").in_row("advances", i));
            }
            if id(&adv, "employee") != id(doc, "employee") || d(&adv, "docstatus") != Decimal::ONE {
                errors.push(FieldError::new("employee_advance", "is not a submitted advance of this employee").in_row("advances", i));
            } else if d(r, "allocated_amount") > unsettled(&adv) {
                errors.push(
                    FieldError::new("allocated_amount", format!("only {} of this advance is unsettled", unsettled(&adv).normalize())).in_row("advances", i),
                );
            }
        }
        if d(doc, "total_advance_amount") > d(doc, "grand_total") {
            errors.push(FieldError::new("advances", "the advances settled exceed the claim"));
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_some_and(|o| s(o, "expense_approver") == s(doc, "expense_approver")) {
            return Ok(());
        }
        let title = format!("Expense claim from {} awaits your approval", s(doc, "employee_name").unwrap_or_default());
        notify(hc, s(doc, "expense_approver"), crate::EXPENSE_REQUEST, &title, id(doc, "id").unwrap_or_default()).await
    }

    async fn before_submit(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        require_approver(hc.ctx, doc, s(doc, "expense_approver"), "expense")
    }

    /// Approval posts the claim; rejection only records the decision.
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("claim without id"))?;
        let rejected = s(doc, "workflow_state") == Some("Rejected");
        let title = format!("Your expense claim {} was {}", s(doc, "number").unwrap_or_default(), if rejected { "rejected" } else { "approved" });
        notify(hc, s(doc, "employee_user"), crate::EXPENSE_DECISION, &title, me).await?;
        if rejected {
            let mut v = Doc::new();
            set(&mut v, "status", "Rejected");
            return hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ExpenseClaim", me, v).await;
        }
        let co = company(hc.kernel, hc.ctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
        let on = opt_date(doc, "posting_date").unwrap_or_else(today);
        let mut entry = Entry::new(co.id, on, "ExpenseClaim", me, s(doc, "number").map(str::to_string));
        let name = s(doc, "employee_name").unwrap_or_default().to_string();
        entry.memo = Some(format!("Expense claim of {name}"));
        let (cc, project) = (id(doc, "cost_center"), id(doc, "project"));
        for r in rows(doc, "expenses") {
            let amount = d(&r, "sanctioned_amount");
            if amount.is_zero() {
                continue;
            }
            let account = id(&r, "default_account").ok_or_else(|| Error::invalid("an expense line has no account"))?;
            let remark = s(&r, "description").map(str::to_string).unwrap_or_else(|| format!("Expense of {name}"));
            entry.lines.push(
                Line::new(account, amount, &co.currency_code).cost_center(id(&r, "cost_center").or(cc)).project(id(&r, "project").or(project)).remarks(remark),
            );
        }
        for t in rows(doc, "taxes") {
            if let Some(account) = id(&t, "account_head") {
                entry.lines.push(
                    Line::new(account, d(&t, "tax_amount"), &co.currency_code).cost_center(cc).remarks(s(&t, "description").unwrap_or("Tax").to_string()),
                );
            }
        }
        let mut settled = Decimal::ZERO;
        for a in rows(doc, "advances") {
            let amount = d(&a, "allocated_amount");
            let Some(adv_id) = id(&a, "employee_advance").filter(|_| amount > Decimal::ZERO) else { continue };
            let adv = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, "EmployeeAdvance", adv_id, true).await?;
            if amount > unsettled(&adv) {
                return Err(Error::invalid(format!(
                    "advance {} has only {} left to settle",
                    s(&adv, "number").unwrap_or_default(),
                    unsettled(&adv).normalize()
                )));
            }
            let account = id(&adv, "advance_account").ok_or_else(|| Error::invalid("the advance has no advance account"))?;
            entry.lines.push(Line::new(account, -amount, &co.currency_code).against("EmployeeAdvance", adv_id).remarks(format!("Advance of {name} settled")));
            update_advance(hc, &adv, "claimed_amount", d(&adv, "claimed_amount") + amount).await?;
            settled += amount;
        }
        let payable = d(doc, "grand_total") - settled;
        if payable > Decimal::ZERO {
            let account = id(doc, "payable_account").ok_or_else(|| Error::field("payable_account", "is required"))?;
            entry.lines.push(Line::new(account, -payable, &co.currency_code).against("ExpenseClaim", me).remarks(format!("Payable to {name}")));
        }
        ledger::post(hc.tx, hc.ctx, entry, co.precision).await?;
        let mut v = Doc::new();
        set(&mut v, "status", if payable > Decimal::ZERO { "Unpaid" } else { "Paid" });
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ExpenseClaim", me, v).await
    }

    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if d(doc, "total_amount_reimbursed") > Decimal::ZERO {
            return Err(Error::invalid("this claim has been paid; cancel the payment entries first"));
        }
        require_approver(hc.ctx, doc, s(doc, "expense_approver"), "expense")
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let me = id(doc, "id").ok_or_else(|| Error::internal("claim without id"))?;
        if s(doc, "status") != Some("Rejected") {
            let co = company(hc.kernel, hc.ctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
            ledger::reverse(hc.tx, hc.ctx, "ExpenseClaim", me, today(), co.precision).await?;
            for a in rows(doc, "advances") {
                let amount = d(&a, "allocated_amount");
                let Some(adv_id) = id(&a, "employee_advance").filter(|_| amount > Decimal::ZERO) else { continue };
                let adv = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, "EmployeeAdvance", adv_id, true).await?;
                update_advance(hc, &adv, "claimed_amount", d(&adv, "claimed_amount") - amount).await?;
            }
        }
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "ExpenseClaim", me, v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            // Fills the advances table with what the employee's paid advances still hold.
            "get_advances" => {
                let me = id(doc, "id").ok_or_else(|| Error::internal("claim without id"))?;
                let advances: Vec<(Uuid, Decimal)> = sqlx::query_as(
                    "select id, open from (select id, posting_date, created_at, \
                     coalesce(paid_amount, 0) - coalesce(claimed_amount, 0) - coalesce(return_amount, 0) as open from e_employee_advance \
                     where tenant_id = $1 and employee = $2 and docstatus = 1) a where open > 0 order by posting_date, created_at",
                )
                .bind(hc.ctx.tenant)
                .bind(id(doc, "employee"))
                .fetch_all(&mut **hc.tx)
                .await?;
                let mut left = d(doc, "grand_total");
                let mut list = Vec::new();
                for (adv, open) in advances {
                    let take = open.min(left).max(Decimal::ZERO);
                    left -= take;
                    list.push(json!({"employee_advance": adv.to_string(), "allocated_amount": money(take)}));
                }
                let mut patch = Doc::new();
                patch.insert("advances".into(), Value::Array(list));
                let updated = hc.kernel.update_tx(hc.ctx, hc.tx, "ExpenseClaim", me, patch, None).await?;
                Ok(Some(Value::Object(updated)))
            }
            "make_payment_entry" => {
                let outstanding = d(doc, "outstanding_amount");
                let amount = amount_param(params, outstanding)?;
                let payable = id(doc, "payable_account").ok_or_else(|| Error::field("payable_account", "is required"))?;
                let cid = id(doc, "company").unwrap_or_default();
                let bank = bank_param(hc, params, cid, None).await?;
                let remark =
                    format!("Reimbursement of expense claim {} to {}", s(doc, "number").unwrap_or_default(), s(doc, "employee_name").unwrap_or_default());
                payment_entry(hc, doc, "Expense Claim", &[(payable, amount), (bank, -amount)], params, remark).await.map(Some)
            }
            _ => Ok(None),
        }
    }
}

struct AdvanceHook;

#[async_trait]
impl DocHook for AdvanceHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        stamp_company(hc, doc).await?;
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) == 0 {
            set(doc, "status", "Draft");
        }
        let Some(cid) = id(doc, "company") else { return Ok(()) };
        let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
        set(doc, "currency", co.currency_id.to_string());
        if id(doc, "advance_account").is_none() || crate::self_service(hc.ctx, "EmployeeAdvance") {
            let acc = company_account(hc, cid, "hr_employee_advance_account", "employee advance account").await?;
            set(doc, "advance_account", acc.to_string());
        }
        set(doc, "pending_amount", money(unsettled(doc)));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        crate::people::same_company(hc, doc, "advance_account", "Account").await?;
        root_type_is(hc, doc, "advance_account", "Asset").await
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut v = Doc::new();
        set(&mut v, "status", "Unpaid");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EmployeeAdvance", id(doc, "id").unwrap_or_default(), v).await
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if d(doc, "paid_amount") > Decimal::ZERO {
            return Err(Error::invalid("this advance has been paid; cancel the payment entries first"));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut v = Doc::new();
        set(&mut v, "status", "Cancelled");
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "EmployeeAdvance", id(doc, "id").unwrap_or_default(), v).await
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        let account = id(doc, "advance_account").ok_or_else(|| Error::field("advance_account", "is required"))?;
        let cid = id(doc, "company").unwrap_or_default();
        let (number, who) = (s(doc, "number").unwrap_or_default().to_string(), s(doc, "employee_name").unwrap_or_default().to_string());
        match name {
            "make_payment_entry" => {
                let amount = amount_param(params, d(doc, "advance_amount") - d(doc, "paid_amount"))?;
                let bank = bank_param(hc, params, cid, None).await?;
                let remark = format!("Payment of employee advance {number} to {who}");
                payment_entry(hc, doc, "Employee Advance", &[(account, amount), (bank, -amount)], params, remark).await.map(Some)
            }
            "make_return_entry" => {
                let amount = amount_param(params, unsettled(doc))?;
                let bank = bank_param(hc, params, cid, None).await?;
                let remark = format!("Return of unused employee advance {number} by {who}");
                payment_entry(hc, doc, "Employee Advance Return", &[(bank, amount), (account, -amount)], params, remark).await.map(Some)
            }
            _ => Ok(None),
        }
    }
}

// ───────────────────────────── payment entries ─────────────────────────────

/// The `amount` parameter (default: everything open), refused above what is open.
pub(crate) fn amount_param(params: &Value, open: Decimal) -> Result<Decimal> {
    let amount = match params.get("amount") {
        None | Some(Value::Null) => open,
        Some(v) => ferrum_kernel::value::parse_decimal(v).map_err(|m| Error::field("amount", m))?,
    };
    if open <= Decimal::ZERO {
        return Err(Error::invalid("nothing is left to pay"));
    }
    if amount <= Decimal::ZERO || amount > open {
        return Err(Error::field("amount", format!("must be between 0 and {}", open.normalize())));
    }
    Ok(amount)
}

/// The `bank_account` parameter, else the given default, else the company's default bank account.
pub(crate) async fn bank_param(hc: &mut HookCtx<'_>, params: &Value, company_id: Uuid, default: Option<Uuid>) -> Result<Uuid> {
    let given = params
        .get("bank_account")
        .and_then(Value::as_str)
        .map(|v| v.parse::<Uuid>().map_err(|_| Error::field("bank_account", "is not an account id")))
        .transpose()?;
    if let Some(b) = given.or(default) {
        return Ok(b);
    }
    let co = company(hc.kernel, hc.ctx, hc.tx, company_id).await?;
    co.require("default_bank_account", "default bank account")
}

/// Creates the journal entry paying (or receiving back) money for an HR document; `lines` are
/// (account, signed amount: + debit, − credit). With `submit: true` it is posted at once (the
/// caller needs submit rights on journal entries), else it waits as a draft for accounts.
pub(crate) async fn payment_entry(
    hc: &mut HookCtx<'_>,
    reference: &Doc,
    kind: &str,
    lines: &[(Uuid, Decimal)],
    params: &Value,
    remark: String,
) -> Result<Value> {
    hc.ctx.require("JournalEntry", Action::Create)?;
    let submit = params.get("submit").and_then(Value::as_bool).unwrap_or(false);
    if submit {
        hc.ctx.require("JournalEntry", Action::Submit)?;
    }
    let on = match params.get("posting_date").and_then(Value::as_str) {
        Some(v) => ferrum_kernel::value::parse_date(v).map_err(|m| Error::field("posting_date", m))?,
        None => today(),
    };
    let accounts: Vec<Value> = lines
        .iter()
        .map(|(account, amount)| {
            let (dr, cr) = if *amount >= Decimal::ZERO { (*amount, Decimal::ZERO) } else { (Decimal::ZERO, -*amount) };
            json!({"account": account.to_string(), "debit": money(dr), "credit": money(cr)})
        })
        .collect();
    let mut je = Doc::new();
    set(&mut je, "company", reference.get("company").cloned().unwrap_or_default());
    set(&mut je, "posting_date", on.to_string());
    set(&mut je, "entry_type", "Journal Entry");
    set(&mut je, "accounts", Value::Array(accounts));
    set(&mut je, "remark", remark);
    set(&mut je, "hr_reference_type", kind);
    set(&mut je, "hr_reference", s(reference, "id").unwrap_or_default());
    set(&mut je, "hr_reference_number", reference.get("number").cloned().unwrap_or_default());
    let user = elevated(hc.ctx);
    let made = hc.kernel.insert_tx(&user, hc.tx, "JournalEntry", je).await?;
    if !submit {
        return Ok(Value::Object(made));
    }
    let posted = hc.kernel.submit_tx(&user, hc.tx, "JournalEntry", id(&made, "id").unwrap_or_default(), None).await?;
    Ok(Value::Object(posted))
}

/// Net debit (debit − credit) of a journal entry on one account.
fn net_on(je: &Doc, account: Option<Uuid>) -> Decimal {
    rows(je, "accounts").iter().filter(|l| account.is_some() && id(l, "account") == account).map(|l| d(l, "debit") - d(l, "credit")).sum()
}

/// Journal entries referencing an HR document settle it: checked on save, applied on submit and
/// undone on cancel.
struct PaymentHook;

impl PaymentHook {
    async fn settle(&self, hc: &mut HookCtx<'_>, je: &Doc, sign: Decimal, write: bool) -> Result<()> {
        let Some(kind) = s(je, "hr_reference_type") else { return Ok(()) };
        let target = s(je, "hr_reference").and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::field("hr_reference", "is not a document id"))?;
        let (entity, account_field) = match kind {
            "Expense Claim" => ("ExpenseClaim", "payable_account"),
            "Employee Advance" | "Employee Advance Return" => ("EmployeeAdvance", "advance_account"),
            "Payroll Entry" => ("PayrollEntry", "payroll_payable_account"),
            _ => return Err(Error::field("hr_reference_type", "is not supported")),
        };
        let doc = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, entity, target, write).await?;
        let label = format!("{kind} {}", s(&doc, "number").unwrap_or_default());
        if d(&doc, "docstatus") != Decimal::ONE {
            return Err(Error::invalid(format!("{label} is not submitted")));
        }
        if id(&doc, "company") != id(je, "company") {
            return Err(Error::invalid(format!("{label} belongs to another company")));
        }
        let net = net_on(je, id(&doc, account_field));
        let amount = if kind == "Employee Advance Return" { -net } else { net };
        if amount <= Decimal::ZERO {
            let side = if kind == "Employee Advance Return" { "credit" } else { "debit" };
            return Err(Error::invalid(format!("the entry must {side} the {} of {label}", account_field.replace('_', " "))));
        }
        let delta = sign * amount;
        let mut v = Doc::new();
        match kind {
            "Expense Claim" => {
                if s(&doc, "status") == Some("Rejected") {
                    return Err(Error::invalid(format!("{label} was rejected")));
                }
                let paid = d(&doc, "total_amount_reimbursed") + delta;
                let outstanding = d(&doc, "grand_total") - d(&doc, "total_advance_amount") - paid;
                if outstanding < Decimal::ZERO || paid < Decimal::ZERO {
                    return Err(Error::invalid(format!("{label} has only {} outstanding", d(&doc, "outstanding_amount").normalize())));
                }
                v.insert("total_amount_reimbursed".into(), money(paid));
                v.insert("outstanding_amount".into(), money(outstanding));
                v.insert("status".into(), json!(if outstanding.is_zero() { "Paid" } else { "Unpaid" }));
            }
            "Employee Advance" | "Employee Advance Return" => {
                let field = if kind == "Employee Advance" { "paid_amount" } else { "return_amount" };
                if write {
                    return update_advance(hc, &doc, field, d(&doc, field) + delta).await;
                }
                let mut a = doc.clone();
                set(&mut a, field, money(d(&doc, field) + delta));
                if d(&a, "paid_amount") > d(&a, "advance_amount") || unsettled(&a) < Decimal::ZERO {
                    return Err(Error::invalid(format!("{label}: the amount is more than what is open")));
                }
                return Ok(());
            }
            _ => {
                let paid = d(&doc, "paid_amount") + delta;
                let net_pay = d(&doc, "total_net_pay");
                if paid > net_pay || paid < Decimal::ZERO {
                    return Err(Error::invalid(format!("{label} has only {} left to pay", (net_pay - d(&doc, "paid_amount")).normalize())));
                }
                v.insert("paid_amount".into(), money(paid));
                v.insert("status".into(), json!(if paid == net_pay { "Paid" } else { "Slips Submitted" }));
            }
        }
        if write {
            hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, entity, target, v).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl DocHook for PaymentHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if doc.get("docstatus").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Ok(());
        }
        self.settle(hc, doc, Decimal::ONE, false).await
    }

    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        self.settle(hc, doc, Decimal::ONE, true).await
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        self.settle(hc, doc, -Decimal::ONE, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_statuses() {
        let adv = |paid: i64, claimed: i64, returned: i64| {
            let mut a = Doc::new();
            set(&mut a, "advance_amount", "1000");
            set(&mut a, "paid_amount", paid.to_string());
            set(&mut a, "claimed_amount", claimed.to_string());
            set(&mut a, "return_amount", returned.to_string());
            a
        };
        assert_eq!(advance_status(&adv(0, 0, 0)), "Unpaid");
        assert_eq!(advance_status(&adv(1000, 0, 0)), "Paid");
        assert_eq!(advance_status(&adv(1000, 400, 0)), "Paid");
        assert_eq!(advance_status(&adv(1000, 1000, 0)), "Claimed");
        assert_eq!(advance_status(&adv(1000, 0, 1000)), "Returned");
        assert_eq!(advance_status(&adv(1000, 600, 400)), "Partly Claimed and Returned");
        assert_eq!(unsettled(&adv(1000, 600, 300)), Decimal::from(100));
        assert!(amount_param(&json!({}), Decimal::ZERO).is_err());
        assert_eq!(amount_param(&json!({}), Decimal::from(5)).unwrap(), Decimal::from(5));
        assert!(amount_param(&json!({"amount": "6"}), Decimal::from(5)).is_err());
    }
}
