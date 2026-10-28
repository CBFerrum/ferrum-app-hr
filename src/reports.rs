//! HR and payroll reports. Each runs read-only for one company; callers whose access to the
//! report's entity is limited to their own records only see their own employee's rows.

use chrono::{Datelike, Duration, NaiveDate};
use ferrum_domain::reports::{Params, ReportSource, col, param_def, period, read_tx, resolve_company};
use ferrum_domain::util::money;
use ferrum_kernel::{Access, Action, Ctx, Error, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

const MAX_ROWS: usize = 10_000;

pub struct HrReports;

fn link(name: &str, label: &str, to: &str) -> Value {
    json!({"name": name, "label": label, "type": "link", "to": to})
}

fn select(name: &str, label: &str, options: &[&str]) -> Value {
    json!({"name": name, "label": label, "type": "select", "options": options})
}

fn entry(name: &str, label: &str, module: &str, entity: &str, params: Vec<Value>) -> Value {
    json!({"name": name, "label": label, "module": module, "entity": entity, "params": params})
}

/// The signed-in user's email when they may only read their own records of `entity`.
fn own_only(ctx: &Ctx, entity: &str) -> Option<String> {
    (!ctx.is_admin && !ctx.is_system && ctx.perms.access(entity, Action::Read) == Access::Own).then(|| ctx.email.to_lowercase())
}

/// ` and lower(e."user") = <email>` for callers limited to their own employee.
fn push_own(qb: &mut QueryBuilder<Postgres>, own: &Option<String>) {
    if let Some(email) = own {
        qb.push(" and lower(e.\"user\") = ").push_bind(email.clone());
    }
}

fn push_eq(qb: &mut QueryBuilder<Postgres>, column: &str, v: Option<Uuid>) {
    if let Some(v) = v {
        qb.push(format!(" and {column} = ")).push_bind(v);
    }
}

fn sum_columns(rows: &[Value], names: &[String]) -> Map<String, Value> {
    names
        .iter()
        .map(|n| {
            let total: Decimal = rows.iter().filter_map(|r| r.get(n)).filter_map(|v| ferrum_kernel::value::parse_decimal(v).ok()).sum();
            (n.clone(), money(total))
        })
        .collect()
}

/// The next date on or after `from` with the month and day of `date` (29 February falls on the
/// 28th in other years), with the number of years since `date`.
pub fn next_occurrence(date: NaiveDate, from: NaiveDate) -> (NaiveDate, i32) {
    let on = |y: i32| NaiveDate::from_ymd_opt(y, date.month(), date.day()).or_else(|| NaiveDate::from_ymd_opt(y, date.month(), date.day() - 1)).unwrap_or(date);
    let this = on(from.year());
    let next = if this >= from { this } else { on(from.year() + 1) };
    (next, next.year() - date.year())
}

#[async_trait::async_trait]
impl ReportSource for HrReports {
    fn catalog(&self) -> Vec<Value> {
        let company = || param_def("company");
        let statuses = ["Active", "Inactive", "Suspended", "Left"];
        vec![
            entry(
                "employee_directory",
                "Employee Directory",
                "hr",
                "Employee",
                vec![
                    company(),
                    link("department", "Department", "Department"),
                    link("designation", "Designation", "Designation"),
                    select("status", "Status", &statuses),
                ],
            ),
            entry(
                "headcount",
                "Headcount and Attrition",
                "hr",
                "Employee",
                vec![
                    company(),
                    select("group_by", "Group By", &["Department", "Designation", "Branch", "Grade", "Employment Type", "Gender"]),
                    param_def("from_date"),
                    param_def("to_date"),
                ],
            ),
            entry(
                "employee_events",
                "Upcoming Birthdays and Work Anniversaries",
                "hr",
                "Employee",
                vec![
                    company(),
                    json!({"name": "from_date", "label": "From", "type": "date", "default": "today"}),
                    json!({"name": "days", "label": "Days Ahead", "type": "int", "default": 30}),
                ],
            ),
            entry(
                "monthly_attendance_sheet",
                "Monthly Attendance Sheet",
                "attendance",
                "Attendance",
                vec![
                    company(),
                    json!({"name": "month", "label": "Month (any day in it)", "type": "date", "default": "today"}),
                    link("department", "Department", "Department"),
                ],
            ),
            entry(
                "leave_balance",
                "Leave Balance",
                "attendance",
                "LeaveAllocation",
                vec![
                    company(),
                    param_def("as_of"),
                    link("employee", "Employee", "Employee"),
                    link("leave_type", "Leave Type", "LeaveType"),
                    link("department", "Department", "Department"),
                ],
            ),
            entry(
                "expense_claim_summary",
                "Expense Claim Summary",
                "expenses",
                "ExpenseClaim",
                vec![
                    company(),
                    param_def("from_date"),
                    param_def("to_date"),
                    link("employee", "Employee", "Employee"),
                    select("status", "Status", &["Draft", "Unpaid", "Paid", "Rejected"]),
                ],
            ),
            entry(
                "recruitment_pipeline",
                "Recruitment Pipeline",
                "recruitment",
                "JobApplicant",
                vec![company(), select("status", "Opening Status", &["Open", "Closed"])],
            ),
            entry(
                "salary_register",
                "Salary Register",
                "payroll",
                "SalarySlip",
                vec![
                    company(),
                    param_def("from_date"),
                    param_def("to_date"),
                    link("department", "Department", "Department"),
                    select("status", "Slips", &["Submitted", "Draft"]),
                ],
            ),
            entry("payroll_summary", "Payroll Summary by Department", "payroll", "SalarySlip", vec![company(), param_def("from_date"), param_def("to_date")]),
            entry(
                "bank_remittance",
                "Bank Remittance",
                "payroll",
                "PayrollEntry",
                vec![company(), link("payroll_entry", "Payroll Entry", "PayrollEntry"), param_def("from_date"), param_def("to_date")],
            ),
            entry("income_tax_deductions", "Income Tax Deductions", "payroll", "SalarySlip", vec![company(), param_def("from_date"), param_def("to_date")]),
        ]
    }

    async fn run(&self, k: &Kernel, ctx: &Ctx, name: &str, raw: &HashMap<String, String>) -> Result<Value> {
        let p = Params { raw };
        let mut tx = read_tx(k, ctx).await?;
        let co = resolve_company(&mut tx, ctx, &p).await?;
        let mut out = match name {
            "employee_directory" => directory(&mut tx, ctx, co.id, &p).await?,
            "headcount" => headcount(&mut tx, ctx, co.id, &p, &co).await?,
            "employee_events" => events(&mut tx, ctx, co.id, &p).await?,
            "monthly_attendance_sheet" => attendance_sheet(&mut tx, ctx, co.id, &p).await?,
            "leave_balance" => leave_balance(&mut tx, ctx, co.id, &p).await?,
            "expense_claim_summary" => {
                let (from, to) = period(&p, &co)?;
                expense_summary(&mut tx, ctx, co.id, &p, from, to).await?
            }
            "recruitment_pipeline" => pipeline(&mut tx, ctx, co.id, &p).await?,
            "salary_register" | "payroll_summary" | "income_tax_deductions" => {
                let (from, to) = period(&p, &co)?;
                let submitted = p.str("status") != Some("Draft");
                let data = register_data(&mut tx, ctx, co.id, &p, from, to, submitted).await?;
                match name {
                    "salary_register" => salary_register(data),
                    "payroll_summary" => payroll_summary(data),
                    _ => income_tax(&mut tx, ctx, data).await?,
                }
            }
            "bank_remittance" => {
                let (from, to) = period(&p, &co)?;
                bank_remittance(&mut tx, ctx, co.id, &p, from, to).await?
            }
            _ => return Err(Error::NotFound(format!("report {name}"))),
        };
        tx.rollback().await?;
        if let Value::Object(m) = &mut out {
            let meta = m.entry("meta").or_insert_with(|| json!({}));
            if let Value::Object(meta) = meta {
                meta.insert("company".into(), json!(co.name));
                meta.insert("currency".into(), json!(co.currency));
            }
        }
        Ok(out)
    }
}

async fn directory(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>) -> Result<Value> {
    let mut qb = QueryBuilder::new(
        "select e.number, e.employee_name, d.department_name, g.designation_name, b.branch_name, e.status, e.date_of_joining, m.employee_name, \
         e.company_email, e.cell_number from e_employee e \
         left join e_department d on d.tenant_id = e.tenant_id and d.id = e.department \
         left join e_designation g on g.tenant_id = e.tenant_id and g.id = e.designation \
         left join e_branch b on b.tenant_id = e.tenant_id and b.id = e.branch \
         left join e_employee m on m.tenant_id = e.tenant_id and m.id = e.reports_to where e.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and e.company = ").push_bind(company);
    push_eq(&mut qb, "e.department", p.uuid("department")?);
    push_eq(&mut qb, "e.designation", p.uuid("designation")?);
    if let Some(st) = p.str("status") {
        qb.push(" and e.status = ").push_bind(st.to_string());
    }
    push_own(&mut qb, &own_only(ctx, "Employee"));
    qb.push(" order by e.employee_name, e.number limit ").push_bind((MAX_ROWS + 1) as i64);
    #[allow(clippy::type_complexity)]
    let data: Vec<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<NaiveDate>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let truncated = data.len() > MAX_ROWS;
    let rows: Vec<Value> = data
        .into_iter()
        .take(MAX_ROWS)
        .map(|(no, name, dept, desig, branch, status, joined, manager, email, phone)| {
            json!({"employee": no, "employee_name": name, "department": dept, "designation": desig, "branch": branch, "status": status,
                "date_of_joining": joined, "reports_to": manager, "company_email": email, "cell_number": phone})
        })
        .collect();
    let count = rows.len();
    Ok(json!({
        "columns": [col("employee", "Employee", "data"), col("employee_name", "Name", "data"), col("department", "Department", "data"),
            col("designation", "Designation", "data"), col("branch", "Branch", "data"), col("status", "Status", "data"),
            col("date_of_joining", "Joined", "date"), col("reports_to", "Reports To", "data"), col("company_email", "Email", "data"),
            col("cell_number", "Mobile", "data")],
        "rows": rows,
        "totals": {"count": count},
        "meta": {"truncated": truncated},
    }))
}

async fn headcount(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>, co: &ferrum_domain::reports::ReportCompany) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let (label, join) = match p.str("group_by").unwrap_or("Department") {
        "Designation" => ("x.designation_name", "left join e_designation x on x.tenant_id = e.tenant_id and x.id = e.designation"),
        "Branch" => ("x.branch_name", "left join e_branch x on x.tenant_id = e.tenant_id and x.id = e.branch"),
        "Grade" => ("x.grade_name", "left join e_employee_grade x on x.tenant_id = e.tenant_id and x.id = e.grade"),
        "Employment Type" => ("x.employment_type_name", "left join e_employment_type x on x.tenant_id = e.tenant_id and x.id = e.employment_type"),
        "Gender" => ("e.gender", ""),
        _ => ("x.department_name", "left join e_department x on x.tenant_id = e.tenant_id and x.id = e.department"),
    };
    let sql = format!(
        "select coalesce({label}, '(not set)'), \
         count(*) filter (where e.date_of_joining <= $1 and (e.relieving_date is null or e.relieving_date > $1)), \
         count(*) filter (where e.date_of_joining between $2 and $1), \
         count(*) filter (where e.relieving_date between $2 and $1) \
         from e_employee e {join} where e.tenant_id = $3 and e.company = $4 and ($5::text is null or lower(e.\"user\") = $5) \
         group by 1 order by 1"
    );
    let data: Vec<(String, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(to)
        .bind(from)
        .bind(ctx.tenant)
        .bind(company)
        .bind(own_only(ctx, "Employee"))
        .fetch_all(&mut **tx)
        .await?;
    let (mut hc, mut joined, mut left) = (0, 0, 0);
    let rows: Vec<Value> = data
        .into_iter()
        .map(|(group, h, j, l)| {
            hc += h;
            joined += j;
            left += l;
            json!({"group": group, "headcount": h, "joined": j, "left": l})
        })
        .collect();
    // Attrition: leavers over the average headcount of the period.
    let start_count: i64 = hc - joined + left;
    let avg = Decimal::from(start_count + hc) / Decimal::TWO;
    let attrition = if avg.is_zero() { Decimal::ZERO } else { (Decimal::from(left) * Decimal::ONE_HUNDRED / avg).round_dp(2) };
    Ok(json!({
        "columns": [col("group", p.str("group_by").unwrap_or("Department"), "data"), col("headcount", "Headcount", "int"),
            col("joined", "Joined", "int"), col("left", "Left", "int")],
        "rows": rows,
        "totals": {"headcount": hc, "joined": joined, "left": left},
        "meta": {"from_date": from, "to_date": to, "attrition_percent": money(attrition)},
    }))
}

async fn events(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>) -> Result<Value> {
    let from = p.date("from_date")?.unwrap_or_else(crate::today);
    let days: i64 = p.str("days").map(|d| d.parse().map_err(|_| Error::field("days", "must be a whole number"))).transpose()?.unwrap_or(30).clamp(0, 366);
    let until = from + Duration::days(days);
    let mut qb = QueryBuilder::new(
        "select e.number, e.employee_name, d.department_name, e.date_of_birth, e.date_of_joining from e_employee e \
         left join e_department d on d.tenant_id = e.tenant_id and d.id = e.department where e.status = 'Active' and e.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and e.company = ").push_bind(company);
    push_own(&mut qb, &own_only(ctx, "Employee"));
    let data: Vec<(Option<String>, Option<String>, Option<String>, Option<NaiveDate>, Option<NaiveDate>)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let mut found: Vec<(NaiveDate, Value)> = Vec::new();
    for (no, name, dept, born, joined) in data {
        for (event, date) in [("Birthday", born), ("Work Anniversary", joined)] {
            let Some(date) = date else { continue };
            let (next, years) = next_occurrence(date, from);
            if next <= until && years > 0 {
                found.push((next, json!({"date": next, "event": event, "employee": no, "employee_name": name, "department": dept, "years": years})));
            }
        }
    }
    found.sort_by_key(|a| a.0);
    Ok(json!({
        "columns": [col("date", "Date", "date"), col("event", "Event", "data"), col("employee", "Employee", "data"), col("employee_name", "Name", "data"),
            col("department", "Department", "data"), col("years", "Years", "int")],
        "rows": found.into_iter().map(|f| f.1).collect::<Vec<_>>(),
        "meta": {"from_date": from, "to_date": until},
    }))
}

fn code(status: &str) -> &'static str {
    match status {
        "Present" => "P",
        "Absent" => "A",
        "Half Day" => "HD",
        "On Leave" => "L",
        "Work From Home" => "WFH",
        _ => "",
    }
}

