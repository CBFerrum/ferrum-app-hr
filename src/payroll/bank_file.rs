//! Paying a payroll run through the bank (FERRUM-HR-002): the run's net pay as a file the bank
//! takes, one credit transfer to each employee paid by bank. ISO 20022's customer credit transfer
//! initiation (`pain.001.001.09`, category purpose `SALA`), which SEPA banks and many others take,
//! or a CSV for a bank's bulk-upload sheet. It is read with the caller's own rights: employees'
//! bank details and the company's own account are each in a permission group, and someone who
//! cannot read them gets a refusal, not a file. The company's account is Finance's Bank Account for
//! the paying GL account, or the one given with the request where Finance is not installed.

use crate::expense::bank_param;
use chrono::{NaiveDate, Utc};
use ferrum_domain::util::{b, company, id, money, s};
use ferrum_kernel::{Doc, Error, HookCtx, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

pub(crate) const PAIN: &str = "ISO 20022 (pain.001)";
pub(crate) const CSV: &str = "CSV";

/// One credit transfer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Payee {
    pub name: String,
    pub slip: String,
    pub amount: Decimal,
    pub iban: Option<String>,
    pub account: Option<String>,
    pub routing: Option<String>,
    pub bank: Option<String>,
}

/// The company's own account at the bank, and the bank's BIC when known.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Debtor {
    pub name: String,
    pub iban: Option<String>,
    pub account: Option<String>,
    pub bic: Option<String>,
}

/// What one file says beyond its payees.
pub(crate) struct Order<'a> {
    pub message_id: &'a str,
    pub created: &'a str,
    pub pay_on: NaiveDate,
    pub currency: &'a str,
    pub country: Option<&'a str>,
    pub reference: &'a str,
}

/// The `bank_file {format?, execution_date?, bank_account?, from_account?, from_bic?}` action.
pub(crate) async fn bank_file(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let readable =
        |entity: &str, field: &str| hc.ctx.schema.entity(entity).and_then(|e| e.field(field).cloned()).is_some_and(|f| hc.ctx.perms.can_read_field(entity, &f));
    if !readable("Employee", "bank_account_no") {
        return Err(Error::Forbidden("bank details are restricted to payroll and HR managers".into()));
    }
    let run = id(doc, "id").ok_or_else(|| Error::internal("no id"))?;
    let cid = id(doc, "company").ok_or_else(|| Error::field("company", "is required"))?;
    let co = company(hc.kernel, hc.ctx, hc.tx, cid).await?;
    let param = |k: &str| params.get(k).and_then(Value::as_str).map(compact).filter(|v| !v.is_empty());
    let debtor = match (param("from_account"), hc.ctx.schema.entity("BankAccount").is_some()) {
        (Some(acct), _) => Debtor {
            name: co.name().to_string(),
            iban: is_iban(&acct).then(|| acct.clone()),
            account: (!is_iban(&acct)).then_some(acct),
            bic: param("from_bic"),
        },
        (None, true) => {
            if !readable("BankAccount", "account_number") {
                return Err(Error::Forbidden(
                    "the company's own bank details are restricted to Finance's bank roles: give the account with the request".into(),
                ));
            }
            let gl = bank_param(hc, params, cid, id(doc, "payment_account")).await?;
            let found: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
                "select iban, account_number, swift_code from e_bank_account where tenant_id = $1 and company = $2 and account = $3 \
                 and not coalesce(disabled, false) order by coalesce(is_default, false) desc, created_at limit 1",
            )
            .bind(hc.ctx.tenant)
            .bind(cid)
            .bind(gl)
            .fetch_optional(&mut **hc.tx)
            .await?;
            let (iban, account, bic) = found.ok_or_else(|| {
                Error::invalid("record the company's account at the bank (a Bank Account, in Finance) for the account salaries are paid from, with its IBAN or account number")
            })?;
            Debtor { name: co.name().to_string(), iban: iban.map(|v| compact(&v)), account: account.map(|v| compact(&v)), bic: bic.map(|v| compact(&v)) }
        }
        (None, false) => return Err(Error::field("from_account", "the company's account at the bank: its IBAN or account number")),
    };
    if debtor.iban.is_none() && debtor.account.is_none() {
        return Err(Error::invalid("the company's Bank Account has neither an IBAN nor an account number"));
    }
    // What the bank entry pays, one transfer to each employee: held salaries are left out, and what
    // is still owed from an earlier slip goes with this one.
    let due = super::payments::due(hc, run).await?;
    let mut per: Vec<(Uuid, Option<String>, Option<String>, Decimal)> = Vec::new();
    for x in &due {
        match per.iter_mut().find(|p| p.0 == x.employee) {
            Some(p) => {
                p.3 += x.amount;
                if x.own {
                    p.2 = x.number.clone();
                }
            }
            None => per.push((x.employee, x.name.clone(), x.number.clone(), x.amount)),
        }
    }
    #[allow(clippy::type_complexity)]
    let banks: HashMap<Uuid, (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as::<_, (Uuid, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "select id, salary_mode, bank_name, bank_account_no, ifsc_code, iban from e_employee where tenant_id = $1 and id = any($2)",
        )
        .bind(hc.ctx.tenant)
        .bind(per.iter().map(|p| p.0).collect::<Vec<_>>())
        .fetch_all(&mut **hc.tx)
        .await?
        .into_iter()
        .map(|(e, mode, bank, account, routing, iban)| (e, (mode, bank, account, routing, iban)))
        .collect();
    let rows = per
        .into_iter()
        .map(|(e, name, slip, amount)| {
            let (mode, bank, account, routing, iban) = banks.get(&e).cloned().unwrap_or_default();
            (name, mode, bank, account, routing, iban, Some(amount), slip)
        })
        .collect();
    let (payees, missing) = payees(rows);
    if !missing.is_empty() {
        return Err(Error::invalid(format!("no bank account for {}: add it to their employee record, or pay them another way", missing.join(", "))));
    }
    if payees.is_empty() {
        return Err(Error::invalid(if due.is_empty() { super::run::nothing_due(hc, run).await? } else { "nobody in this run is paid by bank".into() }));
    }
    let pay_on = match param("execution_date") {
        Some(v) => NaiveDate::parse_from_str(&v, "%Y-%m-%d").map_err(|_| Error::field("execution_date", "is a day"))?,
        None => crate::opt_date(doc, "posting_date").unwrap_or_else(|| hc.ctx.today()),
    };
    let number = s(doc, "number").unwrap_or("PAYROLL").to_string();
    let now = Utc::now();
    let message_id = format!("{number}-{}", now.format("%Y%m%d%H%M%S"));
    let reference = if b(doc, "off_cycle") {
        format!("Salary paid off-cycle {}", s(doc, "start_date").unwrap_or_default())
    } else {
        format!("Salary {} to {}", s(doc, "start_date").unwrap_or_default(), s(doc, "end_date").unwrap_or_default())
    };
    let order = Order {
        message_id: &message_id,
        created: &now.format("%Y-%m-%dT%H:%M:%S").to_string(),
        pay_on,
        currency: &co.currency_code,
        country: s(&co.doc, "country_code"),
        reference: &reference,
    };
    let total: Decimal = payees.iter().map(|p| p.amount).sum();
    let (name, kind, content) = match param("format").as_deref() {
        None | Some(PAIN) => (format!("{number}-pain.001.xml"), "application/xml", pain001(&order, &debtor, &payees)),
        Some(CSV) => (format!("{number}-bank.csv"), "text/csv", csv(&order, &payees)),
        Some(_) => return Err(Error::field("format", format!("is {PAIN} or {CSV}"))),
    };
    Ok(json!({
        "file": {"name": name, "type": kind, "content": content},
        "payments": payees.len(),
        "total": money(total),
        "message": format!("{} payment{} of {} {} in all", payees.len(), if payees.len() == 1 { "" } else { "s" }, total.normalize(), co.currency_code),
    }))
}

/// What is due as payments: those paid by bank with something to pay, and the names of those paid
/// by bank without an account to pay into.
pub(crate) fn payees(
    rows: Vec<(Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<Decimal>, Option<String>)>,
) -> (Vec<Payee>, Vec<String>) {
    let (mut out, mut missing) = (Vec::new(), Vec::new());
    for (name, mode, bank, account, routing, iban, net, slip) in rows {
        let amount = net.unwrap_or_default();
        if mode.as_deref().unwrap_or("Bank") != "Bank" || amount <= Decimal::ZERO {
            continue;
        }
        let tidy = |v: Option<String>| v.map(|v| compact(&v)).filter(|v| !v.is_empty());
        let (iban, account) = (tidy(iban), tidy(account));
        let name = name.unwrap_or_default();
        if iban.is_none() && account.is_none() {
            missing.push(name);
            continue;
        }
        out.push(Payee { name, slip: slip.unwrap_or_default(), amount, iban, account, routing: tidy(routing), bank: bank.filter(|b| !b.trim().is_empty()) });
    }
    (out, missing)
}