async fn attendance_sheet(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>) -> Result<Value> {
    let any = p.date("month")?.unwrap_or_else(crate::today);
    let first = any.with_day(1).unwrap_or(any);
    let last = (first + Duration::days(32)).with_day(1).map(|d| d - Duration::days(1)).unwrap_or(any);
    let mut qb = QueryBuilder::new("select e.id, e.number, e.employee_name from e_employee e where e.tenant_id = ");
    qb.push_bind(ctx.tenant).push(" and e.company = ").push_bind(company);
    qb.push(" and e.date_of_joining <= ").push_bind(last).push(" and (e.relieving_date is null or e.relieving_date >= ").push_bind(first).push(")");
    push_eq(&mut qb, "e.department", p.uuid("department")?);
    push_own(&mut qb, &own_only(ctx, "Attendance"));
    qb.push(" order by e.employee_name, e.number limit ").push_bind(MAX_ROWS as i64);
    let people: Vec<(Uuid, Option<String>, Option<String>)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let ids: Vec<Uuid> = people.iter().map(|p| p.0).collect();
    let marks: Vec<(Uuid, NaiveDate, String)> = sqlx::query_as(
        "select employee, attendance_date, status from e_attendance where tenant_id = $1 and employee = any($2) and docstatus = 1 \
         and attendance_date between $3 and $4",
    )
    .bind(ctx.tenant)
    .bind(&ids)
    .bind(first)
    .bind(last)
    .fetch_all(&mut **tx)
    .await?;
    let mut by: HashMap<(Uuid, NaiveDate), String> = marks.into_iter().map(|(e, d, s)| ((e, d), s)).collect();
    let mut holidays: HashMap<Uuid, std::collections::BTreeSet<NaiveDate>> = HashMap::new();
    for id in &ids {
        holidays.insert(*id, crate::calendar::employee_holidays(tx, ctx.tenant, *id, first, last).await?);
    }
    let days: Vec<NaiveDate> = crate::calendar::days(first, last).collect();
    let mut rows = Vec::new();
    for (emp, no, name) in people {
        let mut r = Map::new();
        r.insert("employee".into(), json!(no));
        r.insert("employee_name".into(), json!(name));
        let mut counts: BTreeMap<&str, Decimal> = BTreeMap::new();
        for day in &days {
            let status = by.remove(&(emp, *day));
            let cell = match &status {
                Some(s) => code(s),
                None if holidays.get(&emp).is_some_and(|h| h.contains(day)) => "H",
                None => "",
            };
            r.insert(format!("d{}", day.day()), json!(cell));
            let key = match cell {
                "P" | "WFH" => "present",
                "HD" => "half_day",
                "A" => "absent",
                "L" => "on_leave",
                "H" => "holidays",
                _ => continue,
            };
            *counts.entry(key).or_default() += Decimal::ONE;
            if cell == "WFH" {
                *counts.entry("work_from_home").or_default() += Decimal::ONE;
            }
        }
        for k in ["present", "half_day", "absent", "on_leave", "work_from_home", "holidays"] {
            r.insert(k.into(), json!(counts.get(k).copied().unwrap_or_default().to_string()));
        }
        rows.push(Value::Object(r));
    }
    let mut columns = vec![col("employee", "Employee", "data"), col("employee_name", "Name", "data")];
    columns.extend(days.iter().map(|d| col(&format!("d{}", d.day()), &d.day().to_string(), "data")));
    for (k, l) in
        [("present", "Present"), ("half_day", "Half Days"), ("absent", "Absent"), ("on_leave", "On Leave"), ("work_from_home", "WFH"), ("holidays", "Holidays")]
    {
        columns.push(col(k, l, "decimal"));
    }
    let names: Vec<String> = ["present", "half_day", "absent", "on_leave", "work_from_home", "holidays"].iter().map(|s| s.to_string()).collect();
    let totals = sum_columns(&rows, &names);
    Ok(json!({"columns": columns, "rows": rows, "totals": totals, "meta": {"from_date": first, "to_date": last,
        "legend": {"P": "Present", "A": "Absent", "HD": "Half Day", "L": "On Leave", "WFH": "Work From Home", "H": "Holiday"}}}))
}