/// An account number or IBAN without the spaces people write it with, in capitals.
fn compact(v: &str) -> String {
    v.split_whitespace().collect::<String>().to_uppercase()
}

/// Two letters, two check digits and the rest: the shape of an IBAN (its digits are the bank's to check).
fn is_iban(v: &str) -> bool {
    v.len() >= 15 && v.len() <= 34 && v[..2].chars().all(|c| c.is_ascii_uppercase()) && v[2..4].chars().all(|c| c.is_ascii_digit())
}

/// The clearing system a country's routing codes belong to (ISO 20022's external code list), for a
/// payee without an IBAN.
fn clearing_system(country: Option<&str>) -> Option<&'static str> {
    Some(match country? {
        "IN" => "INFSC",
        "US" => "USABA",
        "GB" => "GBDSC",
        "CA" => "CACPA",
        "AU" => "AUBSB",
        "NZ" => "NZNCC",
        "ZA" => "ZANCC",
        _ => return None,
    })
}

fn xml(v: &str) -> String {
    v.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// At most `n` characters, as the message's fields allow.
fn cut(v: &str, n: usize) -> String {
    v.chars().take(n).collect()
}

fn amount(v: Decimal) -> String {
    format!("{:.2}", v.round_dp(2))
}

fn account_xml(tag: &str, iban: Option<&str>, other: Option<&str>) -> String {
    match (iban, other) {
        (Some(i), _) => format!("<{tag}><Id><IBAN>{}</IBAN></Id></{tag}>", xml(i)),
        (None, Some(o)) => format!("<{tag}><Id><Othr><Id>{}</Id></Othr></Id></{tag}>", xml(&cut(o, 34))),
        (None, None) => String::new(),
    }
}

/// The run as a `pain.001.001.09` customer credit transfer initiation: one payment information
/// block from the company's account, one transaction per payee.
pub(crate) fn pain001(o: &Order<'_>, d: &Debtor, payees: &[Payee]) -> String {
    let total: Decimal = payees.iter().map(|p| p.amount).sum();
    let n = payees.len();
    // SEPA's service level, and its charges shared at the service level, for euros between IBANs.
    let sepa = o.currency == "EUR" && d.iban.is_some() && payees.iter().all(|p| p.iban.is_some());
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\"><CstmrCdtTrfInitn>");
    x.push_str(&format!(
        "<GrpHdr><MsgId>{}</MsgId><CreDtTm>{}</CreDtTm><NbOfTxs>{n}</NbOfTxs><CtrlSum>{}</CtrlSum><InitgPty><Nm>{}</Nm></InitgPty></GrpHdr>",
        xml(&cut(o.message_id, 35)),
        o.created,
        amount(total),
        xml(&cut(&d.name, 140)),
    ));
    x.push_str(&format!(
        "<PmtInf><PmtInfId>{}</PmtInfId><PmtMtd>TRF</PmtMtd><BtchBookg>true</BtchBookg><NbOfTxs>{n}</NbOfTxs><CtrlSum>{}</CtrlSum><PmtTpInf>{}<CtgyPurp><Cd>SALA</Cd></CtgyPurp></PmtTpInf>",
        xml(&cut(o.message_id, 35)),
        amount(total),
        if sepa { "<SvcLvl><Cd>SEPA</Cd></SvcLvl>" } else { "" },
    ));
    x.push_str(&format!("<ReqdExctnDt><Dt>{}</Dt></ReqdExctnDt><Dbtr><Nm>{}</Nm></Dbtr>", o.pay_on, xml(&cut(&d.name, 140))));
    x.push_str(&account_xml("DbtrAcct", d.iban.as_deref(), d.account.as_deref()));
    x.push_str(&match &d.bic {
        Some(bic) => format!("<DbtrAgt><FinInstnId><BICFI>{}</BICFI></FinInstnId></DbtrAgt>", xml(bic)),
        None => "<DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></DbtrAgt>".to_string(),
    });
    x.push_str(if sepa { "<ChrgBr>SLEV</ChrgBr>" } else { "<ChrgBr>SHAR</ChrgBr>" });
    for p in payees {
        x.push_str(&format!(
            "<CdtTrfTxInf><PmtId><EndToEndId>{}</EndToEndId></PmtId><Amt><InstdAmt Ccy=\"{}\">{}</InstdAmt></Amt>",
            xml(&cut(if p.slip.is_empty() { o.message_id } else { &p.slip }, 35)),
            xml(o.currency),
            amount(p.amount),
        ));
        // Without an IBAN, the payee's bank is known by its routing code.
        if p.iban.is_none()
            && let Some(code) = &p.routing
        {
            let system = clearing_system(o.country).map(|c| format!("<ClrSysId><Cd>{c}</Cd></ClrSysId>")).unwrap_or_default();
            x.push_str(&format!("<CdtrAgt><FinInstnId><ClrSysMmbId>{system}<MmbId>{}</MmbId></ClrSysMmbId></FinInstnId></CdtrAgt>", xml(&cut(code, 35))));
        }
        x.push_str(&format!("<Cdtr><Nm>{}</Nm></Cdtr>", xml(&cut(&p.name, 140))));
        x.push_str(&account_xml("CdtrAcct", p.iban.as_deref(), p.account.as_deref()));
        x.push_str(&format!("<Purp><Cd>SALA</Cd></Purp><RmtInf><Ustrd>{}</Ustrd></RmtInf></CdtTrfTxInf>", xml(&cut(o.reference, 140))));
    }
    x.push_str("</PmtInf></CstmrCdtTrfInitn></Document>\n");
    x
}

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) { format!("\"{}\"", v.replace('"', "\"\"")) } else { v.to_string() }
}