async fn leave_balance(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>) -> Result<Value> {
    let as_of = p.date("as_of")?.unwrap_or_else(crate::today);
    let mut qb = QueryBuilder::new("with p as (select ");
    qb.push_bind(as_of).push(
        "::date as as_of) select e.number, e.employee_name, t.leave_type_name, a.from_date, a.to_date, \
         coalesce(sum(l.leaves) filter (where l.transaction_type = 'Leave Allocation' and not l.is_carry_forward), 0), \
         coalesce(sum(l.leaves) filter (where l.transaction_type = 'Leave Allocation' and l.is_carry_forward), 0), \
         coalesce(-sum(l.leaves) filter (where l.transaction_type = 'Leave Application' and l.from_date <= p.as_of), 0), \
         coalesce(-sum(l.leaves) filter (where l.transaction_type = 'Leave Application' and l.from_date > p.as_of), 0), \
         coalesce(-sum(l.leaves) filter (where l.transaction_type = 'Leave Encashment' and l.from_date <= p.as_of), 0), \
         coalesce(-sum(l.leaves) filter (where l.transaction_type = 'Expiry' and l.from_date <= p.as_of), 0), \
         (select coalesce(sum(x.total_leave_days), 0) from e_leave_application x where x.tenant_id = a.tenant_id and x.employee = a.employee \
          and x.leave_type = a.leave_type and x.docstatus = 0 and x.from_date <= a.to_date and x.to_date >= a.from_date) \
         from p, e_leave_allocation a join e_employee e on e.tenant_id = a.tenant_id and e.id = a.employee \
         join e_leave_type t on t.tenant_id = a.tenant_id and t.id = a.leave_type \
         left join e_leave_ledger_entry l on l.tenant_id = a.tenant_id and l.allocation = a.id \
         where a.docstatus = 1 and a.from_date <= p.as_of and a.to_date >= p.as_of and a.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and a.company = ").push_bind(company);
    push_eq(&mut qb, "a.employee", p.uuid("employee")?);
    push_eq(&mut qb, "a.leave_type", p.uuid("leave_type")?);
    push_eq(&mut qb, "e.department", p.uuid("department")?);
    push_own(&mut qb, &own_only(ctx, "LeaveAllocation"));
    qb.push(
        " group by e.number, e.employee_name, t.leave_type_name, a.id, a.from_date, a.to_date, a.tenant_id, a.employee, a.leave_type \
             order by e.employee_name, t.leave_type_name limit ",
    )
    .push_bind((MAX_ROWS + 1) as i64);
    #[allow(clippy::type_complexity)]
    let data: Vec<(Option<String>, Option<String>, String, NaiveDate, NaiveDate, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal)> =
        qb.build_query_as().fetch_all(&mut **tx).await?;
    let truncated = data.len() > MAX_ROWS;
    let rows: Vec<Value> = data
        .into_iter()
        .take(MAX_ROWS)
        .map(|(no, name, lt, from, to, new, carried, taken, booked, encashed, expired, pending)| {
            let allocated = new + carried;
            let balance = allocated - taken - encashed - expired;
            json!({"employee": no, "employee_name": name, "leave_type": lt, "from_date": from, "to_date": to,
                "allocated": money(allocated), "carried_forward": money(carried), "taken": money(taken), "booked": money(booked),
                "encashed": money(encashed), "expired": money(expired), "pending": money(pending), "balance": money(balance),
                "available": money(balance - booked - pending)})
        })
        .collect();
    Ok(json!({
        "columns": [col("employee", "Employee", "data"), col("employee_name", "Name", "data"), col("leave_type", "Leave Type", "data"),
            col("from_date", "Allocated From", "date"), col("to_date", "To", "date"), col("allocated", "Allocated", "decimal"),
            col("carried_forward", "Carried Forward", "decimal"), col("taken", "Taken", "decimal"), col("booked", "Approved Ahead", "decimal"),
            col("encashed", "Encashed", "decimal"), col("expired", "Expired", "decimal"), col("pending", "Pending Approval", "decimal"),
            col("balance", "Balance", "decimal"), col("available", "Available", "decimal")],
        "rows": rows,
        "meta": {"as_of": as_of, "truncated": truncated},
    }))
}

async fn expense_summary(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>, from: NaiveDate, to: NaiveDate) -> Result<Value> {
    let mut qb = QueryBuilder::new(
        "select c.id, c.number, e.employee_name, c.posting_date, c.status, coalesce(c.total_claimed_amount, 0), coalesce(c.total_sanctioned_amount, 0), \
         coalesce(c.total_taxes_and_charges, 0), coalesce(c.total_advance_amount, 0), coalesce(c.total_amount_reimbursed, 0), coalesce(c.outstanding_amount, 0) \
         from e_expense_claim c join e_employee e on e.tenant_id = c.tenant_id and e.id = c.employee where c.docstatus < 2 and c.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and c.company = ").push_bind(company);
    qb.push(" and c.posting_date between ").push_bind(from).push(" and ").push_bind(to);
    push_eq(&mut qb, "c.employee", p.uuid("employee")?);
    if let Some(st) = p.str("status") {
        qb.push(" and c.status = ").push_bind(st.to_string());
    }
    push_own(&mut qb, &own_only(ctx, "ExpenseClaim"));
    qb.push(" order by c.posting_date, c.number limit ").push_bind((MAX_ROWS + 1) as i64);
    #[allow(clippy::type_complexity)]
    let data: Vec<(Uuid, Option<String>, Option<String>, NaiveDate, Option<String>, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal)> =
        qb.build_query_as().fetch_all(&mut **tx).await?;
    let truncated = data.len() > MAX_ROWS;
    let rows: Vec<Value> = data
        .into_iter()
        .take(MAX_ROWS)
        .map(|(cid, no, name, day, status, claimed, sanctioned, taxes, advances, paid, outstanding)| {
            json!({"claim_id": cid, "claim": no, "employee_name": name, "posting_date": day, "status": status, "claimed": money(claimed),
                "sanctioned": money(sanctioned), "taxes": money(taxes), "advances": money(advances), "reimbursed": money(paid), "outstanding": money(outstanding)})
        })
        .collect();
    // Totals over approved claims only: drafts and rejections are not money owed.
    let approved: Vec<Value> = rows.iter().filter(|r| matches!(r["status"].as_str(), Some("Unpaid" | "Paid"))).cloned().collect();
    let names: Vec<String> = ["claimed", "sanctioned", "taxes", "advances", "reimbursed", "outstanding"].iter().map(|s| s.to_string()).collect();
    Ok(json!({
        "columns": [col("claim", "Claim", "data"), col("employee_name", "Employee", "data"), col("posting_date", "Date", "date"), col("status", "Status", "data"),
            col("claimed", "Claimed", "currency"), col("sanctioned", "Sanctioned", "currency"), col("taxes", "Taxes", "currency"),
            col("advances", "Advances Settled", "currency"), col("reimbursed", "Reimbursed", "currency"), col("outstanding", "Outstanding", "currency")],
        "rows": rows,
        "totals": sum_columns(&approved, &names),
        "meta": {"from_date": from, "to_date": to, "truncated": truncated, "totals_cover": "approved claims"},
    }))
}