/// The run as a sheet a bank's bulk upload takes: one row per payee.
pub(crate) fn csv(o: &Order<'_>, payees: &[Payee]) -> String {
    let mut out = String::from("Beneficiary Name,Account Number,IBAN,IFSC / Routing Code,Bank,Amount,Currency,Pay On,Reference\r\n");
    for p in payees {
        let row = [
            p.name.clone(),
            p.account.clone().unwrap_or_default(),
            p.iban.clone().unwrap_or_default(),
            p.routing.clone().unwrap_or_default(),
            p.bank.clone().unwrap_or_default(),
            amount(p.amount),
            o.currency.to_string(),
            o.pay_on.to_string(),
            format!("{} {}", o.reference, p.slip),
        ];
        out.push_str(&row.iter().map(|v| csv_field(v)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn order() -> Order<'static> {
        Order {
            message_id: "HR-PRUN-2026-0001-20261105120000",
            created: "2026-11-05T12:00:00",
            pay_on: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            currency: "INR",
            country: Some("IN"),
            reference: "Salary 2026-09-01 to 2026-09-30",
        }
    }

    fn priya() -> Payee {
        Payee {
            name: "Priya & Co".into(),
            slip: "SAL-202609-00001".into(),
            amount: d("143537.14"),
            iban: None,
            account: Some("0011223344".into()),
            routing: Some("HDFC0000001".into()),
            bank: Some("HDFC".into()),
        }
    }

    #[test]
    fn who_is_paid_by_bank() {
        let row = |name: &str, mode: Option<&str>, account: Option<&str>, iban: Option<&str>, net: &str| {
            (
                Some(name.to_string()),
                mode.map(str::to_string),
                None,
                account.map(str::to_string),
                None,
                iban.map(str::to_string),
                Some(d(net)),
                Some(format!("S-{name}")),
            )
        };
        let (paid, missing) = payees(vec![
            row("Asha", Some("Bank"), Some(" 0011 2233 "), None, "100"),
            row("Ben", Some("Cash"), None, None, "50"),
            row("Cyd", None, None, None, "70"),
            row("Dee", Some("Bank"), None, Some("de89 3704 0044 0532 0130 00"), "0"),
            row("Eve", Some("Bank"), None, Some("de89 3704 0044 0532 0130 00"), "80"),
        ]);
        assert_eq!(
            paid.iter().map(|p| (p.name.as_str(), p.account.clone(), p.iban.clone())).collect::<Vec<_>>(),
            [("Asha", Some("00112233".to_string()), None), ("Eve", None, Some("DE89370400440532013000".to_string()))]
        );
        assert_eq!(missing, ["Cyd"], "bank is the default; cash is not paid here; nothing to pay is not paid");
        assert!(is_iban("DE89370400440532013000") && !is_iban("0011223344") && !is_iban("HDFC0000001"));
    }

    #[test]
    fn a_salary_credit_transfer() {
        let debtor = Debtor { name: "Hrpay Pvt Ltd".into(), iban: None, account: Some("50100012345678".into()), bic: Some("HDFCINBB".into()) };
        let x = pain001(&order(), &debtor, &[priya()]);
        assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\">"));
        for part in [
            "<GrpHdr><MsgId>HR-PRUN-2026-0001-20261105120000</MsgId><CreDtTm>2026-11-05T12:00:00</CreDtTm><NbOfTxs>1</NbOfTxs><CtrlSum>143537.14</CtrlSum>",
            "<PmtMtd>TRF</PmtMtd><BtchBookg>true</BtchBookg><NbOfTxs>1</NbOfTxs><CtrlSum>143537.14</CtrlSum><PmtTpInf><CtgyPurp><Cd>SALA</Cd></CtgyPurp></PmtTpInf>",
            "<ReqdExctnDt><Dt>2026-10-01</Dt></ReqdExctnDt><Dbtr><Nm>Hrpay Pvt Ltd</Nm></Dbtr><DbtrAcct><Id><Othr><Id>50100012345678</Id></Othr></Id></DbtrAcct>",
            "<DbtrAgt><FinInstnId><BICFI>HDFCINBB</BICFI></FinInstnId></DbtrAgt><ChrgBr>SHAR</ChrgBr>",
            "<PmtId><EndToEndId>SAL-202609-00001</EndToEndId></PmtId><Amt><InstdAmt Ccy=\"INR\">143537.14</InstdAmt></Amt>",
            "<CdtrAgt><FinInstnId><ClrSysMmbId><ClrSysId><Cd>INFSC</Cd></ClrSysId><MmbId>HDFC0000001</MmbId></ClrSysMmbId></FinInstnId></CdtrAgt>",
            "<Cdtr><Nm>Priya &amp; Co</Nm></Cdtr><CdtrAcct><Id><Othr><Id>0011223344</Id></Othr></Id></CdtrAcct><Purp><Cd>SALA</Cd></Purp>",
            "<RmtInf><Ustrd>Salary 2026-09-01 to 2026-09-30</Ustrd></RmtInf></CdtTrfTxInf></PmtInf></CstmrCdtTrfInitn></Document>",
        ] {
            assert!(x.contains(part), "{part}\nin\n{x}");
        }
        // Euros between IBANs go by SEPA, charges at its service level, the payee's bank unnamed.
        let sepa = Order { currency: "EUR", country: Some("DE"), ..order() };
        let eve = Payee { iban: Some("DE89370400440532013000".into()), account: None, routing: None, ..priya() };
        let x = pain001(&sepa, &Debtor { iban: Some("DE02120300000000202051".into()), account: None, bic: None, ..debtor }, &[eve]);
        assert!(x.contains("<PmtTpInf><SvcLvl><Cd>SEPA</Cd></SvcLvl><CtgyPurp><Cd>SALA</Cd></CtgyPurp></PmtTpInf>"), "{x}");
        assert!(x.contains("<DbtrAcct><Id><IBAN>DE02120300000000202051</IBAN></Id></DbtrAcct><DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></DbtrAgt><ChrgBr>SLEV</ChrgBr>"));
        assert!(!x.contains("<CdtrAgt>") && x.contains("<CdtrAcct><Id><IBAN>DE89370400440532013000</IBAN></Id></CdtrAcct>"));
    }

    #[test]
    fn a_sheet_for_bulk_upload() {
        assert_eq!(
            csv(&order(), &[priya()]),
            "Beneficiary Name,Account Number,IBAN,IFSC / Routing Code,Bank,Amount,Currency,Pay On,Reference\r\n\
             Priya & Co,0011223344,,HDFC0000001,HDFC,143537.14,INR,2026-10-01,Salary 2026-09-01 to 2026-09-30 SAL-202609-00001\r\n"
        );
        assert_eq!(csv_field("A, \"B\""), "\"A, \"\"B\"\"\"");
    }
}