async fn pipeline(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>) -> Result<Value> {
    let mut qb = QueryBuilder::new(
        "select o.number, o.job_title, g.designation_name, o.status, o.vacancies, coalesce(o.hired, 0), \
         count(distinct a.id) filter (where a.status = 'Open'), count(distinct a.id) filter (where a.status = 'Replied'), \
         count(distinct a.id) filter (where a.status = 'Hold'), count(distinct a.id) filter (where a.status = 'Rejected'), \
         count(distinct a.id) filter (where a.status = 'Accepted'), count(distinct i.id), \
         count(distinct f.id) filter (where f.docstatus = 1 and f.status = 'Awaiting Response'), \
         count(distinct f.id) filter (where f.docstatus = 1 and f.status = 'Accepted'), count(distinct f.id) filter (where f.docstatus = 1 and f.status = 'Rejected') \
         from e_job_opening o left join e_designation g on g.tenant_id = o.tenant_id and g.id = o.designation \
         left join e_job_applicant a on a.tenant_id = o.tenant_id and a.job_opening = o.id \
         left join e_interview i on i.tenant_id = o.tenant_id and i.job_opening = o.id \
         left join e_job_offer f on f.tenant_id = o.tenant_id and f.job_opening = o.id where o.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and o.company = ").push_bind(company);
    if let Some(st) = p.str("status") {
        qb.push(" and o.status = ").push_bind(st.to_string());
    }
    qb.push(" group by o.tenant_id, o.id, g.designation_name order by o.created_at desc");
    #[allow(clippy::type_complexity)]
    let data: Vec<(Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64)> =
        qb.build_query_as().fetch_all(&mut **tx).await?;
    let keys = ["open", "replied", "hold", "rejected", "accepted", "interviews", "offers_pending", "offers_accepted", "offers_rejected"];
    let rows: Vec<Value> = data
        .into_iter()
        .map(|(no, title, desig, status, vacancies, hired, a, b, c, d, e, i, f1, f2, f3)| {
            let mut r = json!({"opening": no, "job_title": title, "designation": desig, "status": status, "vacancies": vacancies, "hired": hired});
            for (k, v) in keys.iter().zip([a, b, c, d, e, i, f1, f2, f3]) {
                r[*k] = json!(v);
            }
            r
        })
        .collect();
    let mut totals = Map::new();
    for k in ["hired"].iter().chain(keys.iter()) {
        totals.insert(k.to_string(), json!(rows.iter().filter_map(|r| r[*k].as_i64()).sum::<i64>()));
    }
    Ok(json!({
        "columns": [col("opening", "Opening", "data"), col("job_title", "Job", "data"), col("designation", "Designation", "data"), col("status", "Status", "data"),
            col("vacancies", "Vacancies", "int"), col("hired", "Hired", "int"), col("open", "Applied", "int"), col("replied", "Replied", "int"),
            col("hold", "On Hold", "int"), col("rejected", "Rejected", "int"), col("accepted", "Accepted", "int"), col("interviews", "Interviews", "int"),
            col("offers_pending", "Offers Pending", "int"), col("offers_accepted", "Offers Accepted", "int"), col("offers_rejected", "Offers Declined", "int")],
        "rows": rows,
        "totals": totals,
    }))
}

// ───────────────────────────── payroll ─────────────────────────────

struct Slip {
    id: Uuid,
    number: Option<String>,
    employee: Option<String>,
    employee_name: Option<String>,
    department: Option<String>,
    designation: Option<String>,
    start: NaiveDate,
    end: NaiveDate,
    payment_days: Decimal,
    gross: Decimal,
    deductions: Decimal,
    net: Decimal,
    taxable: Decimal,
    tax: Decimal,
    employee_id: Uuid,
}

struct RegisterData {
    slips: Vec<Slip>,
    /// (abbr, name, type) in display order: earnings, then deductions.
    components: Vec<(String, String, String)>,
    amounts: HashMap<(Uuid, String), Decimal>,
    truncated: bool,
}

async fn register_data(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>, from: NaiveDate, to: NaiveDate, submitted: bool) -> Result<RegisterData> {
    let mut qb = QueryBuilder::new(
        "select ss.id, ss.number, e.number, ss.employee_name, d.department_name, g.designation_name, ss.start_date, ss.end_date, \
         coalesce(ss.payment_days, 0), coalesce(ss.gross_pay, 0), coalesce(ss.total_deduction, 0), coalesce(ss.net_pay, 0), \
         coalesce(ss.taxable_earnings, 0), coalesce(ss.income_tax_deducted, 0), e.id \
         from e_salary_slip ss join e_employee e on e.tenant_id = ss.tenant_id and e.id = ss.employee \
         left join e_department d on d.tenant_id = ss.tenant_id and d.id = ss.department \
         left join e_designation g on g.tenant_id = ss.tenant_id and g.id = ss.designation where ss.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and ss.company = ").push_bind(company);
    qb.push(" and ss.docstatus = ").push_bind(if submitted { 1i16 } else { 0i16 });
    qb.push(" and ss.start_date >= ").push_bind(from).push(" and ss.end_date <= ").push_bind(to);
    push_eq(&mut qb, "ss.department", p.uuid("department")?);
    push_own(&mut qb, &own_only(ctx, "SalarySlip"));
    qb.push(" order by ss.start_date, ss.employee_name limit ").push_bind((MAX_ROWS + 1) as i64);
    #[allow(clippy::type_complexity)]
    let data: Vec<(
        Uuid,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        NaiveDate,
        NaiveDate,
        Decimal,
        Decimal,
        Decimal,
        Decimal,
        Decimal,
        Decimal,
        Uuid,
    )> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let truncated = data.len() > MAX_ROWS;
    let slips: Vec<Slip> = data
        .into_iter()
        .take(MAX_ROWS)
        .map(|(id, number, employee, employee_name, department, designation, start, end, payment_days, gross, deductions, net, taxable, tax, employee_id)| {
            Slip { id, number, employee, employee_name, department, designation, start, end, payment_days, gross, deductions, net, taxable, tax, employee_id }
        })
        .collect();
    let ids: Vec<Uuid> = slips.iter().map(|s| s.id).collect();
    let lines: Vec<(Uuid, String, String, String, Decimal)> = sqlx::query_as(
        "select r.parent_id, c.abbr, c.component_name, c.component_type, r.amount from e_salary_slip__earnings r \
         join e_salary_component c on c.tenant_id = r.tenant_id and c.id = r.salary_component where r.tenant_id = $1 and r.parent_id = any($2) \
         union all select r.parent_id, c.abbr, c.component_name, c.component_type, r.amount from e_salary_slip__deductions r \
         join e_salary_component c on c.tenant_id = r.tenant_id and c.id = r.salary_component where r.tenant_id = $1 and r.parent_id = any($2)",
    )
    .bind(ctx.tenant)
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    let mut components: BTreeMap<(bool, String), (String, String, String)> = BTreeMap::new();
    let mut amounts = HashMap::new();
    for (slip, abbr, name, kind, amount) in lines {
        components.insert((kind == "Deduction", name.clone()), (abbr.clone(), name, kind));
        *amounts.entry((slip, abbr)).or_insert(Decimal::ZERO) += amount;
    }
    Ok(RegisterData { slips, components: components.into_values().collect(), amounts, truncated })
}

fn component_key(abbr: &str) -> String {
    format!("c_{abbr}")
}

fn salary_register(data: RegisterData) -> Value {
    let mut columns = vec![
        col("slip", "Salary Slip", "data"),
        col("employee", "Employee", "data"),
        col("employee_name", "Name", "data"),
        col("department", "Department", "data"),
        col("designation", "Designation", "data"),
        col("start_date", "From", "date"),
        col("end_date", "To", "date"),
        col("payment_days", "Payment Days", "decimal"),
    ];
    let mut numeric: Vec<String> = Vec::new();
    for (abbr, name, _) in &data.components {
        columns.push(col(&component_key(abbr), name, "currency"));
        numeric.push(component_key(abbr));
    }
    for (k, l) in [("gross_pay", "Gross Pay"), ("total_deduction", "Total Deduction"), ("net_pay", "Net Pay")] {
        columns.push(col(k, l, "currency"));
        numeric.push(k.to_string());
    }
    let rows: Vec<Value> = data
        .slips
        .iter()
        .map(|s| {
            let mut r = json!({"slip_id": s.id, "slip": s.number, "employee": s.employee, "employee_name": s.employee_name, "department": s.department,
                "designation": s.designation, "start_date": s.start, "end_date": s.end, "payment_days": money(s.payment_days),
                "gross_pay": money(s.gross), "total_deduction": money(s.deductions), "net_pay": money(s.net)});
            for (abbr, _, _) in &data.components {
                r[component_key(abbr)] = money(data.amounts.get(&(s.id, abbr.clone())).copied().unwrap_or_default());
            }
            r
        })
        .collect();
    let totals = sum_columns(&rows, &numeric);
    json!({"columns": columns, "rows": rows, "totals": totals, "meta": {"truncated": data.truncated}})
}

fn payroll_summary(data: RegisterData) -> Value {
    let mut groups: BTreeMap<String, (i64, BTreeMap<String, Decimal>)> = BTreeMap::new();
    for s in &data.slips {
        let g = groups.entry(s.department.clone().unwrap_or_else(|| "(no department)".into())).or_default();
        g.0 += 1;
        for (k, v) in [("gross_pay", s.gross), ("total_deduction", s.deductions), ("net_pay", s.net)] {
            *g.1.entry(k.to_string()).or_default() += v;
        }
        for (abbr, _, _) in &data.components {
            *g.1.entry(component_key(abbr)).or_default() += data.amounts.get(&(s.id, abbr.clone())).copied().unwrap_or_default();
        }
    }
    let mut columns = vec![col("department", "Department", "data"), col("employees", "Slips", "int")];
    let mut numeric = Vec::new();
    for (abbr, name, _) in &data.components {
        columns.push(col(&component_key(abbr), name, "currency"));
        numeric.push(component_key(abbr));
    }
    for (k, l) in [("gross_pay", "Gross Pay"), ("total_deduction", "Total Deduction"), ("net_pay", "Net Pay")] {
        columns.push(col(k, l, "currency"));
        numeric.push(k.to_string());
    }
    let rows: Vec<Value> = groups
        .into_iter()
        .map(|(dept, (n, sums))| {
            let mut r = Map::new();
            r.insert("department".into(), json!(dept));
            r.insert("employees".into(), json!(n));
            for k in &numeric {
                r.insert(k.clone(), money(sums.get(k).copied().unwrap_or_default()));
            }
            Value::Object(r)
        })
        .collect();
    let totals = sum_columns(&rows, &numeric);
    json!({"columns": columns, "rows": rows, "totals": totals, "meta": {"truncated": data.truncated}})
}

async fn income_tax(tx: &mut Tx, ctx: &Ctx, data: RegisterData) -> Result<Value> {
    let tax_id = ctx.schema.entity("Employee").and_then(|e| e.field("tax_id").cloned()).is_some_and(|f| ctx.perms.can_read_field("Employee", &f));
    let mut per: BTreeMap<(String, Uuid), (Option<String>, i64, Decimal, Decimal, Decimal)> = BTreeMap::new();
    for s in &data.slips {
        let e = per.entry((s.employee_name.clone().unwrap_or_default(), s.employee_id)).or_insert((
            s.employee.clone(),
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
        ));
        e.1 += 1;
        e.2 += s.gross;
        e.3 += s.taxable;
        e.4 += s.tax;
    }
    let ids: Vec<Uuid> = per.keys().map(|k| k.1).collect();
    let pans: HashMap<Uuid, Option<String>> = if tax_id {
        sqlx::query_as::<_, (Uuid, Option<String>)>("select id, tax_id from e_employee where tenant_id = $1 and id = any($2)")
            .bind(ctx.tenant)
            .bind(&ids)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let rows: Vec<Value> = per
        .into_iter()
        .map(|((name, emp), (no, n, gross, taxable, tax))| {
            json!({"employee": no, "employee_name": name, "tax_id": pans.get(&emp).cloned().flatten(), "slips": n, "gross_pay": money(gross),
                "taxable_earnings": money(taxable), "income_tax": money(tax)})
        })
        .collect();
    let mut columns = vec![col("employee", "Employee", "data"), col("employee_name", "Name", "data")];
    if tax_id {
        columns.push(col("tax_id", "Tax ID", "data"));
    }
    columns.extend([
        col("slips", "Slips", "int"),
        col("gross_pay", "Gross Pay", "currency"),
        col("taxable_earnings", "Taxable Earnings", "currency"),
        col("income_tax", "Income Tax", "currency"),
    ]);
    let names: Vec<String> = ["gross_pay", "taxable_earnings", "income_tax"].iter().map(|s| s.to_string()).collect();
    let totals = sum_columns(&rows, &names);
    Ok(json!({"columns": columns, "rows": rows, "totals": totals, "meta": {"truncated": data.truncated}}))
}

async fn bank_remittance(tx: &mut Tx, ctx: &Ctx, company: Uuid, p: &Params<'_>, from: NaiveDate, to: NaiveDate) -> Result<Value> {
    let readable = ctx.schema.entity("Employee").and_then(|e| e.field("bank_account_no").cloned()).is_some_and(|f| ctx.perms.can_read_field("Employee", &f));
    if !readable {
        return Err(Error::Forbidden("bank details are restricted to payroll and HR managers".into()));
    }
    let mut qb = QueryBuilder::new(
        "select e.number, ss.employee_name, e.salary_mode, e.bank_name, e.bank_account_no, e.ifsc_code, e.iban, ss.net_pay, ss.number \
         from e_salary_slip ss join e_employee e on e.tenant_id = ss.tenant_id and e.id = ss.employee where ss.docstatus = 1 and ss.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and ss.company = ").push_bind(company);
    match p.uuid("payroll_entry")? {
        Some(run) => {
            qb.push(" and ss.payroll_entry = ").push_bind(run);
        }
        None => {
            qb.push(" and ss.start_date >= ").push_bind(from).push(" and ss.end_date <= ").push_bind(to);
        }
    }
    push_own(&mut qb, &own_only(ctx, "PayrollEntry"));
    qb.push(" order by e.salary_mode, ss.employee_name");
    #[allow(clippy::type_complexity)]
    let data: Vec<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Decimal>,
        Option<String>,
    )> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let rows: Vec<Value> = data
        .into_iter()
        .map(|(no, name, mode, bank, account, ifsc, iban, net, slip)| {
            json!({"employee": no, "employee_name": name, "salary_mode": mode, "bank_name": bank, "bank_account_no": account, "ifsc_code": ifsc,
                "iban": iban, "net_pay": money(net.unwrap_or_default()), "slip": slip})
        })
        .collect();
    let missing = rows.iter().filter(|r| r["salary_mode"].as_str().unwrap_or("Bank") == "Bank" && r["bank_account_no"].is_null()).count();
    let totals = sum_columns(&rows, &["net_pay".to_string()]);
    Ok(json!({
        "columns": [col("employee", "Employee", "data"), col("employee_name", "Name", "data"), col("salary_mode", "Mode", "data"),
            col("bank_name", "Bank", "data"), col("bank_account_no", "Account No", "data"), col("ifsc_code", "IFSC / Routing", "data"),
            col("iban", "IBAN", "data"), col("net_pay", "Net Pay", "currency"), col("slip", "Salary Slip", "data")],
        "rows": rows,
        "totals": totals,
        "meta": {"missing_bank_details": missing},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn birthdays_and_anniversaries_roll_forward() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(next_occurrence(d("1990-11-05"), d("2026-10-29")), (d("2026-11-05"), 36));
        assert_eq!(next_occurrence(d("1990-01-05"), d("2026-10-29")), (d("2027-01-05"), 37));
        assert_eq!(next_occurrence(d("1992-02-29"), d("2026-10-29")), (d("2027-02-28"), 35));
        assert_eq!(next_occurrence(d("2026-10-29"), d("2026-10-29")), (d("2026-10-29"), 0));
    }
}
