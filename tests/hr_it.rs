//! End-to-end HR and payroll tests against a real PostgreSQL (`FERRUM_HR_TEST_DATABASE_URL`,
//! default `postgres://ferrum:ferrum@localhost:5432/ferrum_hr_test`). The database is reset once;
//! each scenario runs in its own tenant, so they run side by side like real customers.
//!
//! Users sign in with real roles (HR manager, leave/expense approver, employee, payroll manager,
//! accountant, attendance device) and drive the documents through the kernel API.

use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, ListQuery};
use ferrum_meta::Catalog;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use uuid::Uuid;

const PW: &str = "Correct-Horse-9";

static RT: LazyLock<tokio::runtime::Runtime> =
    // Same runtime shape as the server: unoptimized async hook chains need more than 2 MiB.
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap());
static KERNEL: tokio::sync::OnceCell<Arc<Kernel>> = tokio::sync::OnceCell::const_new();

async fn kernel() -> Arc<Kernel> {
    KERNEL
        .get_or_init(|| async {
            let url = std::env::var("FERRUM_HR_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_hr_test".into());
            let db = Db::connect(DbConfig { url, max_connections: 24, ..Default::default() }).await.expect("connect");
            sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
            let mut packages = ferrum_domain::packages().expect("platform packages");
            packages.extend(ferrum_app_erp::packages().expect("ERP's packages"));
            packages.extend(ferrum_app_hr::packages().expect("app packages"));
            let catalog = Catalog::from_packages(packages).expect("catalog");
            let mut hooks = HookRegistry::default();
            ferrum_domain::register(&mut hooks);
            ferrum_app_erp::register(&mut hooks);
            for source in ferrum_app_erp::reports() {
                ferrum_domain::reports::add_source(source);
            }
            ferrum_app_hr::register(&mut hooks);
            for source in ferrum_app_hr::reports() {
                ferrum_domain::reports::add_source(source);
            }
            let k = Kernel::boot(db, catalog, hooks, KernelSettings::default(), None).await.expect("boot");
            ferrum_domain::maintenance(&k).await.expect("journal partitions");
            ferrum_app_erp::maintenance(&k).await.expect("journal partitions");
            k
        })
        .await
        .clone()
}

fn run<F: std::future::Future<Output = ()> + Send + 'static>(scenario: F) {
    RT.block_on(async { tokio::spawn(scenario).await.unwrap() });
}

#[test]
fn leave_management() {
    run(leave_scenario());
}

#[test]
fn attendance_from_checkins() {
    run(attendance_scenario());
}

#[test]
fn attendance_requests() {
    run(attendance_request_scenario());
}

#[test]
fn leave_accrual() {
    run(accrual_scenario());
}

#[test]
fn approvals_calendar_directory_and_careers() {
    run(site_scenario());
}

#[test]
fn expense_claims_and_advances() {
    run(expense_scenario());
}

#[test]
fn payroll_run() {
    run(payroll_scenario());
}

#[test]
fn recruitment_lifecycle_and_appraisal() {
    run(talent_scenario());
}

#[test]
fn hire_to_retire() {
    run(retire_scenario());
}

#[test]
fn employer_contributions() {
    run(contribution_scenario());
}

#[test]
fn opening_figures_and_a_leavers_tax() {
    run(opening_scenario());
}

#[test]
fn notice_and_full_and_final_settlement() {
    run(notice_scenario());
}

#[test]
fn payroll_corrections_off_cycle_and_held_salaries() {
    run(corrections_scenario());
}

#[test]
fn salary_arrears() {
    run(arrears_scenario());
}

#[test]
fn payments_made_before_the_upgrade() {
    run(upgrade_scenario());
}

#[test]
fn hr_and_payroll_without_erp() {
    run(alone_scenario());
}

/// HR and payroll in a workspace without ERP (Ferrum docs/plan/apps-ui.md §13): no expense claims,
/// accounts or journals; a payroll run makes and submits salary slips and posts nothing.
async fn alone_scenario() {
    let t = T::with("hralone", &["payroll"]).await;
    let schema = t.admin.schema.clone();
    assert_eq!(schema.packages, ["core", "hr", "payroll"]);
    for erp in ["Account", "JournalEntry", "ExpenseClaim", "EmployeeAdvance", "FullAndFinalStatement", "SalaryComponentAccount"] {
        assert!(schema.entity(erp).is_none(), "{erp} without ERP");
    }
    let separation = schema.entity("EmployeeSeparation").unwrap();
    assert!(separation.def.actions.iter().all(|a| a.name != "make_full_and_final"), "settled through ERP, so only with it");
    let pe = schema.entity("PayrollEntry").unwrap();
    assert!(pe.field("payroll_payable_account").is_none() && pe.def.actions.iter().all(|a| a.name != "make_bank_entry"));
    assert!(schema.entity("Employee").unwrap().field("payroll_cost_center").is_none());

    let asha = t.employee("Asha", json!({})).await;
    let flat = t
        .ins(
            &t.admin,
            "SalaryComponent",
            json!({"component_name": "Flat Pay", "abbr": "FLAT", "component_type": "Earning", "amount": "30000", "depends_on_payment_days": false}),
        )
        .await;
    let st = t
        .ins_submit(
            &t.admin,
            "SalaryStructure",
            json!({"structure_name": "Flat", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [{"salary_component": flat["id"]}]}),
        )
        .await;
    t.ins_submit(
        &t.admin,
        "SalaryStructureAssignment",
        json!({"employee": asha["id"], "salary_structure": st["id"], "from_date": "2026-04-01", "base": "30000"}),
    )
    .await;
    let pe =
        t.ins(&t.admin, "PayrollEntry", json!({"company": t.c(), "posting_date": "2026-09-30", "start_date": "2026-09-01", "end_date": "2026-09-30"})).await;
    let pe = t.act(&t.admin, "PayrollEntry", uid(&pe), "get_employees", json!({})).await;
    let pe_id = vid(&pe);
    t.submit(&t.admin, "PayrollEntry", pe_id).await;
    t.act(&t.admin, "PayrollEntry", pe_id, "create_salary_slips", json!({})).await;
    let done = t.act(&t.admin, "PayrollEntry", pe_id, "submit_salary_slips", json!({})).await;
    assert_eq!(done["status"], json!("Slips Submitted"));
    let slips = t.list(&t.admin, "SalarySlip", json!({"payroll_entry": pe_id.to_string()})).await;
    assert_eq!((slips.len(), dec(&slips[0]["net_pay"])), (1, n("30000")));
}

// ───────────────────────────── helpers ─────────────────────────────

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap_or_else(|| panic!("not an object: {v}"))
}

fn dec(v: &Value) -> Decimal {
    match v {
        Value::String(s) => Decimal::from_str(s).unwrap_or_else(|_| panic!("not a decimal: {s}")),
        Value::Number(n) => Decimal::from_str(&n.to_string()).unwrap(),
        Value::Null => Decimal::ZERO,
        other => panic!("not a decimal: {other}"),
    }
}

fn n(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn uid(doc: &Doc) -> Uuid {
    doc["id"].as_str().unwrap().parse().unwrap()
}

fn vid(v: &Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

fn describe(e: &Error) -> String {
    match e {
        Error::Validation(f) => f.iter().map(|x| format!("{}: {}", x.field, x.message)).collect::<Vec<_>>().join("; "),
        other => other.to_string(),
    }
}

#[track_caller]
fn refused<T: std::fmt::Debug>(r: ferrum_kernel::Result<T>, needle: &str) {
    match r {
        Ok(v) => panic!("expected a refusal containing '{needle}', got {v:?}"),
        Err(e) => assert!(describe(&e).contains(needle), "expected '{needle}' in: {}", describe(&e)),
    }
}

#[track_caller]
fn forbidden<T: std::fmt::Debug>(r: ferrum_kernel::Result<T>) {
    assert!(matches!(r, Err(Error::Forbidden(_))), "expected forbidden, got {r:?}");
}

struct T {
    k: Arc<Kernel>,
    tenant: Uuid,
    admin: Ctx,
    company: Uuid,
}

impl T {
    /// A workspace with HR and payroll and their connectors with ERP.
    async fn new(slug: &str) -> T {
        T::with(slug, &["hr_erp", "payroll_erp"]).await
    }

    async fn with(slug: &str, packages: &[&str]) -> T {
        let k = kernel().await;
        let tenant = k
            .create_tenant(NewTenant {
                slug: slug.into(),
                name: format!("{slug} Ltd"),
                admin_email: format!("admin@{slug}.test"),
                admin_password: PW.into(),
                admin_name: None,
                packages: packages.iter().map(|s| s.to_string()).collect(),
                settings: Some(json!({"fiscal_year_start_month": 4, "timezone": "Asia/Kolkata"})),
            })
            .await
            .expect("tenant");
        let session = k.login(slug, &format!("admin@{slug}.test"), PW).await.expect("login");
        let admin = k.load_ctx(tenant, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
        let mut t = T { k, tenant, admin, company: Uuid::nil() };
        let inr = t.one("Currency", json!({"code": "INR"})).await;
        let co = t
            .ins(&t.admin.clone(), "Company", json!({"company_name": format!("{slug} Pvt Ltd"), "abbr": slug.to_uppercase(), "default_currency": inr["id"]}))
            .await;
        t.company = uid(&co);
        // Weekly offs and a national holiday, as the company default.
        let list = t
            .ins(
                &t.admin.clone(),
                "HolidayList",
                json!({"holiday_list_name": "India 2026", "from_date": "2026-01-01", "to_date": "2026-12-31",
                "holidays": [{"holiday_date": "2026-10-02", "description": "Gandhi Jayanti"}]}),
            )
            .await;
        let after = t.act(&t.admin.clone(), "HolidayList", uid(&list), "add_weekly_offs", json!({"weekdays": ["Saturday", "Sunday"]})).await;
        assert_eq!(after["total_holidays"], json!(105), "52 Saturdays + 52 Sundays + 1 holiday");
        t.k.update(&t.admin, "Company", t.company, obj(json!({"hr_default_holiday_list": list["id"]})), None).await.expect("default holiday list");
        t
    }

    fn c(&self) -> String {
        self.company.to_string()
    }

    async fn user(&self, email: &str, roles: &[&str]) -> Ctx {
        let roles: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        let id = self.k.create_user(&self.admin, email, email, Some(PW), &roles, false).await.unwrap_or_else(|e| panic!("user {email}: {e:?}"));
        self.k.load_ctx(self.tenant, id, None, "it".into()).await.expect("user ctx")
    }

    async fn try_ins(&self, ctx: &Ctx, entity: &str, v: Value) -> ferrum_kernel::Result<Doc> {
        self.k.insert(ctx, entity, obj(v)).await
    }

    async fn ins(&self, ctx: &Ctx, entity: &str, v: Value) -> Doc {
        self.try_ins(ctx, entity, v).await.unwrap_or_else(|e| panic!("insert {entity}: {}", describe(&e)))
    }

    async fn submit(&self, ctx: &Ctx, entity: &str, id: Uuid) -> Doc {
        self.k.submit(ctx, entity, id, None).await.unwrap_or_else(|e| panic!("submit {entity}: {}", describe(&e)))
    }

    async fn ins_submit(&self, ctx: &Ctx, entity: &str, v: Value) -> Doc {
        let d = self.ins(ctx, entity, v).await;
        self.submit(ctx, entity, uid(&d)).await
    }

    async fn act(&self, ctx: &Ctx, entity: &str, id: Uuid, action: &str, params: Value) -> Value {
        self.k.run_action(ctx, entity, id, action, params).await.unwrap_or_else(|e| panic!("{entity}.{action}: {}", describe(&e)))
    }

    async fn trans(&self, ctx: &Ctx, entity: &str, id: Uuid, action: &str) -> ferrum_kernel::Result<Doc> {
        self.k.transition(ctx, entity, id, action, None).await
    }

    async fn get(&self, entity: &str, id: Uuid) -> Doc {
        self.k.get(&self.admin, entity, id, false).await.unwrap_or_else(|e| panic!("get {entity}: {}", describe(&e)))
    }

    async fn list(&self, ctx: &Ctx, entity: &str, filters: Value) -> Vec<Doc> {
        let q = ListQuery { filters: ferrum_kernel::query::parse_filters(&filters).unwrap(), limit: Some(500), ..Default::default() };
        self.k.list(ctx, entity, &q).await.unwrap_or_else(|e| panic!("list {entity}: {}", describe(&e))).rows
    }

    async fn one(&self, entity: &str, filters: Value) -> Doc {
        let rows = self.list(&self.admin, entity, filters.clone()).await;
        assert_eq!(rows.len(), 1, "{entity} {filters}");
        rows[0].clone()
    }

    async fn employee(&self, first: &str, extra: Value) -> Doc {
        let mut v = obj(json!({"first_name": first, "last_name": "Test", "company": self.c(), "date_of_joining": "2024-06-01"}));
        v.extend(obj(extra));
        self.ins(&self.admin.clone(), "Employee", Value::Object(v)).await
    }

    async fn account(&self, name: &str) -> String {
        self.one("Account", json!({"account_name": name, "company": self.c()})).await["id"].as_str().unwrap().to_string()
    }

    /// Debit minus credit on an account (by name) across all postings of the tenant.
    async fn gl(&self, account: &str) -> Decimal {
        let mut tx = self.k.db.tenant_tx(self.tenant).await.unwrap();
        let v: Decimal = sqlx::query_scalar(
            "select coalesce(sum(l.debit - l.credit), 0) from journal_line l join e_account a on a.tenant_id = l.tenant_id and a.id = l.account_id \
             where l.tenant_id = $1 and a.account_name = $2",
        )
        .bind(self.tenant)
        .bind(account)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        v
    }

    async fn report(&self, ctx: &Ctx, name: &str, params: &[(&str, &str)]) -> ferrum_kernel::Result<Value> {
        let p: HashMap<String, String> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ferrum_domain::reports::run(&self.k, ctx, name, &p).await
    }

    async fn scalar_i64(&self, sql: &str, id: Uuid) -> i64 {
        let mut tx = self.k.db.tenant_tx(self.tenant).await.unwrap();
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).bind(self.tenant).bind(id).fetch_one(&mut *tx).await.unwrap()
    }
}

fn row<'a>(report: &'a Value, key: &str, value: &str) -> &'a Value {
    report["rows"].as_array().unwrap().iter().find(|r| r[key] == json!(value)).unwrap_or_else(|| panic!("no row {key}={value} in {}", report["rows"]))
}

// ───────────────────────────── leave ─────────────────────────────

async fn leave_scenario() {
    let t = T::new("hrleave").await;
    let hr = t.user("hr@hrleave.test", &["HR Manager"]).await;
    let boss = t.user("boss@hrleave.test", &["Employee", "Leave Approver"]).await;
    let other = t.user("other@hrleave.test", &["Employee", "Leave Approver"]).await;
    let peer = t.user("peer@hrleave.test", &["Employee"]).await;
    let me = t.user("me@hrleave.test", &["Employee"]).await;
    let m = t.employee("Maya", json!({"user": "boss@hrleave.test"})).await;
    let e = t.employee("Esha", json!({"user": "me@hrleave.test", "reports_to": m["id"]})).await;
    let o = t.employee("Omar", json!({"user": "other@hrleave.test"})).await;
    let newbie = t.employee("Neel", json!({"date_of_joining": "2026-08-17"})).await;
    let pia = t.employee("Pia", json!({"user": "peer@hrleave.test"})).await;
    let (eid, mid) = (uid(&e), uid(&m));

    // Own access: the employee sees exactly their own employee record.
    let mine = t.list(&me, "Employee", json!({})).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["employee_name"], json!("Esha Test"));
    refused(t.k.update(&t.admin, "Employee", mid, obj(json!({"reports_to": e["id"]})), None).await, "cycle");
    refused(
        t.try_ins(&t.admin, "Employee", json!({"first_name": "Ghost", "company": t.c(), "date_of_joining": "2026-01-01", "user": "nobody@hrleave.test"})).await,
        "create the user first",
    );

    let lt = |name: &str| {
        let t = &t;
        let name = name.to_string();
        async move { t.one("LeaveType", json!({"leave_type_name": name})).await["id"].as_str().unwrap().to_string() }
    };
    let (casual, sick, privilege, lwp) = (lt("Casual Leave").await, lt("Sick Leave").await, lt("Privilege Leave").await, lt("Leave Without Pay").await);
    let alloc = |emp: Uuid, lt: &str, from: &str, to: &str, days: &str, cf: bool| json!({"employee": emp.to_string(), "leave_type": lt, "from_date": from, "to_date": to, "new_leaves_allocated": days, "carry_forward": cf});
    let cas_alloc = t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &casual, "2026-01-01", "2026-12-31", "10", false)).await;
    t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &sick, "2026-01-01", "2026-12-31", "1", false)).await;
    refused(t.try_ins(&hr, "LeaveAllocation", alloc(eid, &casual, "2026-06-01", "2027-05-31", "5", false)).await, "overlaps allocation");
    refused(t.try_ins(&hr, "LeaveAllocation", alloc(eid, &casual, "2027-01-01", "2027-12-31", "15", false)).await, "at most 12");
    refused(t.try_ins(&hr, "LeaveAllocation", alloc(eid, &lwp, "2027-01-01", "2027-12-31", "5", false)).await, "needs no allocation");

    // Thu 1 Oct to Mon 5 Oct: Friday is a holiday and the weekend is off, so two days.
    let app = |emp: Uuid, lt: &str, from: &str, to: &str| json!({"employee": emp.to_string(), "leave_type": lt, "from_date": from, "to_date": to, "reason": "family"});
    let a1 = t.ins(&me, "LeaveApplication", app(eid, &casual, "2026-10-01", "2026-10-05")).await;
    assert_eq!(dec(&a1["total_leave_days"]), n("2"));
    assert_eq!(dec(&a1["leave_balance"]), n("10"));
    assert_eq!(a1["leave_approver"], json!("boss@hrleave.test"), "defaults to the manager's user");
    assert_eq!(a1["status"], json!("Open"));
    assert_eq!(a1["employee_user"], json!("me@hrleave.test"));
    let notes = t.scalar_i64("select count(*) from notification where tenant_id = $1 and user_id = $2", boss.user).await;
    assert!(notes >= 1, "the approver is notified");
    forbidden(t.try_ins(&me, "LeaveApplication", app(mid, &casual, "2026-11-02", "2026-11-02")).await);
    // Employees cannot pick their approver; approvers cannot approve themselves.
    let mut pick = app(eid, &casual, "2026-11-09", "2026-11-09");
    pick["leave_approver"] = json!("other@hrleave.test");
    let picked = t.ins(&me, "LeaveApplication", pick).await;
    assert_eq!(picked["leave_approver"], json!("boss@hrleave.test"));
    t.k.delete(&me, "LeaveApplication", uid(&picked), None).await.expect("withdraw a draft");
    t.ins_submit(&hr, "LeaveAllocation", alloc(mid, &casual, "2026-01-01", "2026-12-31", "5", false)).await;
    let mut own = app(mid, &casual, "2026-11-10", "2026-11-10");
    own["leave_approver"] = json!("boss@hrleave.test");
    let own = t.ins(&boss, "LeaveApplication", own).await;
    refused(t.trans(&boss, "LeaveApplication", uid(&own), "Approve").await, "your own leave");
    refused(t.try_ins(&me, "LeaveApplication", app(eid, &casual, "2026-10-05", "2026-10-06")).await, "already applied");
    refused(t.try_ins(&me, "LeaveApplication", app(eid, &casual, "2026-10-03", "2026-10-04")).await, "nothing to apply for");
    // Half day.
    let mut half = app(eid, &casual, "2026-10-07", "2026-10-07");
    half["half_day"] = json!(true);
    let a2 = t.ins(&me, "LeaveApplication", half).await;
    assert_eq!(dec(&a2["total_leave_days"]), n("0.5"));
    assert_eq!(dec(&a2["leave_balance"]), n("8"), "10 less the 2 days pending");
    // Not enough sick leave (1 allocated, 2 asked).
    refused(t.try_ins(&me, "LeaveApplication", app(eid, &sick, "2026-10-12", "2026-10-13")).await, "insufficient leave balance");
    // Others cannot read it; the wrong approver cannot approve; the employee cannot approve.
    refused(t.k.get(&peer, "LeaveApplication", uid(&a1), false).await, "not found");
    assert!(t.list(&peer, "LeaveApplication", json!({})).await.is_empty());
    forbidden(t.trans(&other, "LeaveApplication", uid(&a1), "Approve").await);
    forbidden(t.trans(&me, "LeaveApplication", uid(&a1), "Approve").await);
    refused(t.k.submit(&hr, "LeaveApplication", uid(&a1), None).await, "workflow");

    let approved = t.trans(&boss, "LeaveApplication", uid(&a1), "Approve").await.expect("approve");
    assert_eq!(approved["workflow_state"], json!("Approved"));
    assert_eq!(t.get("LeaveApplication", uid(&a1)).await["status"], json!("Approved"));
    let att = t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "docstatus": 1})).await;
    let mut days: Vec<&str> = att.iter().map(|a| a["attendance_date"].as_str().unwrap()).collect();
    days.sort();
    assert_eq!(days, vec!["2026-10-01", "2026-10-05"], "leave marked on working days only");
    assert!(att.iter().all(|a| a["status"] == json!("On Leave")));
    assert_eq!(t.list(&me, "Attendance", json!({})).await.len(), 2, "the employee sees their attendance");
    t.trans(&boss, "LeaveApplication", uid(&a2), "Approve").await.expect("approve half day");
    let hd = t.one("Attendance", json!({"employee": eid.to_string(), "attendance_date": "2026-10-07"})).await;
    assert_eq!(hd["status"], json!("Half Day"));

    let bal = t.report(&hr, "leave_balance", &[("as_of", "2026-10-29"), ("employee", &eid.to_string())]).await.expect("leave balance");
    let cas = row(&bal, "leave_type", "Casual Leave");
    assert_eq!((dec(&cas["allocated"]), dec(&cas["taken"]), dec(&cas["balance"])), (n("10"), n("2.5"), n("7.5")));
    let own = t.report(&me, "leave_balance", &[("as_of", "2026-10-29")]).await.expect("own balance");
    assert_eq!(own["rows"].as_array().unwrap().len(), 2, "only the employee's own allocations");

    // Cancelling approved leave gives the days back and clears its attendance.
    forbidden(t.trans(&other, "LeaveApplication", uid(&a1), "Cancel").await);
    t.trans(&boss, "LeaveApplication", uid(&a1), "Cancel").await.expect("cancel leave");
    assert_eq!(t.get("LeaveApplication", uid(&a1)).await["status"], json!("Cancelled"));
    assert_eq!(t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "docstatus": 1})).await.len(), 1, "only the half day remains");
    let bal = t.report(&hr, "leave_balance", &[("as_of", "2026-10-29"), ("employee", &eid.to_string()), ("leave_type", &casual)]).await.unwrap();
    assert_eq!(dec(&bal["rows"][0]["balance"]), n("9.5"));
    let net: Decimal = t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": cas_alloc["id"]})).await.iter().map(|l| dec(&l["leaves"])).sum();
    assert_eq!(net, n("9.5"), "ledger: +10 −2 +2 −0.5");
    t.ins(&me, "LeaveApplication", app(eid, &casual, "2026-10-01", "2026-10-01")).await;

    // The ledger is written by the system only.
    forbidden(t.try_ins(&hr, "LeaveLedgerEntry", json!({"employee": eid.to_string(), "leave_type": casual})).await);
    refused(
        t.try_ins(
            &t.admin,
            "LeaveLedgerEntry",
            json!({"employee": eid.to_string(), "leave_type": casual, "transaction_type": "Leave Allocation",
            "transaction_id": "x", "leaves": "100", "from_date": "2026-01-01", "to_date": "2026-12-31"}),
        )
        .await,
        "system",
    );

    // Leave types that only apply after enough service.
    t.ins_submit(&hr, "LeaveAllocation", alloc(uid(&newbie), &privilege, "2026-01-01", "2026-12-31", "5", false)).await;
    refused(t.try_ins(&hr, "LeaveApplication", app(uid(&newbie), &privilege, "2026-10-19", "2026-10-19")).await, "after 90 working days");

    // Year-end: 2025 privilege leave partly used, expired, and carried into 2026.
    let p25 = t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &privilege, "2025-01-01", "2025-12-31", "18", false)).await;
    let dec_leave = t.ins(&hr, "LeaveApplication", app(eid, &privilege, "2025-12-01", "2025-12-03")).await;
    assert_eq!(dec(&dec_leave["total_leave_days"]), n("3"));
    t.trans(&boss, "LeaveApplication", uid(&dec_leave), "Approve").await.expect("approve december leave");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance");
    let expired: Decimal =
        t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": p25["id"], "transaction_type": "Expiry"})).await.iter().map(|l| dec(&l["leaves"])).sum();
    assert_eq!(expired, n("-15"), "what was left of 2025 expired");
    // An allocation that ended recently with leaves left: the scheduler expires them.
    let today = chrono::Utc::now().date_naive();
    let (start, end) = ((today - chrono::Duration::days(100)).to_string(), (today - chrono::Duration::days(10)).to_string());
    let ended = t.ins_submit(&hr, "LeaveAllocation", alloc(uid(&pia), &sick, &start, &end, "5", false)).await;
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance");
    let lapsed: Vec<Doc> = t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": ended["id"], "transaction_type": "Expiry"})).await;
    assert_eq!((lapsed.len(), lapsed.first().map(|l| dec(&l["leaves"]))), (1, Some(n("-5"))), "unused leave expires at the end");
    let p26 = t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &privilege, "2026-01-01", "2026-12-31", "18", true)).await;
    assert_eq!((dec(&p26["unused_leaves"]), dec(&p26["total_leaves_allocated"])), (n("15"), n("33")), "15 carried forward");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance is idempotent");
    let bal = t.report(&hr, "leave_balance", &[("as_of", "2026-10-29"), ("employee", &eid.to_string()), ("leave_type", &privilege)]).await.unwrap();
    assert_eq!((dec(&bal["rows"][0]["carried_forward"]), dec(&bal["rows"][0]["balance"])), (n("15"), n("33")));

    // A leave type that may go negative.
    let comp_off = t.ins(&hr, "LeaveType", json!({"leave_type_name": "Comp Off", "allow_negative": true})).await;
    let co_id = comp_off["id"].as_str().unwrap().to_string();
    t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &co_id, "2026-01-01", "2026-12-31", "1", false)).await;
    let neg = t.ins(&me, "LeaveApplication", app(eid, &co_id, "2026-10-26", "2026-10-27")).await;
    t.trans(&boss, "LeaveApplication", uid(&neg), "Approve").await.expect("overdraw comp off");

    // Leave without pay needs no allocation; HR managers approve when no approver is set.
    let unpaid = t.ins(&hr, "LeaveApplication", app(uid(&o), &lwp, "2026-09-21", "2026-09-22")).await;
    assert_eq!(unpaid["leave_approver"], Value::Null);
    forbidden(t.trans(&boss, "LeaveApplication", uid(&unpaid), "Approve").await);
    t.trans(&hr, "LeaveApplication", uid(&unpaid), "Approve").await.expect("HR approves");
    let lwp_entry = t.one("LeaveLedgerEntry", json!({"transaction_id": unpaid["id"]})).await;
    assert_eq!((lwp_entry["is_lwp"].clone(), dec(&lwp_entry["leaves"])), (json!(true), n("-2")));

    // Policies allocate in bulk, once.
    let policy = t
        .ins(&hr, "LeavePolicy", json!({"title": "Standard", "leave_policy_details": [{"leave_type": casual, "annual_allocation": "12"}, {"leave_type": sick, "annual_allocation": "6"}]}))
        .await;
    let params = json!({"from_date": "2027-01-01", "to_date": "2027-12-31", "employees": [eid.to_string(), mid.to_string()]});
    let first = t.act(&hr, "LeavePolicy", uid(&policy), "assign_allocations", params.clone()).await;
    assert_eq!((first["created"].clone(), first["skipped"].clone()), (json!(4), json!(0)));
    let again = t.act(&hr, "LeavePolicy", uid(&policy), "assign_allocations", params).await;
    assert_eq!((again["created"].clone(), again["skipped"].clone()), (json!(0), json!(4)));
}

// ───────────────────────────── attendance requests ─────────────────────────────

/// A missed check-in put right, and days worked from home: asked for by the employee, decided by
/// their approver, and written to attendance by the decision.
async fn attendance_request_scenario() {
    let t = T::new("hrareq").await;
    let hr = t.user("hr@hrareq.test", &["HR Manager"]).await;
    let boss = t.user("boss@hrareq.test", &["Employee", "Leave Approver"]).await;
    let other = t.user("other@hrareq.test", &["Employee", "Leave Approver"]).await;
    let me = t.user("me@hrareq.test", &["Employee"]).await;
    let m = t.employee("Maya", json!({"user": "boss@hrareq.test"})).await;
    let e = t.employee("Esha", json!({"user": "me@hrareq.test", "reports_to": m["id"]})).await;
    let (eid, mid) = (uid(&e), uid(&m));
    let ask = |reason: &str, from: &str, to: &str| json!({"employee": eid.to_string(), "reason": reason, "from_date": from, "to_date": to, "explanation": "Forgot to punch"});

    // Tuesday 6 October was marked absent for want of a check-in; she asks for it to be put right.
    t.ins_submit(&hr, "Attendance", json!({"employee": eid.to_string(), "attendance_date": "2026-10-06", "status": "Absent", "remarks": "No check-in"})).await;
    let mut fix = ask("Missed Check-in", "2026-10-06", "2026-10-06");
    fix["in_time"] = json!("09:05");
    fix["out_time"] = json!("18:10");
    let r1 = t.ins(&me, "AttendanceRequest", fix).await;
    assert_eq!(
        (r1["status"].clone(), r1["approver"].clone(), r1["employee_user"].clone()),
        (json!("Open"), json!("boss@hrareq.test"), json!("me@hrareq.test"))
    );
    let told = t.scalar_i64("select count(*) from notification where tenant_id = $1 and user_id = $2", boss.user).await;
    assert!(told >= 1, "the approver is told");
    // Employees cannot pick their approver, or ask for a colleague.
    let mut pick = ask("Work From Home", "2026-10-07", "2026-10-07");
    pick["approver"] = json!("other@hrareq.test");
    let picked = t.ins(&me, "AttendanceRequest", pick).await;
    assert_eq!(picked["approver"], json!("boss@hrareq.test"));
    t.k.delete(&me, "AttendanceRequest", uid(&picked), None).await.expect("withdraw a request");
    let mut colleague = ask("Work From Home", "2026-10-07", "2026-10-07");
    colleague["employee"] = json!(mid.to_string());
    forbidden(t.try_ins(&me, "AttendanceRequest", colleague).await);

    // What cannot be asked for.
    refused(t.try_ins(&me, "AttendanceRequest", ask("Missed Check-in", "2099-01-05", "2099-01-05")).await, "future");
    refused(t.try_ins(&me, "AttendanceRequest", ask("Work From Home", "2026-10-03", "2026-10-04")).await, "holiday or weekly off");
    refused(t.try_ins(&me, "AttendanceRequest", ask("Work From Home", "2026-08-01", "2026-09-30")).await, "at most 31 days");
    refused(t.try_ins(&me, "AttendanceRequest", ask("Missed Check-in", "2026-10-06", "2026-10-06")).await, "already asked");
    let mut timed = ask("Work From Home", "2026-10-07", "2026-10-08");
    timed["in_time"] = json!("09:00");
    refused(t.try_ins(&me, "AttendanceRequest", timed).await, "single day");
    let mut backwards = ask("Missed Check-in", "2026-10-07", "2026-10-07");
    backwards["in_time"] = json!("18:00");
    backwards["out_time"] = json!("09:00");
    refused(t.try_ins(&me, "AttendanceRequest", backwards).await, "after the time came in");
    let mut sloppy = ask("Missed Check-in", "2026-10-07", "2026-10-07");
    sloppy["in_time"] = json!("9am");
    refused(t.try_ins(&me, "AttendanceRequest", sloppy).await, "09:00");
    let casual = t.one("LeaveType", json!({"leave_type_name": "Casual Leave"})).await["id"].as_str().unwrap().to_string();
    t.ins_submit(
        &hr,
        "LeaveAllocation",
        json!({"employee": eid.to_string(), "leave_type": casual, "from_date": "2026-01-01", "to_date": "2026-12-31", "new_leaves_allocated": "5"}),
    )
    .await;
    let leave =
        t.ins(&me, "LeaveApplication", json!({"employee": eid.to_string(), "leave_type": casual, "from_date": "2026-10-08", "to_date": "2026-10-08"})).await;
    t.trans(&boss, "LeaveApplication", uid(&leave), "Approve").await.expect("approve leave");
    refused(t.try_ins(&me, "AttendanceRequest", ask("Work From Home", "2026-10-08", "2026-10-09")).await, "is on leave");

    // Only the approver (or an HR manager) decides.
    forbidden(t.trans(&me, "AttendanceRequest", uid(&r1), "Approve").await);
    forbidden(t.trans(&other, "AttendanceRequest", uid(&r1), "Approve").await);
    let done = t.trans(&boss, "AttendanceRequest", uid(&r1), "Approve").await.expect("approve");
    assert_eq!(done["workflow_state"], json!("Approved"));
    assert_eq!(t.get("AttendanceRequest", uid(&r1)).await["status"], json!("Approved"));
    let day = t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "attendance_date": "2026-10-06"})).await;
    let live: Vec<&Doc> = day.iter().filter(|a| a["docstatus"].as_i64() == Some(1)).collect();
    assert_eq!(live.len(), 1, "one live mark for the day: {day:?}");
    assert_eq!((live[0]["status"].clone(), live[0]["attendance_request"].clone()), (json!("Present"), r1["id"].clone()));
    assert_eq!(dec(&live[0]["working_hours"]), n("9.08"), "09:05 to 18:10");
    assert!(!live[0]["in_time"].is_null() && !live[0]["out_time"].is_null());
    assert!(day.iter().any(|a| a["docstatus"].as_i64() == Some(2) && a["status"] == json!("Absent")), "the absent mark was cancelled, not kept beside it");
    assert!(t.scalar_i64("select count(*) from notification where tenant_id = $1 and user_id = $2", me.user).await >= 1, "the employee is told");

    // A week at home: the weekend is off.
    let wfh = t.ins(&me, "AttendanceRequest", ask("Work From Home", "2026-10-12", "2026-10-18")).await;
    t.trans(&boss, "AttendanceRequest", uid(&wfh), "Approve").await.expect("approve a week at home");
    let week = t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "attendance_request": wfh["id"], "docstatus": 1})).await;
    let mut dates: Vec<&str> = week.iter().map(|a| a["attendance_date"].as_str().unwrap()).collect();
    dates.sort();
    assert_eq!(dates, ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16"]);
    assert!(week.iter().all(|a| a["status"] == json!("Work From Home")));
    // Cancelling it takes the marks off.
    t.trans(&boss, "AttendanceRequest", uid(&wfh), "Cancel").await.expect("cancel");
    assert_eq!(t.get("AttendanceRequest", uid(&wfh)).await["status"], json!("Cancelled"));
    assert!(t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "attendance_request": wfh["id"], "docstatus": 1})).await.is_empty());

    // A half day; a refusal marks nothing and does not stand in the way of asking again.
    let mut half = ask("Missed Check-in", "2026-10-21", "2026-10-21");
    half["half_day"] = json!(true);
    let half = t.ins(&me, "AttendanceRequest", half).await;
    t.trans(&boss, "AttendanceRequest", uid(&half), "Approve").await.expect("approve half day");
    assert_eq!(t.one("Attendance", json!({"employee": eid.to_string(), "attendance_date": "2026-10-21", "docstatus": 1})).await["status"], json!("Half Day"));
    let no = t.ins(&me, "AttendanceRequest", ask("On Duty", "2026-10-20", "2026-10-20")).await;
    t.trans(&boss, "AttendanceRequest", uid(&no), "Reject").await.expect("reject");
    assert_eq!(t.get("AttendanceRequest", uid(&no)).await["status"], json!("Rejected"));
    assert!(t.list(&t.admin, "Attendance", json!({"employee": eid.to_string(), "attendance_date": "2026-10-20"})).await.is_empty());
    t.ins(&me, "AttendanceRequest", ask("On Duty", "2026-10-20", "2026-10-20")).await;
}

// ───────────────────────────── leave accrual ─────────────────────────────

/// A policy line that accrues grants a share at the start of each month: opening the year's
/// allocation, then topping it up; counting what was granted so that running again changes
/// nothing; leaving alone what was allocated by hand; and running by itself in the upkeep.
async fn accrual_scenario() {
    let t = T::new("hraccrue").await;
    let hr = t.user("hr@hraccrue.test", &["HR Manager"]).await;
    let lt = |name: &str| {
        let t = &t;
        let name = name.to_string();
        async move { t.one("LeaveType", json!({"leave_type_name": name})).await["id"].as_str().unwrap().to_string() }
    };
    let (casual, sick) = (lt("Casual Leave").await, lt("Sick Leave").await);
    let policy = t
        .ins(
            &hr,
            "LeavePolicy",
            json!({"title": "Monthly", "year_starts": "January",
            "leave_policy_details": [{"leave_type": casual, "annual_allocation": "12", "accrual": "Monthly"}, {"leave_type": sick, "annual_allocation": "6"}]}),
        )
        .await;
    let on_policy = |first: &str, joined: &str| json!({"first_name": first, "date_of_joining": joined, "leave_policy": policy["id"]});
    let ann = t.employee("Ann", on_policy("Ann", "2024-06-01")).await;
    let ben = t.employee("Ben", on_policy("Ben", "2025-04-15")).await;
    let cy = t.employee("Cy", on_policy("Cy", "2024-06-01")).await;
    t.employee("Dee", json!({})).await;
    t.ins_submit(
        &hr,
        "LeaveAllocation",
        json!({"employee": cy["id"], "leave_type": casual, "from_date": "2025-01-01", "to_date": "2025-12-31", "new_leaves_allocated": "12"}),
    )
    .await;

    // By 15 March three shares have begun. Ann has none yet, Ben has not joined, and Cy's leave was
    // allocated by hand.
    let first = t.act(&hr, "LeavePolicy", uid(&policy), "accrue", json!({"on": "2025-03-15"})).await;
    assert_eq!((first["granted"].clone(), first["skipped"].clone()), (json!(1), json!(1)), "{first}");
    let of = |emp: &Doc| {
        let (t, emp, casual) = (&t, emp["id"].clone(), casual.clone());
        async move { t.list(&t.admin, "LeaveAllocation", json!({"employee": emp, "leave_type": casual, "from_date": "2025-01-01"})).await }
    };
    let anns = of(&ann).await;
    assert_eq!(anns.len(), 1);
    let ann_alloc = &anns[0];
    assert_eq!(
        (ann_alloc["to_date"].clone(), ann_alloc["leave_policy"].clone(), ann_alloc["docstatus"].as_i64()),
        (json!("2025-12-31"), policy["id"].clone(), Some(1))
    );
    assert_eq!(dec(&ann_alloc["total_leaves_allocated"]), n("3"));
    assert!(of(&ben).await.is_empty(), "not yet joined");
    let cys = of(&cy).await;
    assert_eq!((cys.len(), dec(&cys[0]["total_leaves_allocated"])), (1, n("12")), "allocated by hand, and left as it was");
    let again = t.act(&hr, "LeavePolicy", uid(&policy), "accrue", json!({"on": "2025-03-15"})).await;
    assert_eq!(again["granted"], json!(0), "what was granted is counted first");

    // Two months on, Ann is topped up (the allocation and the ledger agree) and Ben, who joined in
    // the middle of April, has his first share, for May.
    let may = t.act(&hr, "LeavePolicy", uid(&policy), "accrue", json!({"on": "2025-05-01"})).await;
    assert_eq!(may["granted"], json!(2), "{may}");
    let ann_alloc = t.get("LeaveAllocation", uid(ann_alloc)).await;
    assert_eq!((dec(&ann_alloc["new_leaves_allocated"]), dec(&ann_alloc["total_leaves_allocated"])), (n("5"), n("5")));
    let ledger: Decimal = t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": ann_alloc["id"]})).await.iter().map(|l| dec(&l["leaves"])).sum();
    assert_eq!(ledger, n("5"));
    assert_eq!(dec(&of(&ben).await[0]["total_leaves_allocated"]), n("1"));
    // What has accrued is what can be asked for.
    let leave = |from: &str, to: &str| json!({"employee": ann["id"], "leave_type": casual, "from_date": from, "to_date": to});
    t.ins(&hr, "LeaveApplication", leave("2025-05-05", "2025-05-07")).await;
    refused(t.try_ins(&hr, "LeaveApplication", leave("2025-05-12", "2025-05-14")).await, "insufficient leave balance");

    // By the year's end it adds up to the year's amount, and not more than one allocation may hold.
    t.act(&hr, "LeavePolicy", uid(&policy), "accrue", json!({"on": "2025-12-31"})).await;
    assert_eq!(dec(&t.get("LeaveAllocation", uid(&ann_alloc)).await["total_leaves_allocated"]), n("12"));
    assert_eq!(dec(&of(&ben).await[0]["total_leaves_allocated"]), n("8"), "May to December");
    // The sick leave line does not accrue: it waits for Allocate Leaves.
    assert!(t.list(&t.admin, "LeaveAllocation", json!({"employee": ann["id"], "leave_type": sick})).await.is_empty());

    // The upkeep does the same by itself, for the year it is in.
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance");
    let today: String = {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query_scalar("select to_char(now() at time zone 'Asia/Kolkata', 'YYYY-MM-DD')").fetch_one(&mut *tx).await.unwrap()
    };
    let (year, month) = (&today[..4], today[5..7].parse::<i64>().unwrap());
    let now = t.list(&t.admin, "LeaveAllocation", json!({"employee": ann["id"], "leave_type": casual, "from_date": format!("{year}-01-01")})).await;
    assert_eq!(now.len(), 1, "this year's allocation was opened");
    assert_eq!(
        (dec(&now[0]["total_leaves_allocated"]), now[0]["leave_policy"].clone()),
        (Decimal::from(month), policy["id"].clone()),
        "a share for each month so far"
    );
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance again");
    assert_eq!(t.list(&t.admin, "LeaveAllocation", json!({"employee": ann["id"], "leave_type": casual, "from_date": format!("{year}-01-01")})).await.len(), 1);
}

// ───────────────────────────── the screens' endpoints ─────────────────────────────

/// A call to HR's endpoints as `who` (`/api/app/hr/{path}`), or as the public (`/api/site/{workspace}/hr/{path}`).
async fn call(t: &T, who: Option<&Ctx>, method: &str, path: &str, query: &[(&str, &str)], body: Value) -> ferrum_kernel::Result<Value> {
    let site = t.k.hooks.site("hr").expect("HR's endpoints").clone();
    let req = ferrum_kernel::SiteRequest {
        tenant: t.tenant,
        user: who.cloned(),
        method: method.into(),
        path: format!("/{path}"),
        query: query.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(),
        headers: Default::default(),
        body: if body.is_null() { vec![] } else { body.to_string().into_bytes() },
        client_ip: "203.0.113.7".into(),
    };
    let r = site.handle(&t.k, req).await?;
    Ok(serde_json::from_slice(&r.body).unwrap())
}

/// What a manager's approvals page and a team's calendar are drawn from: the requests routed to
/// the manager, with what a decision needs beside each; who is away, and when; the directory, with
/// what each person may see; and the careers page anyone may open.
async fn site_scenario() {
    let t = T::new("hrsite").await;
    let hr = t.user("hr@hrsite.test", &["HR Manager"]).await;
    let boss = t.user("boss@hrsite.test", &["Employee", "Leave Approver", "Expense Approver"]).await;
    let me = t.user("me@hrsite.test", &["Employee"]).await;
    let omar_user = t.user("omar@hrsite.test", &["Employee"]).await;
    let support = t.ins(&hr, "Department", json!({"department_name": "Support", "company": t.c()})).await;
    let team = json!({"department": support["id"], "company": t.c()});
    let with = |extra: Value| {
        let mut v = obj(team.clone());
        v.extend(obj(extra));
        Value::Object(v)
    };
    let m = t.employee("Maya", with(json!({"user": "boss@hrsite.test"}))).await;
    let e = t.employee("Esha", with(json!({"user": "me@hrsite.test", "reports_to": m["id"]}))).await;
    let o = t
        .employee("Omar", with(json!({"user": "omar@hrsite.test", "reports_to": m["id"], "cell_number": "+919845012345", "company_email": "omar@work.test"})))
        .await;
    t.employee("Neel", with(json!({"reports_to": m["id"]}))).await;
    t.employee("Zed", json!({})).await;
    let casual = t.one("LeaveType", json!({"leave_type_name": "Casual Leave"})).await["id"].as_str().unwrap().to_string();
    for emp in [&e, &o] {
        t.ins_submit(
            &hr,
            "LeaveAllocation",
            json!({"employee": emp["id"], "leave_type": casual, "from_date": "2026-01-01", "to_date": "2026-12-31", "new_leaves_allocated": "10"}),
        )
        .await;
    }
    let leave =
        |emp: &Doc, from: &str, to: &str| json!({"employee": emp["id"], "leave_type": casual, "from_date": from, "to_date": to, "reason": "Family wedding"});
    // Omar will be off on the 13th; Esha asks for the 12th to the 14th, and for a day at home.
    let omars = t.ins(&omar_user, "LeaveApplication", leave(&o, "2026-10-13", "2026-10-13")).await;
    t.trans(&boss, "LeaveApplication", uid(&omars), "Approve").await.expect("approve Omar's leave");
    let eshas = t.ins(&me, "LeaveApplication", leave(&e, "2026-10-12", "2026-10-14")).await;
    t.ins(&hr, "EmployeeCheckin", json!({"employee": e["id"], "log_type": "IN", "log_time": "2026-10-15T09:40:00+05:30"})).await;
    let wfh = t
        .ins(&me, "AttendanceRequest", json!({"employee": e["id"], "reason": "Missed Check-in", "from_date": "2026-10-15", "to_date": "2026-10-15", "explanation": "Left without punching out"}))
        .await;
    let travel = t.one("ExpenseClaimType", json!({"expense_type": "Travel"})).await;
    let claim = t
        .ins(&me, "ExpenseClaim", json!({"employee": e["id"], "posting_date": "2026-10-16", "expenses": [{"expense_date": "2026-10-15", "expense_type": travel["id"], "amount": "1200", "description": "Cab to the client"}]}))
        .await;

    // The manager's page: all three, oldest first, with what a decision needs.
    let inbox = call(&t, Some(&boss), "GET", "inbox", &[], Value::Null).await.expect("inbox");
    let items = inbox["items"].as_array().unwrap();
    let kinds: Vec<&str> = items.iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds.len(), 3, "{inbox}");
    for kind in ["leave", "attendance", "expense"] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    let waiting: Vec<&str> = items.iter().map(|i| i["waiting_since"].as_str().unwrap()).collect();
    assert!(waiting.windows(2).all(|w| w[0] <= w[1]), "oldest first: {waiting:?}");
    let pick = |kind: &str| items.iter().find(|i| i["kind"] == json!(kind)).unwrap();
    let l = pick("leave");
    assert_eq!((l["id"].clone(), l["employee"]["name"].clone(), l["reason"].clone()), (eshas["id"].clone(), json!("Esha Test"), json!("Family wedding")));
    assert_eq!((l["summary"]["leave_type"].clone(), dec(&l["summary"]["days"])), (json!("Casual Leave"), n("3")));
    assert_eq!((dec(&l["context"]["balance_before"]), dec(&l["context"]["balance_after"])), (n("10"), n("7")), "what is left once it is granted");
    let team_away = &l["context"]["team"];
    assert_eq!((team_away["label"].clone(), team_away["size"].clone(), team_away["away_count"].clone()), (json!("Support"), json!(4), json!(1)), "{team_away}");
    assert_eq!(team_away["away"][0]["name"], json!("Omar Test"));
    let a = pick("attendance");
    assert_eq!((a["id"].clone(), a["summary"]["reason"].clone()), (wfh["id"].clone(), json!("Missed Check-in")));
    assert_eq!(a["context"]["punches"][0]["at"], json!("2026-10-15T09:40"), "the punch, on the clock the tenant keeps");
    let x = pick("expense");
    assert_eq!((x["id"].clone(), x["summary"]["lines"][0]["type"].clone(), dec(&x["summary"]["claimed"])), (claim["id"].clone(), json!("Travel"), n("1200")));
    // Each is the approver's alone to see; nothing is waiting for the one who asked, and the page is for signed-in people.
    assert!(call(&t, Some(&me), "GET", "inbox", &[], Value::Null).await.unwrap()["items"].as_array().unwrap().is_empty());
    assert!(matches!(call(&t, None, "GET", "inbox", &[], Value::Null).await, Err(Error::Unauthorized(_))));
    // Home's My work lists them without the detail.
    let work = call(&t, Some(&boss), "GET", "work", &[], Value::Null).await.expect("work");
    assert_eq!(work["items"].as_array().unwrap().len(), 3);
    assert_eq!(work["items"][0]["url"], json!("/people/approvals"));
    // A decision, on the version the page showed, takes the request off the page; on an older one it is refused.
    assert!(items.iter().all(|i| i["version"].as_i64().is_some()), "every card carries the version it was read at");
    refused(t.k.transition(&boss, "LeaveApplication", uid(&eshas), "Approve", l["version"].as_i64().map(|v| v - 1)).await, "changed");
    t.k.transition(&boss, "LeaveApplication", uid(&eshas), "Approve", l["version"].as_i64()).await.expect("approve");
    let after = call(&t, Some(&boss), "GET", "inbox", &[], Value::Null).await.unwrap();
    assert_eq!(after["items"].as_array().unwrap().len(), 2);

    // The team calendar: Esha sees her team's approved leave (not the type), the holiday and the weekly offs.
    let month = [("from", "2026-10-01"), ("to", "2026-10-31")];
    let cal = call(&t, Some(&me), "GET", "calendar", &month, Value::Null).await.expect("calendar");
    assert_eq!((cal["scope"].clone(), cal["everyone_allowed"].clone(), cal["size"].clone()), (json!("team"), json!(false), json!(4)), "{cal}");
    let away: Vec<(&str, &str)> = cal["away"].as_array().unwrap().iter().map(|a| (a["name"].as_str().unwrap(), a["from_date"].as_str().unwrap())).collect();
    assert_eq!(away, vec![("Esha Test", "2026-10-12"), ("Omar Test", "2026-10-13")]);
    assert!(cal["away"][0]["leave_type"].is_null(), "colleagues' leave types are theirs");
    let holidays = cal["holidays"].as_array().unwrap();
    assert!(holidays.iter().any(|h| h["date"] == json!("2026-10-02") && h["name"] == json!("Gandhi Jayanti") && h["weekly_off"] == json!(false)));
    assert!(holidays.iter().any(|h| h["date"] == json!("2026-10-03") && h["weekly_off"] == json!(true)));
    let all = call(&t, Some(&me), "GET", "calendar", &[("from", "2026-10-01"), ("to", "2026-10-31"), ("scope", "all")], Value::Null).await.unwrap();
    assert_eq!(all["scope"], json!("team"), "everyone is for those who may see everyone");
    let hr_cal = call(&t, Some(&hr), "GET", "calendar", &[("from", "2026-10-01"), ("to", "2026-10-31"), ("scope", "all")], Value::Null).await.unwrap();
    assert_eq!((hr_cal["scope"].clone(), hr_cal["size"].clone(), hr_cal["away"].as_array().unwrap().len()), (json!("all"), json!(5), 2));
    assert_eq!(hr_cal["away"][0]["leave_type"], json!("Casual Leave"));
    assert!(
        call(&t, Some(&me), "GET", "calendar", &[("from", "2026-01-01"), ("to", "2026-12-31")], Value::Null).await.is_err(),
        "a year at a time is too much"
    );

    // The directory: everyone's name, role and work email; phone numbers only for those who see every employee.
    let dir = call(&t, Some(&me), "GET", "directory", &[], Value::Null).await.expect("directory");
    assert_eq!((dir["people"].as_array().unwrap().len(), dir["detail"].clone()), (5, json!(false)));
    let omar = dir["people"].as_array().unwrap().iter().find(|p| p["name"] == json!("Omar Test")).unwrap();
    assert_eq!((omar["manager"].clone(), omar["department"].clone(), omar["email"].clone()), (m["id"].clone(), json!("Support"), json!("omar@work.test")));
    assert!(omar["phone"].is_null());
    let full = call(&t, Some(&hr), "GET", "directory", &[], Value::Null).await.unwrap();
    assert_eq!(full["people"].as_array().unwrap().iter().find(|p| p["name"] == json!("Omar Test")).unwrap()["phone"], json!("+919845012345"));

    // Careers: only what is published and open; an application from anyone, once.
    let engineer = t.ins(&hr, "Designation", json!({"designation_name": "Engineer"})).await;
    let post = |title: &str, extra: Value| {
        let mut v = obj(
            json!({"job_title": title, "designation": engineer["id"], "company": t.c(), "department": support["id"], "description": "Help **people** with their printers."}),
        );
        v.extend(obj(extra));
        t.ins(&hr, "JobOpening", Value::Object(v))
    };
    let open = post("Support Engineer", json!({"publish": true})).await;
    post("Quiet search", json!({"publish": false})).await;
    post("Last week's", json!({"publish": true, "closes_on": "2026-01-31"})).await;
    let list = call(&t, None, "GET", "careers", &[], Value::Null).await.expect("careers");
    let openings = list["openings"].as_array().unwrap();
    assert_eq!(openings.len(), 1, "{list}");
    assert_eq!(
        (openings[0]["title"].clone(), openings[0]["department"].clone(), openings[0]["company"].clone()),
        (json!("Support Engineer"), json!("Support"), json!("hrsite Pvt Ltd"))
    );
    assert!(openings[0]["description"].is_null(), "the list leaves the description to the opening");
    let one = call(&t, None, "GET", &format!("careers/{}", open["id"].as_str().unwrap()), &[], Value::Null).await.expect("one opening");
    assert_eq!(one["description"], json!("Help **people** with their printers."));
    let hidden = t.one("JobOpening", json!({"job_title": "Quiet search"})).await;
    assert!(matches!(call(&t, None, "GET", &format!("careers/{}", hidden["id"].as_str().unwrap()), &[], Value::Null).await, Err(Error::NotFound(_))));
    let apply = format!("careers/{}/apply", open["id"].as_str().unwrap());
    let form = json!({"name": "  Asha Rao ", "email": "Asha.Rao@Example.com", "phone": "+91 98450 12345", "cover_letter": "I like printers.", "resume_link": "https://example.com/asha.pdf"});
    assert_eq!(call(&t, None, "POST", &apply, &[], form.clone()).await.expect("apply")["received"], json!(true));
    let applicant = t.one("JobApplicant", json!({"job_opening": open["id"]})).await;
    assert_eq!(
        (applicant["applicant_name"].clone(), applicant["email_id"].clone(), applicant["source"].clone(), applicant["status"].clone()),
        (json!("Asha Rao"), json!("asha.rao@example.com"), json!("Website Listing"), json!("Open"))
    );
    // Once for each opening, in whatever case; the second time is thanked and says nothing about the first.
    assert_eq!(call(&t, None, "POST", &apply, &[], form.clone()).await.expect("again")["received"], json!(true));
    let mut shouty = form.clone();
    shouty["email"] = json!("ASHA.RAO@EXAMPLE.COM");
    assert_eq!(call(&t, None, "POST", &apply, &[], shouty).await.expect("shouting")["received"], json!(true));
    assert_eq!(t.list(&t.admin, "JobApplicant", json!({"job_opening": open["id"]})).await.len(), 1);
    let mut phone = form.clone();
    phone["email"] = json!("pia@example.com");
    phone["phone"] = json!("call me");
    refused(call(&t, None, "POST", &apply, &[], phone).await, "phone: ");
    let mut bad = form.clone();
    bad["email"] = json!("not an address");
    refused(call(&t, None, "POST", &apply, &[], bad).await, "email");
    let mut link = form.clone();
    link["email"] = json!("ravi@example.com");
    link["resume_link"] = json!("javascript:alert(1)");
    refused(call(&t, None, "POST", &apply, &[], link).await, "https");
    let mut bot = form.clone();
    bot["email"] = json!("bot@example.com");
    bot["website"] = json!("http://spam.test");
    call(&t, None, "POST", &apply, &[], bot).await.expect("a program is thanked");
    assert_eq!(t.list(&t.admin, "JobApplicant", json!({"job_opening": open["id"]})).await.len(), 1, "and kept out");
    let hidden_apply = format!("careers/{}/apply", hidden["id"].as_str().unwrap());
    assert!(matches!(call(&t, None, "POST", &hidden_apply, &[], form).await, Err(Error::NotFound(_))), "nobody applies to what is not published");
}

// ───────────────────────────── attendance ─────────────────────────────

async fn attendance_scenario() {
    let t = T::new("hratt").await;
    let hr = t.user("hr@hratt.test", &["HR Manager"]).await;
    let device = t.user("device@hratt.test", &["Attendance Device"]).await;
    let alice = t.user("alice@hratt.test", &["Employee"]).await;
    let sync = "2026-09-07T23:00:00+05:30";
    let general = t
        .ins(
            &hr,
            "ShiftType",
            json!({"shift_name": "General", "start_time": "09:00", "end_time": "18:00", "late_entry_grace_period": 15,
            "early_exit_grace_period": 10, "working_hours_threshold_for_half_day": "6", "working_hours_threshold_for_absent": "3",
            "enable_auto_attendance": true, "process_attendance_after": "2026-09-01", "last_sync_of_checkin": sync}),
        )
        .await;
    assert_eq!(general["is_overnight"], json!(false));
    let night = t
        .ins(
            &hr,
            "ShiftType",
            json!({"shift_name": "Night", "start_time": "22:00", "end_time": "06:00", "enable_auto_attendance": true,
            "process_attendance_after": "2026-09-01", "last_sync_of_checkin": sync}),
        )
        .await;
    assert_eq!(night["is_overnight"], json!(true));
    refused(t.try_ins(&hr, "ShiftType", json!({"shift_name": "Bad", "start_time": "9am", "end_time": "18:00"})).await, "09:00");
    let a = t.employee("Alice", json!({"default_shift": general["id"], "user": "alice@hratt.test", "attendance_device_id": "A-01"})).await;
    let b = t.employee("Bob", json!({"default_shift": general["id"]})).await;
    let c = t.employee("Chen", json!({})).await;
    let d = t.employee("Dev", json!({"default_shift": general["id"]})).await;
    let assign = json!({"employee": c["id"], "shift_type": night["id"], "start_date": "2026-09-01", "end_date": "2026-09-30"});
    t.ins_submit(&hr, "ShiftAssignment", assign.clone()).await;
    let mut overlapping = assign.clone();
    overlapping["start_date"] = json!("2026-09-15");
    refused(t.try_ins(&hr, "ShiftAssignment", overlapping).await, "overlaps shift assignment");

    // Punches (devices know Alice by her device id).
    t.ins(&device, "EmployeeCheckin", json!({"employee_device_id": "A-01", "log_type": "IN", "log_time": "2026-09-01T09:05:00+05:30", "device_id": "Gate 1"}))
        .await;
    refused(t.try_ins(&device, "EmployeeCheckin", json!({"employee_device_id": "ZZ", "log_time": "2026-09-01T09:05:00+05:30"})).await, "no employee");
    let punch = |emp: &Doc, ty: &str, at: &str| json!({"employee": emp["id"], "log_type": ty, "log_time": at});
    let a_out = t.ins(&alice, "EmployeeCheckin", punch(&a, "OUT", "2026-09-01T18:02:00+05:30")).await;
    forbidden(t.try_ins(&alice, "EmployeeCheckin", punch(&b, "IN", "2026-09-01T09:00:00+05:30")).await);
    for p in [
        punch(&a, "IN", "2026-09-02T09:00:00+05:30"),
        punch(&a, "OUT", "2026-09-02T14:00:00+05:30"),
        punch(&b, "IN", "2026-09-01T09:20:00+05:30"),
        punch(&b, "OUT", "2026-09-01T17:40:00+05:30"),
        punch(&b, "IN", "2026-09-02T09:00:00+05:30"),
        punch(&b, "OUT", "2026-09-02T11:00:00+05:30"),
        punch(&c, "IN", "2026-09-01T22:05:00+05:30"),
        punch(&c, "OUT", "2026-09-02T06:00:00+05:30"),
    ] {
        t.ins(&hr, "EmployeeCheckin", p).await;
    }
    refused(t.try_ins(&hr, "EmployeeCheckin", punch(&b, "IN", "2026-09-02T09:00:00+05:30")).await, "already has this Employee and Log Time");

    let res = t.act(&hr, "ShiftType", uid(&general), "mark_attendance", json!({"up_to": "2026-09-04"})).await;
    assert_eq!((res["marked"].clone(), res["absent"].clone()), (json!(3), json!(9)), "{res}");
    let att = |emp: &Doc, day: &str| {
        let t = &t;
        let (emp, day) = (emp["id"].as_str().unwrap().to_string(), day.to_string());
        async move { t.one("Attendance", json!({"employee": emp, "attendance_date": day})).await }
    };
    let a1 = att(&a, "2026-09-01").await;
    assert_eq!(
        (a1["status"].clone(), dec(&a1["working_hours"]), a1["late_entry"].clone(), a1["early_exit"].clone()),
        (json!("Present"), n("8.95"), json!(false), json!(false))
    );
    assert_eq!(a1["in_time"], json!("2026-09-01T03:35:00.000Z"), "stored in UTC");
    let b1 = att(&b, "2026-09-01").await;
    assert_eq!(
        (b1["status"].clone(), dec(&b1["working_hours"]), b1["late_entry"].clone(), b1["early_exit"].clone()),
        (json!("Present"), n("8.33"), json!(true), json!(true))
    );
    assert_eq!(att(&a, "2026-09-02").await["status"], json!("Half Day"));
    assert_eq!(att(&b, "2026-09-02").await["status"], json!("Absent"));
    assert_eq!(att(&d, "2026-09-03").await["status"], json!("Absent"), "no punches on a working day");
    assert_eq!(t.get("EmployeeCheckin", uid(&a_out)).await["attendance"], a1["id"], "punches link to their attendance");

    let res = t.act(&hr, "ShiftType", uid(&night), "mark_attendance", json!({"up_to": "2026-09-04"})).await;
    assert_eq!((res["marked"].clone(), res["absent"].clone()), (json!(1), json!(3)), "{res}");
    let c1 = att(&c, "2026-09-01").await;
    assert_eq!(
        (c1["status"].clone(), dec(&c1["working_hours"]), c1["late_entry"].clone()),
        (json!("Present"), n("7.92"), json!(true)),
        "overnight shift, in after 22:00"
    );

    // The scheduler continues up to the last device sync, skipping the weekend.
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance");
    assert_eq!(att(&d, "2026-09-07").await["status"], json!("Absent"));
    assert!(t.list(&t.admin, "Attendance", json!({"employee": d["id"], "attendance_date": "2026-09-05"})).await.is_empty(), "weekly off");
    assert_eq!(t.get("ShiftType", uid(&general)).await["last_processed_date"], json!("2026-09-07"));
    assert_eq!(t.get("ShiftType", uid(&night)).await["last_processed_date"], json!("2026-09-06"), "the night of the 7th ends after the last sync");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("maintenance again");
    assert_eq!(t.list(&t.admin, "Attendance", json!({"employee": d["id"]})).await.len(), 5, "idempotent: 1-4 and 7 September");

    // Refusals: duplicates, the future, editing a used punch.
    refused(t.try_ins(&hr, "Attendance", json!({"employee": a["id"], "attendance_date": "2026-09-01", "status": "Present"})).await, "already exists");
    refused(t.try_ins(&hr, "Attendance", json!({"employee": a["id"], "attendance_date": "2027-01-04", "status": "Present"})).await, "future");
    refused(t.k.delete(&hr, "EmployeeCheckin", uid(&a_out), None).await, "cancel that first");
    assert_eq!(t.list(&alice, "Attendance", json!({})).await.len(), 5, "Alice sees only her own attendance");
    assert_eq!(t.list(&alice, "EmployeeCheckin", json!({})).await.len(), 4, "and her own punches");

    // Cancelling attendance frees its punches for the next run.
    t.k.cancel(&hr, "Attendance", uid(&a1), None).await.expect("cancel attendance");
    assert_eq!(t.get("EmployeeCheckin", uid(&a_out)).await["attendance"], Value::Null);

    let sheet = t.report(&hr, "monthly_attendance_sheet", &[("month", "2026-09-15")]).await.expect("sheet");
    let bob = row(&sheet, "employee_name", "Bob Test");
    assert_eq!((bob["d1"].clone(), bob["d2"].clone(), bob["d5"].clone()), (json!("P"), json!("A"), json!("H")));
    assert_eq!((dec(&bob["present"]), dec(&bob["absent"])), (n("1"), n("4")));
    let own = t.report(&alice, "monthly_attendance_sheet", &[("month", "2026-09-15")]).await.expect("own sheet");
    assert_eq!(own["rows"].as_array().unwrap().len(), 1);
}

// ───────────────────────────── expenses ─────────────────────────────

async fn expense_scenario() {
    let t = T::new("hrexp").await;
    let hr = t.user("hr@hrexp.test", &["HR Manager"]).await;
    let boss = t.user("boss@hrexp.test", &["Employee", "Expense Approver"]).await;
    let other = t.user("other@hrexp.test", &["Expense Approver"]).await;
    let me = t.user("me@hrexp.test", &["Employee"]).await;
    let acct = t.user("acct@hrexp.test", &["Accounts Manager"]).await;
    let m = t.employee("Mira", json!({"user": "boss@hrexp.test"})).await;
    let e = t.employee("Eli", json!({"user": "me@hrexp.test", "reports_to": m["id"]})).await;
    let company = t.get("Company", t.company).await;
    assert!(company["hr_employee_payable_account"].is_string() && company["pay_payroll_payable_account"].is_string(), "HR accounts set up");

    // Travel posts to its own account; food falls back to the company's default expense account.
    let travel_acc = t
        .ins(
            &t.admin,
            "Account",
            json!({"account_name": "Travel Expenses", "company": t.c(), "root_type": "Expense", "parent_account": t.account("Indirect Expenses").await}),
        )
        .await;
    let travel = t.one("ExpenseClaimType", json!({"expense_type": "Travel"})).await;
    t.k.update(&t.admin, "ExpenseClaimType", uid(&travel), obj(json!({"accounts": [{"company": t.c(), "default_account": travel_acc["id"]}]})), None)
        .await
        .unwrap();
    let food = t.one("ExpenseClaimType", json!({"expense_type": "Food"})).await;

    // An advance: requested by the employee, submitted by HR, paid by accounts.
    let adv = t.ins(&me, "EmployeeAdvance", json!({"employee": e["id"], "purpose": "Client visit", "advance_amount": "5000"})).await;
    let adv = t.submit(&hr, "EmployeeAdvance", uid(&adv)).await;
    assert_eq!(adv["status"], json!("Unpaid"));
    assert!(!adv["paid_amount"].is_null() && dec(&adv["paid_amount"]).is_zero(), "nothing paid reads 0, not empty, so the page offers the payment");
    let pay = t.act(&hr, "EmployeeAdvance", uid(&adv), "make_payment_entry", json!({})).await;
    assert_eq!(pay["docstatus"], json!(0), "a draft for accounts");
    assert_eq!(pay["message"], json!("A draft journal entry is made for accounts to post (ERP, Journal Entries)"));
    refused(t.k.submit(&hr, "JournalEntry", vid(&pay), None).await, "not permitted");
    t.submit(&acct, "JournalEntry", vid(&pay)).await;
    let adv = t.get("EmployeeAdvance", uid(&adv)).await;
    assert_eq!((dec(&adv["paid_amount"]), adv["status"].clone()), (n("5000"), json!("Paid")));
    assert_eq!(t.gl("Employee Advances").await, n("5000"));

    // A claim settling the advance, with input tax.
    let claim = t
        .ins(
            &me,
            "ExpenseClaim",
            json!({"employee": e["id"], "posting_date": "2026-10-12",
            "expenses": [{"expense_date": "2026-10-10", "expense_type": travel["id"], "amount": "5000", "description": "Flight"},
                         {"expense_date": "2026-10-11", "expense_type": food["id"], "amount": "1500"}],
            "taxes": [{"account_head": t.account("Input Tax").await, "description": "GST", "rate": "5"}]}),
        )
        .await;
    assert_eq!(
        (dec(&claim["total_sanctioned_amount"]), dec(&claim["grand_total"]), claim["expense_approver"].clone()),
        (n("6500"), n("6825"), json!("boss@hrexp.test"))
    );
    let claim = t.act(&me, "ExpenseClaim", uid(&claim), "get_advances", json!({})).await;
    assert_eq!(dec(&claim["advances"][0]["allocated_amount"]), n("5000"));
    // The approver sanctions less for food; the rate-based tax follows.
    let mut rows = claim["expenses"].as_array().unwrap().clone();
    rows[1]["sanctioned_amount"] = json!("1200");
    let claim = t.k.update(&boss, "ExpenseClaim", vid(&claim), obj(json!({"expenses": rows})), None).await.expect("sanction");
    assert_eq!((dec(&claim["grand_total"]), dec(&claim["outstanding_amount"])), (n("6510"), n("1510")));
    forbidden(t.trans(&other, "ExpenseClaim", uid(&claim), "Approve").await);
    forbidden(t.trans(&me, "ExpenseClaim", uid(&claim), "Approve").await);
    t.trans(&boss, "ExpenseClaim", uid(&claim), "Approve").await.expect("approve claim");
    let claim = t.get("ExpenseClaim", uid(&claim)).await;
    assert_eq!((claim["status"].clone(), dec(&claim["outstanding_amount"])), (json!("Unpaid"), n("1510")));
    assert_eq!(t.gl("Travel Expenses").await, n("5000"));
    assert_eq!(t.gl("Administrative Expenses").await, n("1200"));
    assert_eq!(t.gl("Input Tax").await, n("310"));
    assert_eq!(t.gl("Employee Advances").await, Decimal::ZERO, "advance settled");
    assert_eq!(t.gl("Employee Payables").await, n("-1510"));
    let adv = t.get("EmployeeAdvance", uid(&adv)).await;
    assert_eq!((dec(&adv["claimed_amount"]), adv["status"].clone()), (n("5000"), json!("Claimed")));

    // Paying the claim; it cannot be cancelled while paid.
    refused(t.k.run_action(&hr, "ExpenseClaim", uid(&claim), "make_payment_entry", json!({"amount": "2000"})).await, "between 0 and 1510");
    let reimb = t.act(&hr, "ExpenseClaim", uid(&claim), "make_payment_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&reimb)).await;
    let claim = t.get("ExpenseClaim", uid(&claim)).await;
    assert_eq!((claim["status"].clone(), dec(&claim["outstanding_amount"])), (json!("Paid"), Decimal::ZERO));
    assert_eq!(t.gl("Employee Payables").await, Decimal::ZERO);
    refused(t.trans(&boss, "ExpenseClaim", uid(&claim), "Cancel").await, "paid");
    t.k.cancel(&acct, "JournalEntry", vid(&reimb), None).await.expect("cancel payment");
    assert_eq!(t.get("ExpenseClaim", uid(&claim)).await["status"], json!("Unpaid"));
    t.trans(&boss, "ExpenseClaim", uid(&claim), "Cancel").await.expect("cancel claim");
    assert_eq!(t.gl("Travel Expenses").await, Decimal::ZERO, "reversed");
    assert_eq!(t.gl("Employee Payables").await, Decimal::ZERO);
    let adv = t.get("EmployeeAdvance", uid(&adv)).await;
    assert_eq!((dec(&adv["claimed_amount"]), dec(&adv["pending_amount"]), adv["status"].clone()), (Decimal::ZERO, n("5000"), json!("Paid")));

    // Returning part of the unused advance.
    let ret = t.act(&hr, "EmployeeAdvance", uid(&adv), "make_return_entry", json!({"amount": "1000"})).await;
    t.submit(&acct, "JournalEntry", vid(&ret)).await;
    let adv = t.get("EmployeeAdvance", uid(&adv)).await;
    assert_eq!((dec(&adv["return_amount"]), dec(&adv["pending_amount"])), (n("1000"), n("4000")));
    assert_eq!(t.gl("Employee Advances").await, n("4000"));
    refused(t.k.cancel(&hr, "EmployeeAdvance", uid(&adv), None).await, "has been paid");

    // A rejected claim posts nothing.
    let small = t
        .ins(&me, "ExpenseClaim", json!({"employee": e["id"], "posting_date": "2026-10-20", "expenses": [{"expense_date": "2026-10-20", "expense_type": food["id"], "amount": "300"}]}))
        .await;
    t.trans(&boss, "ExpenseClaim", uid(&small), "Reject").await.expect("reject");
    assert_eq!(t.get("ExpenseClaim", uid(&small)).await["status"], json!("Rejected"));
    assert_eq!(t.gl("Administrative Expenses").await, Decimal::ZERO);
    refused(
        t.try_ins(
            &me,
            "ExpenseClaim",
            json!({"employee": e["id"], "expenses": [{"expense_date": "2026-10-20", "expense_type": food["id"], "amount": "300", "sanctioned_amount": "400"}]}),
        )
        .await,
        "cannot exceed",
    );
    refused(
        t.try_ins(
            &hr,
            "ExpenseClaim",
            json!({"employee": e["id"], "payable_account": t.account("Primary Bank").await,
        "expenses": [{"expense_date": "2026-10-20", "expense_type": food["id"], "amount": "300"}]}),
        )
        .await,
        "must be a liability account",
    );
    forbidden(
        t.try_ins(&me, "ExpenseClaim", json!({"employee": m["id"], "expenses": [{"expense_date": "2026-10-20", "expense_type": food["id"], "amount": "300"}]}))
            .await,
    );

    let summary = t.report(&hr, "expense_claim_summary", &[("from_date", "2026-04-01"), ("to_date", "2026-10-31")]).await.expect("summary");
    assert_eq!(summary["rows"].as_array().unwrap().len(), 1, "the cancelled claim is left out");
    assert_eq!(summary["rows"][0]["status"], json!("Rejected"));
}

// ───────────────────────────── payroll ─────────────────────────────

async fn payroll_scenario() {
    let t = T::new("hrpay").await;
    let pm = t.user("pm@hrpay.test", &["Payroll Manager"]).await;
    let hr = t.user("hr@hrpay.test", &["HR Manager"]).await;
    let hru = t.user("hruser@hrpay.test", &["HR User"]).await;
    let acct = t.user("acct@hrpay.test", &["Accounts Manager"]).await;
    let p1u = t.user("p1@hrpay.test", &["Employee"]).await;
    let comp = |abbr: &str| {
        let t = &t;
        let abbr = abbr.to_string();
        async move { t.one("SalaryComponent", json!({"abbr": abbr})).await }
    };
    let (bs, hra, sa, pf, it, bonus, le) =
        (comp("BS").await, comp("HRA").await, comp("SA").await, comp("PF").await, comp("IT").await, comp("BON").await, comp("LE").await);
    // Deductions need their own accounts; earnings fall back to Salaries.
    let liability = |name: &str| json!({"account_name": name, "company": t.c(), "root_type": "Liability"});
    let pf_acc = t.ins(&t.admin, "Account", liability("Provident Fund Payable")).await;
    let pt_acc = t.ins(&t.admin, "Account", liability("Professional Tax Payable")).await;
    t.k.update(&pm, "SalaryComponent", uid(&pf), obj(json!({"accounts": [{"company": t.c(), "account": pf_acc["id"]}]})), None).await.unwrap();
    t.k.update(&pm, "SalaryComponent", uid(&it), obj(json!({"accounts": [{"company": t.c(), "account": t.account("TDS Payable").await}]})), None)
        .await
        .unwrap();
    let pt = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Professional Tax", "abbr": "PT", "component_type": "Deduction", "amount_based_on_formula": true,
            "formula": "gross > 15000 ? 200 : 0", "depends_on_payment_days": false, "accounts": [{"company": t.c(), "account": pt_acc["id"]}]}),
        )
        .await;
    refused(
        t.try_ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Bad", "abbr": "BAD", "component_type": "Earning", "amount_based_on_formula": true, "formula": "BS * 0.1 + FOO"}),
        )
        .await,
        "FOO",
    );
    refused(t.try_ins(&pm, "SalaryComponent", json!({"component_name": "Base", "abbr": "base", "component_type": "Earning"})).await, "reserved");

    let line = |c: &Doc| json!({"salary_component": c["id"]});
    let structure = |earnings: Vec<Value>, deductions: Vec<Value>| {
        json!({"structure_name": "Standard Monthly", "company": t.c(), "payroll_frequency": "Monthly", "earnings": earnings, "deductions": deductions,
               "leave_encashment_amount_per_day": "2000"})
    };
    let mut bad = line(&hra);
    bad["formula"] = json!("BS * 0.4 + XYZ");
    refused(t.try_ins(&pm, "SalaryStructure", structure(vec![line(&bs), bad], vec![])).await, "unknown name XYZ");
    refused(t.try_ins(&pm, "SalaryStructure", structure(vec![line(&hra), line(&bs)], vec![])).await, "unknown name BS");
    let st = t.ins_submit(&pm, "SalaryStructure", structure(vec![line(&bs), line(&hra), line(&sa)], vec![line(&pf), line(&pt), line(&it)])).await;
    assert_eq!(st["earnings"][1]["abbr"], json!("HRA"));

    let slab = t
        .ins(&pm, "IncomeTaxSlab", json!({"slab_name": "New Regime", "effective_from": "2026-04-01", "standard_tax_exemption_amount": "75000",
            "slabs": [{"from_amount": "0", "to_amount": "400000", "percent_deduction": "0"}, {"from_amount": "400000", "to_amount": "800000", "percent_deduction": "5"},
                      {"from_amount": "800000", "to_amount": "1200000", "percent_deduction": "10"}, {"from_amount": "1200000", "to_amount": "0", "percent_deduction": "15"}]}))
        .await;
    let fy = t.ins(&pm, "PayrollPeriod", json!({"period_name": "FY 2026-27", "company": t.c(), "start_date": "2026-04-01", "end_date": "2027-03-31"})).await;
    let p1 = t.employee("Priya", json!({"user": "p1@hrpay.test", "bank_name": "HDFC", "bank_account_no": "0011223344", "ifsc_code": "HDFC0000001"})).await;
    let p2 = t.employee("Rahul", json!({"date_of_joining": "2026-09-16"})).await;
    let p3 = t.employee("Sana", json!({})).await;
    let ssa = |e: &Doc, from: &str, base: &str| json!({"employee": e["id"], "salary_structure": st["id"], "from_date": from, "base": base, "income_tax_slab": slab["id"]});
    let ssa1 = t.ins_submit(&pm, "SalaryStructureAssignment", ssa(&p1, "2026-04-01", "150000")).await;
    refused(t.k.cancel(&pm, "SalaryStructure", uid(&st), None).await, "uses this structure");
    refused(t.try_ins(&pm, "SalaryStructureAssignment", ssa(&p1, "2026-04-01", "160000")).await, "already starts on 2026-04-01");
    let mut no_slab = ssa(&p2, "2026-09-16", "40000");
    no_slab["income_tax_slab"] = Value::Null;
    refused(t.try_ins(&pm, "SalaryStructureAssignment", no_slab).await, "income tax slab");
    t.ins_submit(&pm, "SalaryStructureAssignment", ssa(&p2, "2026-09-16", "40000")).await;
    t.ins_submit(&pm, "SalaryStructureAssignment", ssa(&p3, "2026-04-01", "30000")).await;
    let cat = t.ins(&pm, "EmployeeTaxExemptionCategory", json!({"category_name": "Investments", "max_amount": "150000"})).await;
    t.ins_submit(
        &pm,
        "EmployeeTaxExemptionDeclaration",
        json!({"employee": p1["id"], "payroll_period": fy["id"], "declarations": [{"exemption_category": cat["id"], "amount": "50000"}]}),
    )
    .await;

    // Sana: two days unpaid leave, one absence and a half day; plus a bonus.
    let lwp = t.one("LeaveType", json!({"leave_type_name": "Leave Without Pay"})).await;
    let leave =
        t.ins(&hr, "LeaveApplication", json!({"employee": p3["id"], "leave_type": lwp["id"], "from_date": "2026-09-21", "to_date": "2026-09-22"})).await;
    t.trans(&hr, "LeaveApplication", uid(&leave), "Approve").await.expect("approve lwp");
    t.ins_submit(&hr, "Attendance", json!({"employee": p3["id"], "attendance_date": "2026-09-23", "status": "Absent"})).await;
    t.ins_submit(&hr, "Attendance", json!({"employee": p3["id"], "attendance_date": "2026-09-24", "status": "Half Day"})).await;
    t.ins_submit(&pm, "AdditionalSalary", json!({"employee": p3["id"], "salary_component": bonus["id"], "amount": "5000", "payroll_date": "2026-09-30"})).await;

    // The September run.
    let pe = t.ins(&pm, "PayrollEntry", json!({"company": t.c(), "posting_date": "2026-09-30", "start_date": "2026-09-01", "end_date": "2026-09-30"})).await;
    let pe = t.act(&pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await;
    assert_eq!(pe["employees"].as_array().unwrap().len(), 3);
    let pe_id = vid(&pe);
    t.submit(&pm, "PayrollEntry", pe_id).await;
    refused(
        t.try_ins(&pm, "PayrollEntry", json!({"company": t.c(), "start_date": "2026-09-01", "end_date": "2026-09-30"})).await,
        "already covers this period",
    );
    t.act(&pm, "PayrollEntry", pe_id, "create_salary_slips", json!({})).await;
    let again = t.act(&pm, "PayrollEntry", pe_id, "create_salary_slips", json!({})).await;
    assert!(again["employees"].as_array().unwrap().iter().all(|r| r["salary_slip"].is_string()));
    let slips = t.list(&pm, "SalarySlip", json!({"payroll_entry": pe_id.to_string()})).await;
    assert_eq!(slips.len(), 3, "creating slips twice makes no duplicates");
    let slip_of = |e: &Doc| {
        let t = &t;
        let e = e["id"].as_str().unwrap().to_string();
        async move {
            let s = t.one("SalarySlip", json!({"employee": e})).await;
            t.get("SalarySlip", uid(&s)).await
        }
    };
    let amount = |s: &Doc, table: &str, abbr: &str| {
        s[table].as_array().unwrap().iter().find(|r| r["abbr"] == json!(abbr)).map(|r| dec(&r["amount"])).unwrap_or_default()
    };
    let s1 = slip_of(&p1).await;
    assert_eq!((dec(&s1["payment_days"]), dec(&s1["gross_pay"])), (n("30"), n("150000")));
    assert_eq!((amount(&s1, "earnings", "BS"), amount(&s1, "earnings", "HRA"), amount(&s1, "earnings", "SA")), (n("75000"), n("30000"), n("45000")));
    assert_eq!((amount(&s1, "deductions", "PF"), amount(&s1, "deductions", "PT")), (n("1800"), n("200")));
    // (148200 × 7 − 75000 − 50000) = 912400 → 20000 + 11240 = 31240 a year, over 7 months.
    assert_eq!(amount(&s1, "deductions", "IT"), n("4462.86"));
    assert_eq!(dec(&s1["net_pay"]), n("143537.14"));
    let s2 = slip_of(&p2).await;
    assert_eq!((dec(&s2["total_working_days"]), dec(&s2["payment_days"])), (n("30"), n("15")), "joined on the 16th");
    assert_eq!((amount(&s2, "earnings", "BS"), dec(&s2["gross_pay"]), amount(&s2, "deductions", "PF")), (n("10000"), n("20000"), n("900")));
    assert_eq!(dec(&s2["net_pay"]), n("18900"));
    let s3 = slip_of(&p3).await;
    assert_eq!((dec(&s3["leave_without_pay"]), dec(&s3["absent_days"]), dec(&s3["payment_days"])), (n("2"), n("1.5"), n("26.5")));
    assert_eq!((amount(&s3, "earnings", "BS"), amount(&s3, "earnings", "BON"), dec(&s3["gross_pay"])), (n("13250"), n("5000"), n("31500")));
    assert_eq!((amount(&s3, "deductions", "PF"), dec(&s3["net_pay"])), (n("1590"), n("29710")));

    refused(t.k.submit(&pm, "SalarySlip", uid(&s1), None).await, "from the payroll entry");
    let pe = t.act(&pm, "PayrollEntry", pe_id, "submit_salary_slips", json!({})).await;
    assert_eq!((pe["status"].clone(), dec(&pe["total_net_pay"]), dec(&pe["total_gross_pay"])), (json!("Slips Submitted"), n("192147.14"), n("201500")));
    let journals = t.scalar_i64("select count(*) from journal_entry where tenant_id = $1 and source_id = $2", pe_id).await;
    assert_eq!(journals, 1, "one accrual for the run");
    assert_eq!(t.gl("Salaries").await, n("201500"));
    assert_eq!(t.gl("Provident Fund Payable").await, n("-4290"));
    assert_eq!(t.gl("Professional Tax Payable").await, n("-600"));
    assert_eq!(t.gl("TDS Payable").await, n("-4462.86"));
    assert_eq!(t.gl("Payroll Payable").await, n("-192147.14"));
    refused(t.k.cancel(&pm, "SalarySlip", uid(&s1), None).await, "cancel the payroll entry instead");
    refused(t.k.cancel(&pm, "SalaryStructureAssignment", uid(&ssa1), None).await, "computed from this assignment");

    // Employees read their own slips only.
    let own = t.list(&p1u, "SalarySlip", json!({})).await;
    assert_eq!(own.len(), 1);
    assert_eq!(own[0]["employee"], p1["id"]);
    refused(t.k.get(&p1u, "SalarySlip", uid(&s2), false).await, "not found");
    let mine = t.k.get(&p1u, "Employee", uid(&p1), false).await.expect("own employee record");
    assert_eq!(mine["bank_account_no"], json!("0011223344"));
    let masked = t.k.get(&hru, "Employee", uid(&p1), false).await.unwrap();
    assert_eq!(masked["bank_account_no"], json!("•••• 3344"), "HR users see only the last four of bank details");
    assert!(masked.get("bank_name").is_none(), "and nothing of what has no mask");

    // Reports.
    let reg = t.report(&pm, "salary_register", &[("from_date", "2026-09-01"), ("to_date", "2026-09-30")]).await.expect("register");
    assert_eq!(reg["rows"].as_array().unwrap().len(), 3);
    assert_eq!((dec(&reg["totals"]["net_pay"]), dec(&reg["totals"]["c_BS"])), (n("192147.14"), n("98250")));
    let summary = t.report(&pm, "payroll_summary", &[("from_date", "2026-09-01"), ("to_date", "2026-09-30")]).await.expect("summary");
    assert_eq!(dec(&summary["totals"]["gross_pay"]), n("201500"));
    let bank = t.report(&pm, "bank_remittance", &[("payroll_entry", &pe_id.to_string())]).await.expect("remittance");
    assert_eq!(row(&bank, "employee_name", "Priya Test")["bank_account_no"], json!("0011223344"));
    assert_eq!(bank["meta"]["missing_bank_details"], json!(2));
    t.report(&hr, "bank_remittance", &[("payroll_entry", &pe_id.to_string())]).await.expect("HR managers see bank details");
    forbidden(t.report(&hru, "bank_remittance", &[("payroll_entry", &pe_id.to_string())]).await);
    let tax = t.report(&pm, "income_tax_deductions", &[("from_date", "2026-04-01"), ("to_date", "2027-03-31")]).await.expect("tax report");
    assert_eq!(dec(&tax["totals"]["income_tax"]), n("4462.86"));
    // For an AI provider the workspace allowed nothing more, neither: salary slips stay inside the
    // workspace, and bank details are in a field group.
    let provider = pm.for_provider(ferrum_kernel::ctx::Egress::default());
    forbidden(t.report(&provider, "income_tax_deductions", &[("from_date", "2026-04-01"), ("to_date", "2027-03-31")]).await);
    forbidden(t.report(&provider, "bank_remittance", &[("payroll_entry", &pe_id.to_string())]).await);
    let own_reg = t.report(&p1u, "salary_register", &[("from_date", "2026-09-01"), ("to_date", "2026-09-30")]).await.expect("own register");
    assert_eq!(own_reg["rows"].as_array().unwrap().len(), 1);

    // The file the bank pays the run from: refused while anyone paid by bank has no account to pay
    // into, and to those who may not read bank details; then a salary credit transfer to each.
    let file = |params: Value| t.k.run_action(&pm, "PayrollEntry", pe_id, "bank_file", params);
    refused(file(json!({"from_account": "50100012345678"})).await, "no bank account for Rahul Test, Sana Test");
    forbidden(t.k.run_action(&hru, "PayrollEntry", pe_id, "bank_file", json!({})).await);
    t.k.update(&t.admin, "Employee", uid(&p2), obj(json!({"salary_mode": "Cash"})), None).await.expect("Rahul paid in cash");
    t.k.update(&t.admin, "Employee", uid(&p3), obj(json!({"iban": "de89 3704 0044 0532 0130 00"})), None).await.expect("Sana's IBAN");
    refused(file(json!({})).await, "the company's account at the bank");
    let pain = file(json!({"from_account": "5010 0012 3456 78", "from_bic": "HDFCINBB", "execution_date": "2026-10-01"})).await.expect("pain.001");
    assert_eq!((pain["payments"].clone(), dec(&pain["total"])), (json!(2), n("173247.14")), "Priya and Sana; Rahul in cash");
    assert!(pain["file"]["name"].as_str().unwrap().ends_with("-pain.001.xml"));
    let xml = pain["file"]["content"].as_str().unwrap();
    for part in [
        "<NbOfTxs>2</NbOfTxs><CtrlSum>173247.14</CtrlSum>",
        "<ReqdExctnDt><Dt>2026-10-01</Dt></ReqdExctnDt>",
        "<DbtrAcct><Id><Othr><Id>50100012345678</Id></Othr></Id></DbtrAcct><DbtrAgt><FinInstnId><BICFI>HDFCINBB</BICFI></FinInstnId></DbtrAgt>",
        "<InstdAmt Ccy=\"INR\">143537.14</InstdAmt>",
        "<MmbId>HDFC0000001</MmbId></ClrSysMmbId></FinInstnId></CdtrAgt><Cdtr><Nm>Priya Test</Nm></Cdtr><CdtrAcct><Id><Othr><Id>0011223344</Id></Othr></Id></CdtrAcct>",
        "<InstdAmt Ccy=\"INR\">29710.00</InstdAmt></Amt><Cdtr><Nm>Sana Test</Nm></Cdtr><CdtrAcct><Id><IBAN>DE89370400440532013000</IBAN></Id></CdtrAcct>",
    ] {
        assert!(xml.contains(part), "{part} in {xml}");
    }
    let sheet = file(json!({"format": "CSV", "from_account": "50100012345678"})).await.expect("csv");
    assert_eq!(sheet["file"]["content"].as_str().unwrap().lines().count(), 3, "a header and the two paid by bank");
    refused(file(json!({"format": "MT103", "from_account": "50100012345678"})).await, "format");

    // Paying, then unwinding in order.
    let bank_entry = t.act(&pm, "PayrollEntry", pe_id, "make_bank_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&bank_entry)).await;
    let pe = t.get("PayrollEntry", pe_id).await;
    assert_eq!((pe["status"].clone(), dec(&pe["paid_amount"])), (json!("Paid"), n("192147.14")));
    assert_eq!(t.gl("Payroll Payable").await, Decimal::ZERO);
    for sl in &slips {
        let sl = t.get("SalarySlip", uid(sl)).await;
        assert_eq!(dec(&sl["paid_amount"]), dec(&sl["net_pay"]), "each slip paid in full");
    }
    // Paid before 0.10.0, the run's bank entry is shared out over its slips on upgrade, as the bank
    // file lists them.
    {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query("delete from pay_salary_payment where tenant_id = $1").bind(t.tenant).execute(&mut *tx).await.unwrap();
        sqlx::query("update e_salary_slip set paid_amount = null where tenant_id = $1").bind(t.tenant).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("../packages/payroll_erp/sql/001_salary_payments.sql"))).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    for sl in &slips {
        let sl = t.get("SalarySlip", uid(sl)).await;
        assert_eq!(dec(&sl["paid_amount"]), dec(&sl["net_pay"]), "{} after the upgrade", sl["employee_name"]);
    }
    refused(t.k.cancel(&pm, "PayrollEntry", pe_id, None).await, "cancel the bank entry first");
    t.k.cancel(&acct, "JournalEntry", vid(&bank_entry), None).await.expect("cancel bank entry");
    t.k.cancel(&pm, "PayrollEntry", pe_id, None).await.expect("cancel payroll entry");
    assert_eq!(t.gl("Salaries").await, Decimal::ZERO, "accrual reversed");
    assert_eq!(t.gl("Payroll Payable").await, Decimal::ZERO);
    assert_eq!(t.get("SalarySlip", uid(&s1)).await["docstatus"], json!(2));

    // Leave encashment pays unused leave through additional salary.
    let pl = t.one("LeaveType", json!({"leave_type_name": "Privilege Leave"})).await;
    t.ins_submit(
        &hr,
        "LeaveAllocation",
        json!({"employee": p1["id"], "leave_type": pl["id"], "from_date": "2026-01-01", "to_date": "2026-12-31", "new_leaves_allocated": "18"}),
    )
    .await;
    let enc = t
        .ins(&pm, "LeaveEncashment", json!({"employee": p1["id"], "leave_type": pl["id"], "encashment_date": "2026-10-15", "salary_component": le["id"]}))
        .await;
    assert_eq!((dec(&enc["encashable_days"]), dec(&enc["encashment_amount"])), (n("8"), n("16000")), "18 less the 10-day threshold, at 2000 a day");
    let enc = t.submit(&pm, "LeaveEncashment", uid(&enc)).await;
    let extra = t.get("AdditionalSalary", enc["additional_salary"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!((extra["docstatus"].clone(), dec(&extra["amount"])), (json!(1), n("16000")));
    let oct = t.ins(&pm, "SalarySlip", json!({"employee": p1["id"], "start_date": "2026-10-01", "end_date": "2026-10-31"})).await;
    assert_eq!(amount(&oct, "earnings", "LE"), n("16000"));
    t.k.delete(&pm, "SalarySlip", uid(&oct), None).await.unwrap();
    t.k.cancel(&pm, "LeaveEncashment", uid(&enc), None).await.expect("cancel encashment");
    assert_eq!(t.get("AdditionalSalary", uid(&extra)).await["docstatus"], json!(2));

    // The slab's cess and rebate reach the slip. With September's run cancelled, October projects
    // six months of 148,200 taxable, less the 75,000 standard and 50,000 declared exemptions:
    // 764,200, which the slabs tax 18,210, 3,035 a month.
    let october_tax = || async {
        let slip = t.ins(&pm, "SalarySlip", json!({"employee": p1["id"], "start_date": "2026-10-01", "end_date": "2026-10-31"})).await;
        t.k.delete(&pm, "SalarySlip", uid(&slip), None).await.unwrap();
        amount(&slip, "deductions", "IT")
    };
    assert_eq!(october_tax().await, n("3035"));
    let cess = json!({"other_taxes_and_charges": [{"description": "Health and Education Cess", "percent": "4"}]});
    t.k.update(&pm, "IncomeTaxSlab", uid(&slab), obj(cess), None).await.unwrap();
    assert_eq!(october_tax().await, n("3156.40"), "4% on the tax");
    t.k.update(&pm, "IncomeTaxSlab", uid(&slab), obj(json!({"rebate_limit": "1200000", "rebate_max": "60000"})), None).await.unwrap();
    assert_eq!(october_tax().await, Decimal::ZERO, "the rebate takes it all: 764,200 is under 12 lakh");
    t.k.update(&pm, "IncomeTaxSlab", uid(&slab), obj(json!({"rebate_limit": "700000"})), None).await.unwrap();
    assert_eq!(october_tax().await, n("3156.40"), "nor above its limit");
}

// ───────────────────────────── recruitment, lifecycle, appraisal ─────────────────────────────

async fn talent_scenario() {
    let t = T::new("hrtal").await;
    let hr = t.user("hr@hrtal.test", &["HR Manager"]).await;
    let me = t.user("me@hrtal.test", &["Employee"]).await;
    let dept = t.ins(&hr, "Department", json!({"department_name": "Engineering", "company": t.c()})).await;
    let eng = t.ins(&hr, "Designation", json!({"designation_name": "Engineer"})).await;
    let senior = t.ins(&hr, "Designation", json!({"designation_name": "Senior Engineer"})).await;
    let opening = t
        .ins(
            &hr,
            "JobOpening",
            json!({"job_title": "Backend Engineer", "company": t.c(), "designation": eng["id"], "department": dept["id"], "vacancies": 1, "publish": true}),
        )
        .await;
    let applicant = t
        .ins(&hr, "JobApplicant", json!({"applicant_name": "Kiran Rao", "email_id": "kiran@mail.test", "job_opening": opening["id"], "source": "Job Portal"}))
        .await;
    t.ins(&hr, "JobApplicant", json!({"applicant_name": "Lee Park", "email_id": "lee@mail.test", "job_opening": opening["id"], "status": "Rejected"})).await;
    refused(
        t.try_ins(&hr, "JobApplicant", json!({"applicant_name": "Kiran again", "email_id": "kiran@mail.test", "job_opening": opening["id"]})).await,
        "already has this Email Id and Job Opening",
    );
    let interview = |feedback: Value| {
        json!({"job_applicant": applicant["id"], "interview_round": "Technical", "scheduled_on": "2026-10-05T10:00:00+05:30",
        "interviewers": [{"interviewer": "a@hrtal.test"}, {"interviewer": "b@hrtal.test"}], "feedback": feedback})
    };
    refused(t.try_ins(&hr, "Interview", interview(json!([{"interviewer": "z@hrtal.test", "rating": 4, "result": "Cleared"}]))).await, "panel");
    let iv = t
        .ins(
            &hr,
            "Interview",
            interview(
                json!([{"interviewer": "a@hrtal.test", "rating": 4, "result": "Cleared"}, {"interviewer": "b@hrtal.test", "rating": 5, "result": "Cleared"}]),
            ),
        )
        .await;
    assert_eq!((dec(&iv["average_rating"]), iv["status"].clone()), (n("4.5"), json!("Under Review")));
    refused(t.try_ins(&hr, "Interview", interview(json!([{"interviewer": "a@hrtal.test", "rating": 6, "result": "Cleared"}]))).await, "5");

    let offer = t
        .ins_submit(
            &hr,
            "JobOffer",
            json!({"job_applicant": applicant["id"], "company": t.c(), "offer_date": "2026-10-10", "expected_joining_date": "2026-10-20", "ctc": "1800000"}),
        )
        .await;
    assert_eq!((offer["designation"].clone(), offer["status"].clone()), (eng["id"].clone(), json!("Awaiting Response")), "designation from the opening");
    refused(t.k.run_action(&hr, "JobOffer", uid(&offer), "make_employee", json!({})).await, "not available");
    t.act(&hr, "JobOffer", uid(&offer), "accept", json!({})).await;
    assert_eq!(t.get("JobApplicant", uid(&applicant)).await["status"], json!("Accepted"));
    let emp = t.act(&hr, "JobOffer", uid(&offer), "make_employee", json!({"user": "me@hrtal.test"})).await;
    assert_eq!(
        (emp["first_name"].clone(), emp["last_name"].clone(), emp["designation"].clone(), emp["date_of_joining"].clone()),
        (json!("Kiran"), json!("Rao"), eng["id"].clone(), json!("2026-10-20"))
    );
    refused(t.k.run_action(&hr, "JobOffer", uid(&offer), "make_employee", json!({})).await, "already created");
    let op = t.get("JobOpening", uid(&opening)).await;
    assert_eq!((op["hired"].clone(), op["status"].clone()), (json!(1), json!("Closed")));
    refused(t.k.cancel(&hr, "JobOffer", uid(&offer), None).await, "employee was created");
    let pipe = t.report(&hr, "recruitment_pipeline", &[]).await.expect("pipeline");
    let r = row(&pipe, "job_title", "Backend Engineer");
    assert_eq!(
        (r["accepted"].clone(), r["rejected"].clone(), r["interviews"].clone(), r["offers_accepted"].clone(), r["hired"].clone()),
        (json!(1), json!(1), json!(1), json!(1), json!(1))
    );

    // Onboarding checklist.
    let onb = t
        .ins(
            &hr,
            "EmployeeOnboarding",
            json!({"job_applicant": applicant["id"], "employee": emp["id"], "company": t.c(),
            "activities": [{"activity_name": "Laptop", "assignee": "it@hrtal.test"}, {"activity_name": "ID card"}]}),
        )
        .await;
    assert_eq!((onb["status"].clone(), onb["employee_name"].clone()), (json!("Pending"), json!("Kiran Rao")));
    let mut acts = onb["activities"].as_array().unwrap().clone();
    acts[0]["done"] = json!(true);
    let onb = t.k.update(&hr, "EmployeeOnboarding", uid(&onb), obj(json!({"activities": acts.clone()})), None).await.unwrap();
    assert_eq!(onb["status"], json!("In Process"));
    assert!(onb["activities"][0]["completed_on"].is_string());
    acts[1]["done"] = json!(true);
    assert_eq!(t.k.update(&hr, "EmployeeOnboarding", uid(&onb), obj(json!({"activities": acts})), None).await.unwrap()["status"], json!("Completed"));

    // Promotion with history, refused into the future, undone on cancel.
    let eid = vid(&emp);
    refused(
        t.k.submit(
            &hr,
            "EmployeePromotion",
            uid(&t.ins(&hr, "EmployeePromotion", json!({"employee": emp["id"], "promotion_date": "2030-01-01", "new_designation": senior["id"]})).await),
            None,
        )
        .await,
        "before its date",
    );
    refused(
        t.try_ins(&hr, "EmployeePromotion", json!({"employee": emp["id"], "promotion_date": "2026-10-25", "new_designation": eng["id"]})).await,
        "changes nothing",
    );
    let promo = t
        .ins_submit(
            &hr,
            "EmployeePromotion",
            json!({"employee": emp["id"], "promotion_date": "2026-10-25", "new_designation": senior["id"], "revised_ctc": "2100000"}),
        )
        .await;
    assert_eq!(promo["current_designation"], eng["id"], "history kept on the promotion");
    assert_eq!(t.get("Employee", eid).await["designation"], senior["id"]);
    assert_eq!(t.list(&me, "EmployeePromotion", json!({})).await.len(), 2, "the employee sees their promotions");
    let boss = t.employee("Bina", json!({"department": dept["id"]})).await;
    let transfer = t
        .ins_submit(
            &hr,
            "EmployeeTransfer",
            json!({"employee": emp["id"], "transfer_date": "2026-10-26", "new_reports_to": boss["id"], "new_department": dept["id"]}),
        )
        .await;
    assert_eq!(t.get("Employee", eid).await["reports_to"], boss["id"]);
    t.k.cancel(&hr, "EmployeeTransfer", uid(&transfer), None).await.expect("cancel transfer");
    assert_eq!(t.get("Employee", eid).await["reports_to"], Value::Null, "transfer undone");
    let lead = t.ins(&hr, "Designation", json!({"designation_name": "Lead Engineer"})).await;
    let promo2 = t.ins_submit(&hr, "EmployeePromotion", json!({"employee": emp["id"], "promotion_date": "2026-10-27", "new_designation": lead["id"]})).await;
    assert_eq!(promo2["current_designation"], senior["id"]);
    refused(t.k.cancel(&hr, "EmployeePromotion", uid(&promo), None).await, "changed after this promotion");
    t.k.cancel(&hr, "EmployeePromotion", uid(&promo2), None).await.expect("cancel the later promotion");
    t.k.cancel(&hr, "EmployeePromotion", uid(&promo), None).await.expect("cancel promotion");
    assert_eq!(t.get("Employee", eid).await["designation"], eng["id"], "promotion undone");

    // Appraisal: weights must total 100; the employee scores themselves, HR scores and submits.
    refused(
        t.try_ins(
            &hr,
            "AppraisalTemplate",
            json!({"template_title": "Bad", "goals": [{"kra": "A", "per_weightage": "50"}, {"kra": "B", "per_weightage": "40"}]}),
        )
        .await,
        "90%",
    );
    let tpl = t
        .ins(
            &hr,
            "AppraisalTemplate",
            json!({"template_title": "Engineering", "goals": [{"kra": "Delivery", "per_weightage": "60"}, {"kra": "Quality", "per_weightage": "40"}]}),
        )
        .await;
    let cycle = t
        .ins(
            &hr,
            "AppraisalCycle",
            json!({"cycle_name": "H2 2026", "company": t.c(), "start_date": "2026-10-01", "end_date": "2027-03-31", "status": "In Progress"}),
        )
        .await;
    let ap = t.ins(&hr, "Appraisal", json!({"employee": emp["id"], "appraisal_cycle": cycle["id"], "appraisal_template": tpl["id"]})).await;
    assert_eq!(ap["goals"].as_array().unwrap().len(), 2, "goals copied from the template");
    let mut goals = ap["goals"].as_array().unwrap().clone();
    goals[0]["self_score"] = json!("5");
    goals[0]["manager_score"] = json!("5");
    goals[1]["self_score"] = json!("4");
    let ap = t.k.update(&me, "Appraisal", uid(&ap), obj(json!({"goals": goals})), None).await.expect("self appraisal");
    assert_eq!((dec(&ap["self_score"]), ap["goals"][0]["manager_score"].clone()), (n("4.6"), Value::Null), "employees cannot score for the manager");
    refused(t.k.submit(&hr, "Appraisal", uid(&ap), None).await, "score every goal");
    let mut goals = ap["goals"].as_array().unwrap().clone();
    goals[0]["manager_score"] = json!("4");
    goals[1]["manager_score"] = json!("3");
    t.k.update(&hr, "Appraisal", uid(&ap), obj(json!({"goals": goals})), None).await.expect("manager scores");
    let ap = t.submit(&hr, "Appraisal", uid(&ap)).await;
    assert_eq!(dec(&ap["final_score"]), n("3.6"));
    refused(
        t.try_ins(&hr, "Appraisal", json!({"employee": emp["id"], "appraisal_cycle": cycle["id"], "appraisal_template": tpl["id"]})).await,
        "already has appraisal",
    );

    // People reports.
    let dir = t.report(&hr, "employee_directory", &[]).await.expect("directory");
    assert_eq!(dir["totals"]["count"], json!(2));
    let own = t.report(&me, "employee_directory", &[]).await.expect("own directory");
    assert_eq!(own["totals"]["count"], json!(1), "employees only see themselves");
    let hc = t.report(&hr, "headcount", &[("group_by", "Department"), ("from_date", "2026-04-01"), ("to_date", "2026-10-29")]).await.expect("headcount");
    assert_eq!(dec(&hc["totals"]["headcount"]), n("2"));
    let engineering = row(&hc, "group", "Engineering");
    assert_eq!((engineering["headcount"].clone(), engineering["joined"].clone()), (json!(2), json!(1)));
    t.k.update(&hr, "Employee", uid(&boss), obj(json!({"date_of_birth": "1990-11-05"})), None).await.unwrap();
    let ev = t.report(&hr, "employee_events", &[("from_date", "2026-10-29"), ("days", "30")]).await.expect("events");
    let b = row(&ev, "event", "Birthday");
    assert_eq!((b["date"].clone(), b["years"].clone()), (json!("2026-11-05"), json!(36)));
    // Run for an AI provider the workspace allowed nothing more: a birthday is a date of birth.
    let provider = hr.for_provider(ferrum_kernel::ctx::Egress::default());
    let ev = t.report(&provider, "employee_events", &[("from_date", "2026-10-29"), ("days", "30")]).await.expect("events for a provider");
    assert!(ev["rows"].as_array().unwrap().iter().all(|r| r["event"] != json!("Birthday")), "{ev}");

    // ───────────── training ─────────────
    let asha = t.employee("Training Asha", json!({})).await;
    let ravi = t.employee("Training Ravi", json!({})).await;
    let program = t.ins(&t.admin, "TrainingProgram", json!({"program_name": "Fire safety", "company": t.c(), "valid_for_months": 12, "mandatory": true})).await;
    let session = t
        .ins(
            &t.admin,
            "TrainingEvent",
            json!({"title": "Fire safety, March", "program": program["id"], "company": t.c(), "held_on": "2026-03-31",
                   "attendees": [{"employee": asha["id"], "result": "Passed"}, {"employee": ravi["id"]}]}),
        )
        .await;
    assert!(t.k.submit(&t.admin, "TrainingEvent", uid(&session), None).await.is_err(), "everyone has to be marked first");
    let marks = json!({"attendees": [{"employee": asha["id"], "result": "Passed"}, {"employee": ravi["id"], "result": "Failed"}]});
    t.k.update(&t.admin, "TrainingEvent", uid(&session), marks.as_object().cloned().unwrap(), None).await.expect("marked");
    t.submit(&t.admin, "TrainingEvent", uid(&session)).await;
    let records = t.list(&t.admin, "TrainingRecord", json!({"program": program["id"]})).await;
    assert_eq!(records.len(), 1, "who passed: {records:?}");
    assert_eq!((records[0]["employee"].clone(), records[0]["valid_until"].as_str()), (asha["id"].clone(), Some("2027-03-31")));
    t.k.cancel(&t.admin, "TrainingEvent", uid(&session), None).await.expect("cancelled");
    assert!(t.list(&t.admin, "TrainingRecord", json!({"program": program["id"]})).await.is_empty(), "cancelling it takes it off");
}

/// Hire to retire (FERRUM-JOURNEY-003): hired from an accepted offer with a sign-in, onboarded,
/// then separated. Submitting the separation makes them one who leaves on its relieving date;
/// until that day their sign-in works, on it HR's upkeep ends it (Ferrum deprovisions the user,
/// their work moving to their manager), once. Their full and final statement then gathers their
/// open claim and unsettled advance, takes gratuity and a recovery added by hand, and one journal
/// entry settles all of it and pays the difference.
async fn retire_scenario() {
    let t = T::new("hrret").await;
    let hr = t.user("hr@hrret.test", &["HR Manager"]).await;
    let acct = t.user("acct@hrret.test", &["Accounts Manager"]).await;
    // What the actions read, their pages ask for: a parameter left undeclared can never be given
    // from a page (Allocate Leaves could not work from one, needing its dates).
    let asks = |entity: &str, action: &str| -> Vec<String> {
        let ent = hr.schema.entity(entity).unwrap();
        ent.def.actions.iter().find(|a| a.name == action).unwrap().params.iter().map(|p| p.name.clone()).collect()
    };
    for (entity, action, names) in [
        ("JobOffer", "make_employee", &["date_of_joining", "user"][..]),
        ("LeavePolicy", "assign_allocations", &["from_date", "to_date", "company", "carry_forward"]),
        ("LeavePolicy", "accrue", &["on"]),
        ("HolidayList", "add_weekly_offs", &["weekday"]),
        ("ShiftType", "mark_attendance", &["up_to"]),
        ("ExpenseClaim", "make_payment_entry", &["amount", "bank_account"]),
        ("EmployeeAdvance", "make_payment_entry", &["amount", "bank_account"]),
        ("EmployeeAdvance", "make_return_entry", &["amount", "bank_account"]),
        ("FullAndFinalStatement", "make_payment_entry", &["bank_account", "posting_date"]),
    ] {
        assert_eq!(asks(entity, action), names, "{entity}.{action}");
    }
    t.user("mona@hrret.test", &["Employee"]).await;
    t.user("kiran@hrret.test", &["Employee"]).await;
    let mona = t.employee("Mona", json!({"user": "mona@hrret.test"})).await;
    let eng = t.ins(&hr, "Designation", json!({"designation_name": "Engineer"})).await;
    let opening = t.ins(&hr, "JobOpening", json!({"job_title": "Engineer", "company": t.c(), "designation": eng["id"], "vacancies": 1})).await;
    let applicant = t.ins(&hr, "JobApplicant", json!({"applicant_name": "Kiran Rao", "email_id": "kiran@mail.test", "job_opening": opening["id"]})).await;
    let offer = t
        .ins_submit(
            &hr,
            "JobOffer",
            json!({"job_applicant": applicant["id"], "company": t.c(), "offer_date": "2026-01-10", "expected_joining_date": "2026-02-01"}),
        )
        .await;
    t.act(&hr, "JobOffer", uid(&offer), "accept", json!({})).await;
    let emp = t.act(&hr, "JobOffer", uid(&offer), "make_employee", json!({"user": "kiran@hrret.test"})).await;
    let kiran = vid(&emp);
    assert_eq!(emp["entity"], json!("Employee"), "the page opens the new employee");
    assert!(emp["message"].as_str().unwrap().ends_with(": Kiran Rao joins on 2026-02-01"), "{}", emp["message"]);
    t.k.update(&hr, "Employee", kiran, obj(json!({"reports_to": mona["id"]})), None).await.expect("reports to Mona");
    let onboarding = t
        .ins(&hr, "EmployeeOnboarding", json!({"employee": kiran.to_string(), "company": t.c(), "activities": [{"activity_name": "Laptop", "done": true}]}))
        .await;
    assert_eq!(onboarding["status"], json!("Completed"));
    // While employed: a claim approved and not yet paid, and an advance paid and not yet settled.
    let travel = t.one("ExpenseClaimType", json!({"expense_type": "Travel"})).await;
    let claim = t
        .ins(
            &hr,
            "ExpenseClaim",
            json!({"employee": kiran.to_string(), "expenses": [{"expense_date": "2026-03-02", "expense_type": travel["id"], "amount": "1200"}]}),
        )
        .await;
    t.trans(&hr, "ExpenseClaim", uid(&claim), "Approve").await.expect("claim approved");
    let adv = t.ins_submit(&hr, "EmployeeAdvance", json!({"employee": kiran.to_string(), "purpose": "Conference", "advance_amount": "2000"})).await;
    let paid = t.act(&hr, "EmployeeAdvance", uid(&adv), "make_payment_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&paid)).await;
    let signed_in = t.k.login("hrret", "kiran@hrret.test", PW).await.expect("Kiran signs in");
    let active = |email: &'static str| {
        let t = &t;
        async move {
            let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
            sqlx::query_scalar::<_, bool>("select is_active from app_user where tenant_id = $1 and email = $2::citext")
                .bind(t.tenant)
                .bind(email)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        }
    };
    let today: chrono::NaiveDate = chrono::Utc::now().date_naive();

    // ───────────── a separation needs its day; submitted, the employee leaves on it ─────────────
    let undated = t.ins(&hr, "EmployeeSeparation", json!({"employee": kiran.to_string(), "reason": "Resignation"})).await;
    refused(t.k.submit(&hr, "EmployeeSeparation", uid(&undated), None).await, "relieving_date");
    let later = today + chrono::Duration::days(10);
    let leaving = t
        .ins_submit(
            &hr,
            "EmployeeSeparation",
            json!({"employee": kiran.to_string(), "reason": "Resignation", "relieving_date": later.to_string(), "exit_interview": "Moving cities"}),
        )
        .await;
    let e = t.get("Employee", kiran).await;
    assert_eq!((e["status"].clone(), e["relieving_date"].clone()), (json!("Active"), json!(later.to_string())), "at work until their last working day");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("upkeep");
    assert!(active("kiran@hrret.test").await, "their sign-in works until the day");

    // Cancelled, they stay; separated again on a day already past, they are Left at once and the
    // upkeep ends the sign-in, once.
    t.k.cancel(&hr, "EmployeeSeparation", uid(&leaving), None).await.expect("cancelled");
    let e = t.get("Employee", kiran).await;
    assert_eq!((e["status"].clone(), e["relieving_date"].clone()), (json!("Active"), Value::Null));
    let yesterday = today - chrono::Duration::days(1);
    let left = t
        .ins_submit(
            &hr,
            "EmployeeSeparation",
            json!({"employee": kiran.to_string(), "reason": "Resignation", "resignation_letter_date": yesterday.to_string(), "relieving_date": yesterday.to_string()}),
        )
        .await;
    assert_eq!(t.get("Employee", kiran).await["status"], json!("Left"), "the last working day has passed");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("upkeep");
    assert!(!active("kiran@hrret.test").await, "deprovisioned once the last working day has passed");
    assert!(active("mona@hrret.test").await, "nobody else");
    assert!(t.k.login("hrret", "kiran@hrret.test", PW).await.is_err(), "no sign-in after leaving");
    assert!(t.k.load_ctx(t.tenant, signed_in.user, Some(signed_in.token_version), "it".into()).await.is_err(), "their session ended");
    let audits = |action: &'static str| {
        let t = &t;
        async move {
            let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
            sqlx::query_scalar::<_, i64>("select count(*) from audit_event where tenant_id = $1 and entity = 'User' and action = $2")
                .bind(t.tenant)
                .bind(action)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        }
    };
    let ended = audits("deprovision").await;
    assert_eq!(ended, 1, "recorded once");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("upkeep again");
    assert_eq!(audits("deprovision").await, ended, "an inactive user is not touched again");

    // ───────────── the full and final statement: gathered, added to, settled at once ─────────────
    refused(t.k.run_action(&hr, "EmployeeSeparation", uid(&leaving), "make_full_and_final", json!({})).await, "not available");
    refused(t.try_ins(&hr, "FullAndFinalStatement", json!({"employee": kiran.to_string(), "separation": leaving["id"]})).await, "is not submitted");
    refused(t.try_ins(&hr, "FullAndFinalStatement", json!({"employee": mona["id"], "separation": left["id"]})).await, "another employee's");
    let made = t.act(&hr, "EmployeeSeparation", uid(&left), "make_full_and_final", json!({})).await;
    assert_eq!((made["entity"].clone(), made["message"].clone()), (json!("FullAndFinalStatement"), json!("The statement is made with the 2 amounts open")));
    let sid = vid(&made);
    let again = t.act(&hr, "EmployeeSeparation", uid(&left), "make_full_and_final", json!({})).await;
    assert_eq!(vid(&again), sid, "one statement per separation");
    refused(t.try_ins(&hr, "FullAndFinalStatement", json!({"employee": kiran.to_string(), "separation": left["id"]})).await, "already has its statement");
    let st = t.get("FullAndFinalStatement", sid).await;
    let (claim_line, advance_line) = (&st["payables"][0], &st["receivables"][0]);
    assert_eq!((claim_line["reference_type"].clone(), dec(&claim_line["amount"])), (json!("Expense Claim"), n("1200")));
    assert_eq!((advance_line["reference_type"].clone(), dec(&advance_line["amount"])), (json!("Employee Advance"), n("2000")));
    assert_eq!((dec(&st["net_payable"]), st["status"].clone()), (n("-800"), json!("Draft")));
    // Gratuity paid and notice not served recovered, added by hand; gathering again keeps them.
    let mut payables = st["payables"].as_array().unwrap().clone();
    payables.push(json!({"component": "Gratuity", "account": t.account("Salaries").await, "amount": "15000"}));
    let mut receivables = st["receivables"].as_array().unwrap().clone();
    receivables.push(json!({"component": "Notice not served", "account": t.account("Other Income").await, "amount": "500"}));
    t.k.update(&hr, "FullAndFinalStatement", sid, obj(json!({"payables": payables, "receivables": receivables})), None).await.expect("lines added");
    let st = t.act(&hr, "FullAndFinalStatement", sid, "get_outstanding", json!({})).await;
    assert_eq!((st["payables"].as_array().unwrap().len(), st["receivables"].as_array().unwrap().len()), (2, 2));
    assert_eq!((dec(&st["total_payable"]), dec(&st["total_receivable"]), dec(&st["net_payable"])), (n("16200"), n("2500"), n("13700")));
    let st = t.submit(&hr, "FullAndFinalStatement", sid).await;
    assert_eq!(st["status"], json!("Unpaid"));

    // One entry: what is paid debited, what is recovered credited, the difference from the bank.
    // Changed so it no longer says what the statement says, it is refused.
    let bank = t.gl("Primary Bank").await;
    let je = t.act(&hr, "FullAndFinalStatement", sid, "make_payment_entry", json!({})).await;
    let salaries = t.account("Salaries").await;
    let mut lines = je["accounts"].as_array().unwrap().clone();
    for l in lines.iter_mut() {
        if l["account"] == json!(salaries) {
            l["debit"] = json!("14000");
        } else if dec(&l["credit"]) == n("13700") {
            l["credit"] = json!("12700");
        }
    }
    refused(t.k.update(&acct, "JournalEntry", vid(&je), obj(json!({"accounts": lines})), None).await, "must debit what");
    t.submit(&acct, "JournalEntry", vid(&je)).await;
    let st = t.get("FullAndFinalStatement", sid).await;
    assert_eq!((st["status"].clone(), st["journal_entry"].clone()), (json!("Paid"), je["id"].clone()));
    let c = t.get("ExpenseClaim", uid(&claim)).await;
    assert_eq!((c["status"].clone(), dec(&c["outstanding_amount"])), (json!("Paid"), Decimal::ZERO));
    let a = t.get("EmployeeAdvance", uid(&adv)).await;
    assert_eq!((a["status"].clone(), dec(&a["pending_amount"])), (json!("Returned"), Decimal::ZERO));
    assert_eq!((t.gl("Employee Payables").await, t.gl("Employee Advances").await), (Decimal::ZERO, Decimal::ZERO));
    assert_eq!((t.gl("Salaries").await, t.gl("Other Income").await, t.gl("Primary Bank").await), (n("15000"), n("-500"), bank - n("13700")));
    refused(t.k.run_action(&hr, "FullAndFinalStatement", sid, "make_payment_entry", json!({})).await, "not available");
    refused(t.k.cancel(&hr, "FullAndFinalStatement", sid, None).await, "cancel its journal entry first");

    // The entry cancelled, everything is open again; the statement cancelled, the separation can
    // have a new one.
    t.k.cancel(&acct, "JournalEntry", vid(&je), None).await.expect("entry cancelled");
    let st = t.get("FullAndFinalStatement", sid).await;
    assert_eq!((st["status"].clone(), st["journal_entry"].clone()), (json!("Unpaid"), Value::Null));
    assert_eq!(dec(&t.get("ExpenseClaim", uid(&claim)).await["outstanding_amount"]), n("1200"));
    assert_eq!(dec(&t.get("EmployeeAdvance", uid(&adv)).await["pending_amount"]), n("2000"));
    t.k.cancel(&hr, "FullAndFinalStatement", sid, None).await.expect("statement cancelled");
    assert_eq!(t.get("FullAndFinalStatement", sid).await["status"], json!("Cancelled"));
    let anew = t.act(&hr, "EmployeeSeparation", uid(&left), "make_full_and_final", json!({})).await;
    assert_ne!(vid(&anew), sid, "a new statement once the old one is cancelled");
}

// ───────────────────────────── employer contributions ─────────────────────────────

/// The employer's share of provident fund (its wage ceiling written in the formula, on the pay
/// earned) and of insurance (prorated by payment days): computed like any component, never part of
/// gross or net pay, on the slip and in the register in a section of their own, posted with the run
/// (the cost by the employee's cost centre, against what is owed) and reversed with it.
async fn contribution_scenario() {
    let t = T::new("hrcon").await;
    let pm = t.user("pm@hrcon.test", &["Payroll Manager"]).await;
    let account = |name: &str, root: &str| json!({"account_name": name, "company": t.c(), "root_type": root});
    let pf_owed = t.ins(&t.admin, "Account", account("Employer PF Payable", "Liability")).await;
    let ins_owed = t.ins(&t.admin, "Account", account("Insurance Payable", "Liability")).await;
    let pf_cost = t.ins(&t.admin, "Account", account("Employer PF Cost", "Expense")).await;
    let ops = t.ins(&t.admin, "CostCenter", json!({"cost_center_name": "Operations", "company": t.c()})).await;
    let comp = |abbr: &str| {
        let t = &t;
        let abbr = abbr.to_string();
        async move { t.one("SalaryComponent", json!({"abbr": abbr})).await }
    };
    let (bs, hra, sa) = (comp("BS").await, comp("HRA").await, comp("SA").await);
    let epf = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Employer Provident Fund", "abbr": "EPF", "component_type": "Employer Contribution", "amount_based_on_formula": true,
                   "formula": "min(BS * payment_days / working_days, 15000) * 0.12", "depends_on_payment_days": false,
                   "accounts": [{"company": t.c(), "account": pf_owed["id"], "expense_account": pf_cost["id"]}]}),
        )
        .await;
    let eins = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Employer Insurance", "abbr": "EINS", "component_type": "Employer Contribution", "amount_based_on_formula": true,
                   "formula": "gross * 0.0325", "depends_on_payment_days": true}),
        )
        .await;
    let line = |c: &Doc| json!({"salary_component": c["id"]});
    let structure = |earnings: Vec<Value>, contributions: Vec<Value>| json!({"structure_name": "With Contributions", "company": t.c(), "payroll_frequency": "Monthly", "earnings": earnings, "employer_contributions": contributions});
    refused(t.try_ins(&pm, "SalaryStructure", structure(vec![line(&bs), line(&epf)], vec![])).await, "Employer Provident Fund is not an earning");
    refused(t.try_ins(&pm, "SalaryStructure", structure(vec![line(&bs)], vec![line(&hra)])).await, "House Rent Allowance is not an employer contribution");
    let st = t.ins_submit(&pm, "SalaryStructure", structure(vec![line(&bs), line(&hra), line(&sa)], vec![line(&epf), line(&eins)])).await;
    let priya = t.employee("Priya", json!({"payroll_cost_center": ops["id"]})).await;
    let rahul = t.employee("Rahul", json!({"date_of_joining": "2026-09-16"})).await;
    for (e, from, base) in [(&priya, "2026-04-01", "150000"), (&rahul, "2026-09-16", "40000")] {
        t.ins_submit(&pm, "SalaryStructureAssignment", json!({"employee": e["id"], "salary_structure": st["id"], "from_date": from, "base": base})).await;
    }

    let pe = t.ins(&pm, "PayrollEntry", json!({"company": t.c(), "posting_date": "2026-09-30", "start_date": "2026-09-01", "end_date": "2026-09-30"})).await;
    let pe_id = vid(&t.act(&pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await);
    t.submit(&pm, "PayrollEntry", pe_id).await;
    t.act(&pm, "PayrollEntry", pe_id, "create_salary_slips", json!({})).await;
    let slip_of = |e: &Doc| {
        let t = &t;
        let e = e["id"].as_str().unwrap().to_string();
        async move {
            let s = t.one("SalarySlip", json!({"employee": e})).await;
            t.get("SalarySlip", uid(&s)).await
        }
    };
    let amount = |s: &Doc, table: &str, abbr: &str| {
        s[table].as_array().unwrap().iter().find(|r| r["abbr"] == json!(abbr)).map(|r| dec(&r["amount"])).unwrap_or_default()
    };
    // Priya: 12% of the 15,000 ceiling, not of her 75,000 basic; 3.25% of her gross.
    let s1 = slip_of(&priya).await;
    assert_eq!((amount(&s1, "employer_contributions", "EPF"), amount(&s1, "employer_contributions", "EINS")), (n("1800"), n("4875")));
    assert_eq!(
        (dec(&s1["gross_pay"]), dec(&s1["net_pay"]), dec(&s1["total_employer_contribution"])),
        (n("150000"), n("150000"), n("6675")),
        "never in gross or net pay"
    );
    assert!(s1["earnings"].as_array().unwrap().iter().chain(s1["deductions"].as_array().unwrap()).all(|r| r["abbr"] != json!("EPF")));
    // Rahul joined on the 16th: 12% of the 10,000 basic he earned, and half a month's insurance.
    let s2 = slip_of(&rahul).await;
    assert_eq!((amount(&s2, "employer_contributions", "EPF"), amount(&s2, "employer_contributions", "EINS")), (n("1200"), n("650")));
    assert_eq!((dec(&s2["gross_pay"]), dec(&s2["net_pay"]), dec(&s2["total_employer_contribution"])), (n("20000"), n("20000"), n("1850")));

    // Every contribution needs the account it is owed on.
    refused(t.k.run_action(&pm, "PayrollEntry", pe_id, "submit_salary_slips", json!({})).await, "set the account of salary component Employer Insurance");
    t.k.update(&pm, "SalaryComponent", uid(&eins), obj(json!({"accounts": [{"company": t.c(), "account": ins_owed["id"]}]})), None).await.unwrap();
    t.act(&pm, "PayrollEntry", pe_id, "submit_salary_slips", json!({})).await;
    // The cost debited (its own expense account, else Salaries), what is owed credited.
    assert_eq!((t.gl("Employer PF Cost").await, t.gl("Employer PF Payable").await), (n("3000"), n("-3000")));
    assert_eq!((t.gl("Insurance Payable").await, t.gl("Salaries").await), (n("-5525"), n("175525")), "170,000 of pay and 5,525 of insurance");
    assert_eq!(t.gl("Payroll Payable").await, n("-170000"), "net pay is what is paid, as before");
    let on_ops: Decimal = {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query_scalar(
            "select coalesce(sum(l.debit), 0) from journal_line l join e_account a on a.tenant_id = l.tenant_id and a.id = l.account_id \
             where l.tenant_id = $1 and a.account_name = 'Employer PF Cost' and l.cost_center_id = $2",
        )
        .bind(t.tenant)
        .bind(uid(&ops))
        .fetch_one(&mut *tx)
        .await
        .unwrap()
    };
    assert_eq!(on_ops, n("1800"), "Priya's share on her cost centre");

    // The register shows them in their own columns, after the deductions.
    let reg = t.report(&pm, "salary_register", &[("from_date", "2026-09-01"), ("to_date", "2026-09-30")]).await.expect("register");
    assert_eq!((dec(&reg["totals"]["c_EPF"]), dec(&reg["totals"]["c_EINS"]), dec(&reg["totals"]["gross_pay"])), (n("3000"), n("5525"), n("170000")));
    let keys: Vec<&str> = reg["columns"].as_array().unwrap().iter().map(|c| c["key"].as_str().or(c["name"].as_str()).unwrap_or_default()).collect();
    let at = |k: &str| keys.iter().position(|c| *c == k).unwrap_or_else(|| panic!("{k} in {keys:?}"));
    assert!(at("c_SA") < at("c_EINS") && at("c_EPF") < at("gross_pay"), "{keys:?}");

    // Cancelled, the run takes its contributions back with it.
    t.k.cancel(&pm, "PayrollEntry", pe_id, None).await.expect("cancel run");
    for a in ["Employer PF Cost", "Employer PF Payable", "Insurance Payable", "Salaries", "Payroll Payable"] {
        assert_eq!(t.gl(a).await, Decimal::ZERO, "{a}");
    }
}

// ───────────────────────────── opening figures, a leaver's tax ─────────────────────────────

/// Opening year-to-date figures for a company moving here mid-year, and for a joiner with a
/// previous employer's income: income tax counts them, they never count twice, and the year's tax
/// is the same as if the whole year had been made here. A leaver's last slip settles the year.
async fn opening_scenario() {
    let t = T::with("hropen", &["payroll"]).await;
    let pm = t.user("pm@hropen.test", &["Payroll Manager"]).await;
    let it = t.one("SalaryComponent", json!({"abbr": "IT"})).await;
    let bonus = t.one("SalaryComponent", json!({"abbr": "BON"})).await;
    let pay = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Pay", "abbr": "PAY", "component_type": "Earning", "amount_based_on_formula": true, "formula": "base", "depends_on_payment_days": true}),
        )
        .await;
    let st = t
        .ins_submit(
            &pm,
            "SalaryStructure",
            json!({"structure_name": "Monthly", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [{"salary_component": pay["id"]}],
                   "deductions": [{"salary_component": it["id"]}]}),
        )
        .await;
    let slab = t
        .ins(&pm, "IncomeTaxSlab", json!({"slab_name": "New Regime", "effective_from": "2026-04-01", "standard_tax_exemption_amount": "75000",
            "slabs": [{"from_amount": "0", "to_amount": "400000", "percent_deduction": "0"}, {"from_amount": "400000", "to_amount": "800000", "percent_deduction": "5"},
                      {"from_amount": "800000", "to_amount": "1200000", "percent_deduction": "10"}, {"from_amount": "1200000", "to_amount": "0", "percent_deduction": "15"}]}))
        .await;
    let fy = t.ins(&pm, "PayrollPeriod", json!({"period_name": "FY 2026-27", "company": t.c(), "start_date": "2026-04-01", "end_date": "2027-03-31"})).await;
    let hire = |first: &'static str, joined: &'static str| {
        let (t, pm, st, slab) = (&t, &pm, &st, &slab);
        async move {
            let e = t.employee(first, json!({"date_of_joining": joined})).await;
            let from = if joined > "2026-04-01" { joined } else { "2026-04-01" };
            t.ins_submit(
                pm,
                "SalaryStructureAssignment",
                json!({"employee": e["id"], "salary_structure": st["id"], "from_date": from, "base": "100000", "income_tax_slab": slab["id"]}),
            )
            .await;
            e
        }
    };
    let months: Vec<(String, String)> = (0..12)
        .map(|i| {
            let first = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap().checked_add_months(chrono::Months::new(i)).unwrap();
            let last = first.checked_add_months(chrono::Months::new(1)).unwrap() - chrono::Duration::days(1);
            (first.to_string(), last.to_string())
        })
        .collect();
    let slip = |e: &Doc, m: usize| {
        let (t, pm, e, (from, to)) = (&t, &pm, e["id"].clone(), months[m].clone());
        async move { t.ins_submit(pm, "SalarySlip", json!({"employee": e, "start_date": from, "end_date": to})).await }
    };
    let tax = |s: &Doc| s["deductions"].as_array().unwrap().iter().find(|r| r["abbr"] == json!("IT")).map(|r| dec(&r["amount"])).unwrap_or_default();

    // Asha's whole year is made here, with a bonus in November.
    let asha = hire("Asha", "2024-06-01").await;
    t.ins_submit(&pm, "AdditionalSalary", json!({"employee": asha["id"], "salary_component": bonus["id"], "amount": "300000", "payroll_date": "2026-11-20"}))
        .await;
    let mut hers = Vec::new();
    for m in 0..12 {
        hers.push(slip(&asha, m).await);
    }
    let (to_december, tax_to_december): (Decimal, Decimal) =
        hers[..9].iter().map(|s| (dec(&s["taxable_earnings"]), tax(s))).fold((Decimal::ZERO, Decimal::ZERO), |a, b| (a.0 + b.0, a.1 + b.1));

    // Ben's company moved here in January: his April to December comes as opening figures,
    // imported like other records. They may not reach into months already paid here.
    let ben = hire("Ben", "2024-06-01").await;
    let opening = |e: &Doc, up_to: &str| json!({"employee": e["id"], "payroll_period": fy["id"], "up_to_date": up_to, "taxable_earnings": money(to_december), "tax_deducted": money(tax_to_december)});
    refused(t.try_ins(&pm, "PayrollOpening", opening(&asha, "2026-12-31")).await, "already pays Asha Test here");
    refused(t.try_ins(&pm, "PayrollOpening", opening(&ben, "2027-04-30")).await, "is outside payroll period FY 2026-27");
    let (made, failed) = t.k.import_rows(&pm, "PayrollOpening", &[opening(&ben, "2026-12-31")]).await.expect("import");
    assert!(failed.is_empty(), "{failed:?}");
    t.submit(&pm, "PayrollOpening", made[0].as_str().unwrap().parse().unwrap()).await;
    refused(t.try_ins(&pm, "PayrollOpening", opening(&ben, "2026-11-30")).await, "already cover this payroll period");
    refused(
        t.try_ins(&pm, "SalarySlip", json!({"employee": ben["id"], "start_date": "2026-12-01", "end_date": "2026-12-31"})).await,
        "already count Ben Test's pay up to 2026-12-31",
    );
    // January to March here: each month's tax, and so the year's, agrees to the cent with Asha's.
    let mut year_ben = tax_to_december;
    for m in 9..12 {
        let s = slip(&ben, m).await;
        assert_eq!(tax(&s), tax(&hers[m]), "{}", months[m].0);
        year_ben += tax(&s);
    }
    let year_asha: Decimal = hers.iter().map(tax).sum();
    assert_eq!(year_ben, year_asha);
    assert!(year_asha > Decimal::ZERO);

    // Chen joins in October from an employer who paid him 600,000 and deducted 25,000 since April:
    // taxed on the year's 1,125,000 (after the standard 75,000) with credit for what was paid,
    // 52,500 less 25,000 over the six months left, not on his pay here alone.
    let chen = hire("Chen", "2026-10-01").await;
    let alone = t.ins(&pm, "SalarySlip", json!({"employee": chen["id"], "start_date": "2026-10-01", "end_date": "2026-10-31"})).await;
    assert_eq!(tax(&alone), n("1041.67"), "without what came before, 525,000 a year");
    t.k.delete(&pm, "SalarySlip", uid(&alone), None).await.unwrap();
    t.ins_submit(
        &pm,
        "PayrollOpening",
        json!({"employee": chen["id"], "payroll_period": fy["id"], "up_to_date": "2026-09-30", "previous_employer": "Old Co",
               "previous_employer_income": "600000", "previous_employer_tax": "25000"}),
    )
    .await;
    assert_eq!(tax(&slip(&chen, 6).await), n("4583.33"));

    // The year's income tax report counts the opening figures, on columns of their own: Ben's year
    // agrees to the cent with Asha's, made entirely here, and Chen's shows both employers.
    let year = t.report(&pm, "income_tax_deductions", &[("from_date", "2026-04-01"), ("to_date", "2027-03-31")]).await.expect("tax report");
    let (a, b, c) = (row(&year, "employee_name", "Asha Test"), row(&year, "employee_name", "Ben Test"), row(&year, "employee_name", "Chen Test"));
    let year_taxable: Decimal = hers.iter().map(|s| dec(&s["taxable_earnings"])).sum();
    assert_eq!((dec(&a["total_taxable"]), dec(&a["total_tax"]), a["slips"].clone()), (year_taxable, year_asha, json!(12)));
    assert_eq!((dec(&b["opening_taxable"]), dec(&b["opening_tax"]), b["slips"].clone()), (to_december, tax_to_december, json!(3)));
    assert_eq!((dec(&b["total_taxable"]), dec(&b["total_tax"])), (year_taxable, year_asha), "the same year, to the cent");
    assert_eq!((dec(&c["previous_employer_income"]), dec(&c["previous_employer_tax"])), (n("600000"), n("25000")));
    assert_eq!((dec(&c["taxable_earnings"]), dec(&c["income_tax"])), (n("100000"), n("4583.33")));
    assert_eq!((dec(&c["total_taxable"]), dec(&c["total_tax"])), (n("700000"), n("29583.33")), "both employers");
    assert_eq!(dec(&year["totals"]["total_tax"]), dec(&a["total_tax"]) + dec(&b["total_tax"]) + dec(&c["total_tax"]));

    // Dia leaves on 15 August, paid a bonus of 800,000 with her last pay: her last slip taxes the
    // year she actually had (400,000 to July, 48,387.10 and the bonus in August, less 75,000:
    // 57,338.71) less the 17,500 deducted, instead of spreading it over months she will not work.
    let dia = hire("Dia", "2024-06-01").await;
    for m in 0..4 {
        assert_eq!(tax(&slip(&dia, m).await), n("4375"));
    }
    t.ins_submit(&pm, "AdditionalSalary", json!({"employee": dia["id"], "salary_component": bonus["id"], "amount": "800000", "payroll_date": "2026-08-10"}))
        .await;
    t.ins_submit(
        &t.admin,
        "EmployeeSeparation",
        json!({"employee": dia["id"], "reason": "Resignation", "resignation_letter_date": "2026-07-16", "relieving_date": "2026-08-15"}),
    )
    .await;
    let last = slip(&dia, 4).await;
    assert_eq!((dec(&last["payment_days"]), last["final_settlement"].clone()), (n("15"), json!(true)));
    assert_eq!(tax(&last), n("39838.71"));
}

// ───────────────────────────── notice, leaving and the full and final statement ─────────────────────────────

/// Notice served in full, short, waived and ended early by the employer; leave taken while serving
/// it; a separation withdrawn; a death; gratuity either side of the minimum service; the statement
/// gathering it all, adjusted with a reason, posted and paid; and a workspace upgraded from when a
/// separation made its employee Left at once.
async fn notice_scenario() {
    let t = T::new("hrnotice").await;
    let hr = t.user("hr@hrnotice.test", &["HR Manager"]).await;
    let pm = t.user("pm@hrnotice.test", &["Payroll Manager"]).await;
    let acct = t.user("acct@hrnotice.test", &["Accounts Manager"]).await;
    // India's gratuity: 15 days' basic a year on a 26-day month, after five years.
    let rule = json!({"hr_notice_period_days": 30, "hr_gratuity_min_years": 5, "hr_gratuity_days_per_year": "15", "hr_gratuity_days_in_month": "26",
                      "hr_gratuity_pay_formula": "BS"});
    t.k.update(&t.admin, "Company", t.company, obj(rule), None).await.expect("the company's rules");
    let comp = |abbr: &str| {
        let t = &t;
        let abbr = abbr.to_string();
        async move { t.one("SalaryComponent", json!({"abbr": abbr})).await }
    };
    let (bs, hra, sa) = (comp("BS").await, comp("HRA").await, comp("SA").await);
    let st = t
        .ins_submit(
            &pm,
            "SalaryStructure",
            json!({"structure_name": "Simple", "company": t.c(), "payroll_frequency": "Monthly",
                   "earnings": [{"salary_component": bs["id"]}, {"salary_component": hra["id"]}, {"salary_component": sa["id"]}]}),
        )
        .await;
    let pl = t.one("LeaveType", json!({"leave_type_name": "Privilege Leave"})).await;
    let hire = |first: &'static str, extra: Value| {
        let (t, hr, pm, st, pl) = (&t, &hr, &pm, &st, &pl);
        async move {
            let e = t.employee(first, extra).await;
            t.ins_submit(
                pm,
                "SalaryStructureAssignment",
                json!({"employee": e["id"], "salary_structure": st["id"], "from_date": "2026-04-01", "base": "62000"}),
            )
            .await;
            t.ins_submit(
                hr,
                "LeaveAllocation",
                json!({"employee": e["id"], "leave_type": pl["id"], "from_date": "2026-01-01", "to_date": "2026-12-31", "new_leaves_allocated": "18"}),
            )
            .await;
            e
        }
    };
    let separate = |e: &Doc, reason: &str, given: &str, last: &str, extra: Value| {
        let mut v = obj(json!({"employee": e["id"], "reason": reason, "resignation_letter_date": given, "relieving_date": last}));
        v.extend(obj(extra));
        let t = &t;
        let hr = &hr;
        async move { t.ins_submit(hr, "EmployeeSeparation", Value::Object(v)).await }
    };
    let statement = |sep: &Doc| {
        let (t, hr, sep) = (&t, &hr, uid(sep));
        async move { t.get("FullAndFinalStatement", vid(&t.act(hr, "EmployeeSeparation", sep, "make_full_and_final", json!({})).await)).await }
    };
    let lines = |st: &Doc, table: &str| -> Vec<(String, Decimal)> {
        st[table].as_array().unwrap().iter().map(|r| (r["reference_type"].as_str().unwrap_or("").to_string(), dec(&r["amount"]))).collect()
    };
    let notice = |sep: &Doc| {
        (sep["notice_period_days"].clone(), sep["notice_served_days"].clone(), sep["notice_shortfall_days"].clone(), sep["shortfall_settlement"].clone())
    };

    // ───────────── served in full; withdrawn ─────────────
    let sam = hire("Sam", json!({})).await;
    let sep = separate(&sam, "Resignation", "2026-07-01", "2026-07-31", json!({})).await;
    assert_eq!(notice(&sep), (json!(30), json!(30), json!(0), Value::Null), "the company's 30 days, served");
    assert_eq!(t.get("Employee", uid(&sam)).await["status"], json!("Left"), "the last working day has passed");
    t.k.cancel(&hr, "EmployeeSeparation", uid(&sep), None).await.expect("withdrawn");
    let e = t.get("Employee", uid(&sam)).await;
    assert_eq!((e["status"].clone(), e["relieving_date"].clone()), (json!("Active"), Value::Null), "everything as it was");
    refused(
        t.try_ins(&hr, "EmployeeSeparation", json!({"employee": sam["id"], "resignation_letter_date": "2026-08-02", "relieving_date": "2026-08-01"})).await,
        "after the last working day",
    );

    // ───────────── served short, leave taken while serving it: the statement ─────────────
    // Nina owes 60 days' notice, resigns on 11 July and leaves on 20 August (40 days), having taken
    // two days of privilege leave on the 10th and 11th.
    let nina = hire("Nina", json!({"notice_period_days": 60})).await;
    let leave =
        t.ins(&hr, "LeaveApplication", json!({"employee": nina["id"], "leave_type": pl["id"], "from_date": "2026-08-10", "to_date": "2026-08-11"})).await;
    t.trans(&hr, "LeaveApplication", uid(&leave), "Approve").await.expect("leave approved");
    let sep = separate(&nina, "Resignation", "2026-07-11", "2026-08-20", json!({})).await;
    assert_eq!(notice(&sep), (json!(60), json!(40), json!(20), json!("Recover from Employee")));
    let fnf = statement(&sep).await;
    // A day's pay is August's 62,000 over its 31 days: 2,000.
    assert_eq!(lines(&fnf, "payables"), vec![("Salary Slip".to_string(), n("40000")), ("Leave Encashment".to_string(), n("32000"))], "{fnf:?}");
    assert_eq!(lines(&fnf, "receivables"), vec![("Notice Recovery".to_string(), n("40000"))]);
    assert_eq!(dec(&fnf["payables"][1]["quantity"]), n("16"), "18 less the two taken during notice");
    assert_eq!(dec(&fnf["net_payable"]), n("32000"));
    let slip = t.get("SalarySlip", fnf["payables"][0]["reference"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(
        (slip["start_date"].clone(), slip["end_date"].clone(), dec(&slip["payment_days"]), slip["docstatus"].clone()),
        (json!("2026-08-01"), json!("2026-08-31"), n("20"), json!(0))
    );
    // Adjusted, with a reason; the last pay follows its slip.
    let fid = uid(&fnf);
    let mut recv = fnf["receivables"].as_array().unwrap().clone();
    recv[0]["amount"] = json!("30000");
    refused(
        t.k.update(&hr, "FullAndFinalStatement", fid, obj(json!({"receivables": recv.clone()})), None).await,
        "say why Notice not served differs from the 40000 gathered",
    );
    recv[0]["reason"] = json!("Five days bought out by the new employer");
    let mut pays = fnf["payables"].as_array().unwrap().clone();
    pays[0]["amount"] = json!("41000");
    pays[0]["reason"] = json!("rounding");
    refused(t.k.update(&hr, "FullAndFinalStatement", fid, obj(json!({"payables": pays})), None).await, "is its salary slip's net pay");
    t.k.update(&hr, "FullAndFinalStatement", fid, obj(json!({"receivables": recv})), None).await.expect("adjusted");
    let fnf = t.act(&hr, "FullAndFinalStatement", fid, "get_outstanding", json!({})).await;
    assert_eq!(lines(&obj(fnf.clone()), "receivables"), vec![("Notice Recovery".to_string(), n("30000"))], "gathering again keeps the adjustment");
    assert_eq!(fnf["receivables"][0]["reason"], json!("Five days bought out by the new employer"));
    let slip_id: Uuid = fnf["payables"][0]["reference"].as_str().unwrap().parse().unwrap();
    assert_eq!(t.list(&t.admin, "SalarySlip", json!({"employee": nina["id"]})).await.len(), 1, "made again, not twice");

    // Submitted: the slip with it (its accrual posted), the leave used up; then paid.
    t.submit(&hr, "FullAndFinalStatement", fid).await;
    assert_eq!(t.get("SalarySlip", slip_id).await["docstatus"], json!(1));
    assert_eq!((t.gl("Salaries").await, t.gl("Payroll Payable").await), (n("40000"), n("-40000")));
    let balance = |e: &Doc| {
        let (t, e, pl) = (&t, e["id"].clone(), pl["id"].clone());
        async move { t.list(&t.admin, "LeaveLedgerEntry", json!({"employee": e, "leave_type": pl})).await.iter().map(|l| dec(&l["leaves"])).sum::<Decimal>() }
    };
    assert_eq!(balance(&nina).await, Decimal::ZERO, "18 allocated, 2 taken, 16 encashed");
    let bank = t.gl("Primary Bank").await;
    let je = t.act(&hr, "FullAndFinalStatement", fid, "make_payment_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&je)).await;
    assert_eq!(t.gl("Payroll Payable").await, Decimal::ZERO, "the last pay paid");
    assert_eq!((t.gl("Salaries").await, t.gl("Primary Bank").await), (n("42000"), bank - n("42000")), "40,000 + 32,000 − 30,000");
    refused(t.k.cancel(&hr, "EmployeeSeparation", uid(&sep), None).await, "settles this separation; cancel it first");
    // Unwound in order: the slip cancelled with the statement, the leave given back.
    t.k.cancel(&acct, "JournalEntry", vid(&je), None).await.expect("entry cancelled");
    t.k.cancel(&hr, "FullAndFinalStatement", fid, None).await.expect("statement cancelled");
    assert_eq!(t.get("SalarySlip", slip_id).await["docstatus"], json!(2));
    assert_eq!((t.gl("Salaries").await, t.gl("Payroll Payable").await), (Decimal::ZERO, Decimal::ZERO));
    assert_eq!(balance(&nina).await, n("16"));

    // ───────────── waived; ended early by the employer ─────────────
    let omar = hire("Omar", json!({})).await;
    let sep = separate(&omar, "Resignation", "2026-07-21", "2026-08-10", json!({"shortfall_settlement": "Waive"})).await;
    assert_eq!(notice(&sep), (json!(30), json!(20), json!(10), json!("Waive")));
    let fnf = statement(&sep).await;
    assert!(lines(&fnf, "receivables").is_empty() && lines(&fnf, "payables").iter().all(|l| l.0 != "Notice Pay"), "waived: neither way");
    let tara = hire("Tara", json!({})).await;
    let sep = separate(&tara, "Termination", "2026-08-01", "2026-08-11", json!({})).await;
    assert_eq!(notice(&sep), (json!(30), json!(10), json!(20), json!("Pay in Lieu of Notice")));
    let fnf = statement(&sep).await;
    assert!(lines(&fnf, "payables").contains(&("Notice Pay".to_string(), n("40000"))), "{:?}", lines(&fnf, "payables"));

    // ───────────── gratuity either side of five years; a death ─────────────
    let gratuity = |st: &Doc| lines(st, "payables").into_iter().find(|l| l.0 == "Gratuity").map(|l| l.1);
    let uma = hire("Uma", json!({"date_of_joining": "2021-08-21"})).await;
    let fnf = statement(&separate(&uma, "Resignation", "2026-07-20", "2026-08-19", json!({})).await).await;
    assert_eq!(gratuity(&fnf), None, "a day short of five years");
    let vik = hire("Vik", json!({"date_of_joining": "2021-08-10"})).await;
    let fnf = statement(&separate(&vik, "Resignation", "2026-07-20", "2026-08-19", json!({})).await).await;
    assert_eq!(gratuity(&fnf), Some(n("89423.08")), "31,000 × 15 / 26 × 5");
    let dev = hire("Dev", json!({"date_of_joining": "2023-01-01", "notice_period_days": 60})).await;
    let sep = separate(&dev, "Death", "2026-08-14", "2026-08-14", json!({})).await;
    assert_eq!(notice(&sep), (json!(0), json!(0), json!(0), Value::Null), "no notice is due");
    let fnf = statement(&sep).await;
    assert_eq!(gratuity(&fnf), Some(n("71538.46")), "the minimum waived; three years and seven months count as four");

    // ───────────── serving notice now ─────────────
    let today = chrono::Utc::now().date_naive();
    let day = |n: i64| (today + chrono::Duration::days(n)).to_string();
    t.user("ria@hrnotice.test", &["Employee"]).await;
    let casual = t.one("LeaveType", json!({"leave_type_name": "Casual Leave"})).await;
    let policy = t
        .ins(&hr, "LeavePolicy", json!({"title": "Monthly", "year_starts": "January", "leave_policy_details": [{"leave_type": casual["id"], "annual_allocation": "12", "accrual": "Monthly"}]}))
        .await;
    let ria = t.employee("Ria", json!({"user": "ria@hrnotice.test", "leave_policy": policy["id"]})).await;
    let sep = separate(&ria, "Resignation", &day(-10), &day(20), json!({})).await;
    let e = t.get("Employee", uid(&ria)).await;
    assert_eq!((e["status"].clone(), e["relieving_date"].clone()), (json!("Active"), json!(day(20))), "at work until the last working day");
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("upkeep");
    assert_eq!(t.get("Employee", uid(&ria)).await["status"], json!("Active"));
    let accrued = t.list(&t.admin, "LeaveAllocation", json!({"employee": ria["id"], "leave_policy": policy["id"]})).await;
    assert_eq!(accrued.len(), 1, "still accruing leave: the upkeep granted her this year's shares");
    let dir = call(&t, Some(&hr), "GET", "directory", &[], Value::Null).await.expect("directory");
    let her = dir["people"].as_array().unwrap().iter().find(|p| p["id"] == ria["id"]).expect("still in the directory");
    assert_eq!(her["leaving"], json!(day(20)), "shown as serving notice");
    // Time passes: the day after the last working day, the upkeep makes her Left and ends her sign-in.
    {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        for table in ["e_employee", "e_employee_separation"] {
            let (key, id) = if table == "e_employee" { ("id", uid(&ria)) } else { ("id", uid(&sep)) };
            sqlx::query(sqlx::AssertSqlSafe(format!("update {table} set relieving_date = $3::date where tenant_id = $1 and {key} = $2")))
                .bind(t.tenant)
                .bind(id)
                .bind(day(-1))
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }
    ferrum_app_hr::upkeep(&t.k, t.tenant).await.expect("upkeep");
    assert_eq!(t.get("Employee", uid(&ria)).await["status"], json!("Left"));
    assert!(t.k.login("hrnotice", "ria@hrnotice.test", PW).await.is_err(), "her sign-in ended");
    assert!(call(&t, Some(&hr), "GET", "directory", &[], Value::Null).await.unwrap()["people"].as_array().unwrap().iter().all(|p| p["id"] != ria["id"]));

    // ───────────── upgrading: those the old release made Left while serving notice ─────────────
    let old = t.employee("Olu", json!({})).await;
    separate(&old, "Resignation", &day(-5), &day(25), json!({})).await;
    let gone = t.employee("Gil", json!({})).await;
    separate(&gone, "Resignation", &day(-40), &day(-10), json!({})).await;
    {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query("update e_employee set status = 'Left' where tenant_id = $1 and id = $2").bind(t.tenant).bind(uid(&old)).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    let hr_package = ferrum_app_hr::packages().unwrap().into_iter().find(|p| p.manifest.package == "hr").unwrap();
    let fill = hr_package.manifest.backfills.iter().find(|b| b.name == "serving_notice").expect("the backfill");
    {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        let sql = format!("update e_employee t set {} where t.tenant_id = $1 and ({})", fill.set, fill.needs);
        let changed = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(t.tenant).execute(&mut *tx).await.unwrap().rows_affected();
        tx.commit().await.unwrap();
        assert_eq!(changed, 1, "Olu only");
    }
    assert_eq!((t.get("Employee", uid(&old)).await["status"].clone(), t.get("Employee", uid(&gone)).await["status"].clone()), (json!("Active"), json!("Left")));
}

// ───────────────────────────── corrections, off-cycle pay, held salaries ─────────────────────────────

/// One employee's slip corrected after the run was paid, up and down, and before it was paid; an
/// off-cycle bonus between two regular runs; a salary held through the next run and through a full
/// and final statement; a whole run with a corrected slip cancelled. The payroll payable account
/// agrees with what is owed at every step, and the bank file with the bank entry.
async fn corrections_scenario() {
    let t = T::new("hrfix").await;
    let pm = t.user("pm@hrfix.test", &["Payroll Manager"]).await;
    let hr = t.user("hr@hrfix.test", &["HR Manager"]).await;
    let acct = t.user("acct@hrfix.test", &["Accounts Manager"]).await;
    let comp = |abbr: &str| {
        let t = &t;
        let abbr = abbr.to_string();
        async move { t.one("SalaryComponent", json!({"abbr": abbr})).await }
    };
    let (bs, hra, sa, pf, it, bonus) = (comp("BS").await, comp("HRA").await, comp("SA").await, comp("PF").await, comp("IT").await, comp("BON").await);
    let account = |name: &str, root: &str| json!({"account_name": name, "company": t.c(), "root_type": root});
    let pf_acc = t.ins(&t.admin, "Account", account("Provident Fund Payable", "Liability")).await;
    let epf_owed = t.ins(&t.admin, "Account", account("Employer PF Payable", "Liability")).await;
    let epf_cost = t.ins(&t.admin, "Account", account("Employer PF Cost", "Expense")).await;
    let ops = t.ins(&t.admin, "CostCenter", json!({"cost_center_name": "Operations", "company": t.c()})).await;
    t.k.update(&pm, "SalaryComponent", uid(&pf), obj(json!({"accounts": [{"company": t.c(), "account": pf_acc["id"]}]})), None).await.unwrap();
    t.k.update(&pm, "SalaryComponent", uid(&it), obj(json!({"accounts": [{"company": t.c(), "account": t.account("TDS Payable").await}]})), None)
        .await
        .unwrap();
    let epf = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Employer Provident Fund", "abbr": "EPF", "component_type": "Employer Contribution", "amount_based_on_formula": true,
                   "formula": "min(BS * payment_days / working_days, 15000) * 0.12", "depends_on_payment_days": false,
                   "accounts": [{"company": t.c(), "account": epf_owed["id"], "expense_account": epf_cost["id"]}]}),
        )
        .await;
    let line = |c: &Doc| json!({"salary_component": c["id"]});
    let st = t
        .ins_submit(
            &pm,
            "SalaryStructure",
            json!({"structure_name": "Standard", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [line(&bs), line(&hra), line(&sa)],
                   "deductions": [line(&pf), line(&it)], "employer_contributions": [line(&epf)]}),
        )
        .await;
    let slab = t
        .ins(&pm, "IncomeTaxSlab", json!({"slab_name": "New Regime", "effective_from": "2026-04-01", "standard_tax_exemption_amount": "75000",
            "slabs": [{"from_amount": "0", "to_amount": "400000", "percent_deduction": "0"}, {"from_amount": "400000", "to_amount": "800000", "percent_deduction": "5"},
                      {"from_amount": "800000", "to_amount": "1200000", "percent_deduction": "10"}, {"from_amount": "1200000", "to_amount": "0", "percent_deduction": "15"}]}))
        .await;
    t.ins(&pm, "PayrollPeriod", json!({"period_name": "FY 2026-27", "company": t.c(), "start_date": "2026-04-01", "end_date": "2027-03-31"})).await;
    let hire = |first: &'static str, base: &'static str, extra: Value| {
        let (t, pm, st, slab) = (&t, &pm, &st, &slab);
        async move {
            let mut v = obj(json!({"bank_name": "HDFC", "bank_account_no": format!("00{}", first.len() * 1111), "ifsc_code": "HDFC0000001"}));
            v.extend(obj(extra));
            let e = t.employee(first, Value::Object(v)).await;
            t.ins_submit(
                pm,
                "SalaryStructureAssignment",
                json!({"employee": e["id"], "salary_structure": st["id"], "from_date": "2026-04-01", "base": base, "income_tax_slab": slab["id"]}),
            )
            .await;
            e
        }
    };
    let asha = hire("Asha", "150000", json!({"payroll_cost_center": ops["id"]})).await;
    let ben = hire("Ben", "100000", json!({})).await;
    let chen = hire("Chen", "80000", json!({})).await;
    // Dev earns what Ben does and is paid September's bonus with September, not after it.
    let dev = hire("Dev", "100000", json!({})).await;
    let extra = |e: &Doc, amount: &str, day: &str, off_cycle: bool| {
        let (t, pm) = (&t, &pm);
        let v = json!({"employee": e["id"], "salary_component": bonus["id"], "amount": amount, "payroll_date": day, "off_cycle": off_cycle,
                       "deduct_full_tax": false});
        async move { t.ins_submit(pm, "AdditionalSalary", v).await }
    };
    extra(&dev, "5000", "2026-09-30", false).await;

    let make_run = |start: &str, end: &str, off_cycle: bool| {
        let (t, pm) = (&t, &pm);
        let v = json!({"company": t.c(), "posting_date": end, "start_date": start, "end_date": end, "off_cycle": off_cycle});
        async move {
            let pe = t.ins(pm, "PayrollEntry", v).await;
            let pe = t.act(pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await;
            let id = vid(&pe);
            t.submit(pm, "PayrollEntry", id).await;
            t.act(pm, "PayrollEntry", id, "create_salary_slips", json!({})).await;
            id
        }
    };
    let submit_slips = |pe: Uuid| {
        let (t, pm) = (&t, &pm);
        async move { t.act(pm, "PayrollEntry", pe, "submit_salary_slips", json!({})).await }
    };
    let slip_of = |e: &Doc, start: &str| {
        let t = &t;
        let f = json!({"employee": e["id"], "start_date": start, "docstatus": 1, "off_cycle": false});
        async move { t.get("SalarySlip", uid(&t.one("SalarySlip", f).await)).await }
    };
    let amount = |s: &Doc, table: &str, abbr: &str| {
        s[table].as_array().unwrap().iter().find(|r| r["abbr"] == json!(abbr)).map(|r| dec(&r["amount"])).unwrap_or_default()
    };
    let net = |s: &Doc| dec(&s["net_pay"]);
    // The bank file's transfers by name, and its total; then the bank entry, posted.
    let file = |pe: Uuid| {
        let (t, pm) = (&t, &pm);
        async move {
            let f = t.act(pm, "PayrollEntry", pe, "bank_file", json!({"format": "CSV", "from_account": "50100012345678"})).await;
            let rows: HashMap<String, Decimal> = f["file"]["content"]
                .as_str()
                .unwrap()
                .lines()
                .skip(1)
                .map(|l| {
                    let c: Vec<&str> = l.split(',').collect();
                    (c[0].to_string(), n(c[5]))
                })
                .collect();
            (rows, dec(&f["total"]))
        }
    };
    let pay = |pe: Uuid| {
        let (t, pm, acct) = (&t, &pm, &acct);
        async move {
            let je = t.act(pm, "PayrollEntry", pe, "make_bank_entry", json!({})).await;
            let je = t.submit(acct, "JournalEntry", vid(&je)).await;
            je["accounts"].as_array().unwrap().iter().map(|l| dec(&l["debit"])).sum::<Decimal>()
        }
    };
    let payable = || t.gl("Payroll Payable");
    // What every live run slip still owes, which the payroll payable account holds.
    let owed = || async {
        let slips = t.list(&t.admin, "SalarySlip", json!({"docstatus": 1})).await;
        slips.iter().map(|s| dec(&s["net_pay"]) - dec(&s["paid_amount"])).sum::<Decimal>()
    };

    // ───────────── September, paid by bank ─────────────
    let sep = make_run("2026-09-01", "2026-09-30", false).await;
    submit_slips(sep).await;
    let (rows, total) = file(sep).await;
    assert_eq!(rows.len(), 4);
    assert_eq!(pay(sep).await, total, "the bank file pays what the bank entry pays");
    assert_eq!(payable().await, Decimal::ZERO);
    let (salaries, epf_paid) = (t.gl("Salaries").await, t.gl("Employer PF Payable").await);
    let (s_asha, s_ben, s_chen) = (slip_of(&asha, "2026-09-01").await, slip_of(&ben, "2026-09-01").await, slip_of(&chen, "2026-09-01").await);
    assert_eq!(dec(&s_ben["paid_amount"]), net(&s_ben));

    // ───────────── Ben's slip corrected upward after it was paid ─────────────
    refused(t.k.cancel(&pm, "SalarySlip", uid(&s_ben), None).await, "correct it (Correct)");
    let fix = t.act(&pm, "SalarySlip", uid(&s_ben), "make_correction", json!({})).await;
    assert_eq!(t.get("SalarySlip", uid(&s_ben)).await["docstatus"], json!(2));
    assert_eq!(payable().await, net(&s_ben), "his share taken back: what the bank paid him is owed back until the correction posts");
    extra(&ben, "5000", "2026-09-30", false).await;
    let fixed = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    assert_eq!((fixed["replaces"].clone(), t.get("SalarySlip", uid(&s_ben)).await["replaced_by"].clone()), (s_ben["id"].clone(), fixed["id"].clone()));
    assert_eq!((amount(&fixed, "earnings", "BON"), dec(&fixed["paid_amount"])), (n("5000"), net(&s_ben)));
    let owed_ben = net(&fixed) - net(&s_ben);
    assert!(owed_ben > Decimal::ZERO && owed_ben < n("5000"), "the bonus less its tax: {owed_ben}");
    assert_eq!(payable().await, -owed_ben, "still owed to him");
    assert_eq!(t.gl("Salaries").await, salaries + n("5000"));
    // As if the bonus had been paid with September: Dev's slip is the same.
    let s_dev = slip_of(&dev, "2026-09-01").await;
    for f in ["gross_pay", "taxable_earnings", "income_tax_deducted", "net_pay"] {
        assert_eq!(dec(&fixed[f]), dec(&s_dev[f]), "{f}");
    }
    // Nobody else is touched; the run's row for Ben is the correction.
    assert_eq!(
        (t.get("SalarySlip", uid(&s_asha)).await["version"].clone(), net(&t.get("SalarySlip", uid(&s_asha)).await)),
        (s_asha["version"].clone(), net(&s_asha))
    );
    let run = t.get("PayrollEntry", sep).await;
    let row_ben = run["employees"].as_array().unwrap().iter().find(|r| r["employee"] == ben["id"]).unwrap().clone();
    assert_eq!((row_ben["salary_slip"].clone(), run["status"].clone()), (fixed["id"].clone(), json!("Slips Submitted")));

    // ───────────── Chen's corrected downward: a day absent ─────────────
    t.ins_submit(&hr, "Attendance", json!({"employee": chen["id"], "attendance_date": "2026-09-15", "status": "Absent"})).await;
    let fix = t.act(&pm, "SalarySlip", uid(&s_chen), "make_correction", json!({})).await;
    let fixed_c = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    let excess = net(&s_chen) - net(&fixed_c);
    assert!(excess > Decimal::ZERO);
    assert_eq!((dec(&fixed_c["payment_days"]), dec(&fixed_c["paid_amount"])), (n("29"), net(&s_chen)));
    assert_eq!(payable().await, excess - owed_ben, "Chen owes his excess back; Ben is owed his difference");
    assert_eq!(payable().await, -owed().await);
    assert_eq!(t.gl("Employer PF Payable").await, epf_paid, "his employer's share is under its ceiling either way");

    // ───────────── October: Chen's excess taken from his pay, Ben's owed paid with it ─────────────
    let oct = make_run("2026-10-01", "2026-10-31", false).await;
    submit_slips(oct).await;
    let o_chen = slip_of(&chen, "2026-10-01").await;
    assert_eq!((dec(&o_chen["recovery_amount"]), dec(&o_chen["recovery_left"])), (excess, Decimal::ZERO));
    assert_eq!(dec(&o_chen["gross_pay"]) - dec(&o_chen["total_deduction"]), net(&o_chen), "shown as a deduction from his pay");
    assert_eq!(dec(&t.get("SalarySlip", uid(&fixed_c)).await["paid_amount"]), net(&fixed_c), "his corrected September is settled");
    // A correction before the run is paid: Asha's October, with a bonus she was due.
    let o_asha = slip_of(&asha, "2026-10-01").await;
    let fix = t.act(&pm, "SalarySlip", uid(&o_asha), "make_correction", json!({})).await;
    extra(&asha, "10000", "2026-10-31", false).await;
    let o_asha = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    assert_eq!((amount(&o_asha, "earnings", "BON"), dec(&o_asha["paid_amount"])), (n("10000"), Decimal::ZERO));
    assert_eq!(payable().await, -owed().await);
    let (rows, total) = file(oct).await;
    let o_ben = slip_of(&ben, "2026-10-01").await;
    assert_eq!(rows["Ben Test"], net(&o_ben) + owed_ben, "his October and what September still owed him");
    assert_eq!((rows["Chen Test"], rows["Asha Test"]), (net(&o_chen), net(&o_asha)));
    assert_eq!(pay(oct).await, total);
    assert_eq!(payable().await, Decimal::ZERO);
    for pe in [sep, oct] {
        assert_eq!(t.get("PayrollEntry", pe).await["status"], json!("Paid"));
    }
    refused(t.k.run_action(&pm, "PayrollEntry", sep, "make_bank_entry", json!({})).await, "nothing is left to pay");
    // October's income tax counts the corrected September, not the slip it replaced: Ben's is Dev's.
    let o_dev = slip_of(&dev, "2026-10-01").await;
    assert_eq!(amount(&o_ben, "deductions", "IT"), amount(&o_dev, "deductions", "IT"));
    assert!(amount(&o_ben, "deductions", "IT") > Decimal::ZERO);

    // ───────────── an off-cycle bonus between two regular runs ─────────────
    // Ben's 20,000 is paid on 10 November off-cycle, its full tax with it; Dev's with November's pay,
    // marked to have its full tax deducted then.
    extra(&ben, "20000", "2026-11-10", true).await;
    t.ins_submit(
        &pm,
        "AdditionalSalary",
        json!({"employee": dev["id"], "salary_component": bonus["id"], "amount": "20000", "payroll_date": "2026-11-30", "deduct_full_tax": true}),
    )
    .await;
    let off = make_run("2026-11-10", "2026-11-10", true).await;
    let off_run = t.get("PayrollEntry", off).await;
    assert_eq!((off_run["end_date"].clone(), off_run["employees"].as_array().unwrap().len()), (json!("2026-11-10"), 1), "only Ben has something named for it");
    submit_slips(off).await;
    let b_off = t.get("SalarySlip", uid(&t.one("SalarySlip", json!({"employee": ben["id"], "off_cycle": true})).await)).await;
    assert_eq!((b_off["start_date"].clone(), b_off["end_date"].clone(), dec(&b_off["gross_pay"])), (json!("2026-11-10"), json!("2026-11-10"), n("20000")));
    assert_eq!(b_off["earnings"].as_array().unwrap().len(), 1, "the bonus alone: no regular pay");
    assert!(b_off["employer_contributions"].as_array().unwrap().is_empty());
    let off_tax = amount(&b_off, "deductions", "IT");
    assert!(off_tax > Decimal::ZERO, "the tax the bonus brings, taken with it");
    let (rows, total) = file(off).await;
    assert_eq!((rows.len(), total), (1, net(&b_off)));
    assert_eq!(pay(off).await, total);
    assert_eq!(payable().await, Decimal::ZERO);

    // ───────────── November: Chen's salary held ─────────────
    let dia = hire("Dia", "60000", json!({})).await;
    let nov = make_run("2026-11-01", "2026-11-30", false).await;
    submit_slips(nov).await;
    let n_ben = slip_of(&ben, "2026-11-01").await;
    assert_eq!(amount(&n_ben, "earnings", "BON"), Decimal::ZERO, "the off-cycle bonus is not paid again");
    // The year's tax: Ben's off-cycle bonus and November together are Dev's November.
    let n_dev = slip_of(&dev, "2026-11-01").await;
    assert_eq!(off_tax + amount(&n_ben, "deductions", "IT"), amount(&n_dev, "deductions", "IT"));
    let reg = t.report(&pm, "salary_register", &[("from_date", "2026-11-01"), ("to_date", "2026-11-30")]).await.expect("register");
    assert_eq!(reg["rows"].as_array().unwrap().len(), 6, "five regular slips and the off-cycle one");
    let n_chen = slip_of(&chen, "2026-11-01").await;
    refused(t.k.run_action(&pm, "SalarySlip", uid(&n_chen), "hold_salary", json!({})).await, "reason");
    t.act(&pm, "SalarySlip", uid(&n_chen), "hold_salary", json!({"reason": "Bank account being changed"})).await;
    let held = t.get("SalarySlip", uid(&n_chen)).await;
    assert_eq!((held["held"].clone(), held["hold_reason"].clone()), (json!(true), json!("Bank account being changed")));
    let run = t.get("PayrollEntry", nov).await;
    assert_eq!(run["employees"].as_array().unwrap().iter().find(|r| r["employee"] == chen["id"]).unwrap()["held"], json!(true));
    let (rows, total) = file(nov).await;
    assert!(!rows.contains_key("Chen Test") && rows.len() == 4, "{rows:?}");
    assert_eq!(pay(nov).await, total);
    assert_eq!(payable().await, -net(&n_chen), "accrued, not paid");
    assert_eq!(t.get("PayrollEntry", nov).await["status"], json!("Slips Submitted"));
    refused(t.k.run_action(&pm, "PayrollEntry", nov, "make_bank_entry", json!({})).await, "Chen Test's salary is held");

    // ───────────── December: released, paid with December's bank payment; Dia's held as she leaves ─────────────
    let dec_run = make_run("2026-12-01", "2026-12-31", false).await;
    submit_slips(dec_run).await;
    t.act(&pm, "SalarySlip", uid(&n_chen), "release_salary", json!({})).await;
    let d_chen = slip_of(&chen, "2026-12-01").await;
    let d_dia = slip_of(&dia, "2026-12-01").await;
    t.act(&pm, "SalarySlip", uid(&d_dia), "hold_salary", json!({"reason": "Assets not returned"})).await;
    let (rows, total) = file(dec_run).await;
    assert_eq!(rows["Chen Test"], net(&n_chen) + net(&d_chen), "November released with December");
    assert!(!rows.contains_key("Dia Test"));
    assert_eq!(pay(dec_run).await, total);
    assert_eq!(payable().await, -net(&d_dia));
    assert_eq!(t.get("PayrollEntry", nov).await["status"], json!("Paid"));
    refused(t.k.run_action(&pm, "PayrollEntry", nov, "make_bank_entry", json!({})).await, "nothing is left to pay");
    // Dia leaves on 31 December: her statement pays the salary held.
    let sep_doc = t
        .ins_submit(
            &hr,
            "EmployeeSeparation",
            json!({"employee": dia["id"], "reason": "Resignation", "resignation_letter_date": "2026-12-01", "relieving_date": "2026-12-31", "shortfall_settlement": "Waive"}),
        )
        .await;
    let fnf = t.act(&hr, "EmployeeSeparation", uid(&sep_doc), "make_full_and_final", json!({})).await;
    let fnf = t.get("FullAndFinalStatement", vid(&fnf)).await;
    let lines: Vec<(String, Decimal)> =
        fnf["payables"].as_array().unwrap().iter().map(|r| (r["reference_type"].as_str().unwrap_or("").to_string(), dec(&r["amount"]))).collect();
    assert_eq!(lines, vec![("Payroll Salary".to_string(), net(&d_dia))], "{fnf:?}");
    t.submit(&hr, "FullAndFinalStatement", uid(&fnf)).await;
    let je = t.act(&hr, "FullAndFinalStatement", uid(&fnf), "make_payment_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&je)).await;
    assert_eq!(payable().await, Decimal::ZERO);
    assert_eq!(dec(&t.get("SalarySlip", uid(&d_dia)).await["paid_amount"]), net(&d_dia));
    assert_eq!(t.get("PayrollEntry", dec_run).await["status"], json!("Paid"), "paid by her statement");
    refused(t.k.run_action(&pm, "PayrollEntry", dec_run, "make_bank_entry", json!({})).await, "nothing is left to pay");

    // ───────────── a whole run with a corrected slip, cancelled ─────────────
    let before: Vec<Decimal> = {
        let mut v = Vec::new();
        for a in ["Salaries", "Provident Fund Payable", "TDS Payable", "Employer PF Payable", "Employer PF Cost", "Payroll Payable"] {
            v.push(t.gl(a).await);
        }
        v
    };
    let jan = make_run("2027-01-01", "2027-01-31", false).await;
    submit_slips(jan).await;
    let j_asha = slip_of(&asha, "2027-01-01").await;
    let fix = t.act(&pm, "SalarySlip", uid(&j_asha), "make_correction", json!({})).await;
    extra(&asha, "3000", "2027-01-31", false).await;
    let j_fixed = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    assert!(net(&j_fixed) > net(&j_asha));
    // Asha's pay stays on her cost centre through each correction.
    let on_ops: Decimal = {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query_scalar(
            "select coalesce(sum(l.debit - l.credit), 0) from journal_line l join e_account a on a.tenant_id = l.tenant_id and a.id = l.account_id \
             where l.tenant_id = $1 and a.account_name = 'Salaries' and l.cost_center_id = $2",
        )
        .bind(t.tenant)
        .bind(uid(&ops))
        .fetch_one(&mut *tx)
        .await
        .unwrap()
    };
    let hers: Decimal = t.list(&t.admin, "SalarySlip", json!({"employee": asha["id"], "docstatus": 1})).await.iter().map(|s| dec(&s["gross_pay"])).sum();
    assert_eq!(on_ops, hers);
    t.k.cancel(&pm, "PayrollEntry", jan, None).await.expect("cancel January");
    for (a, was) in ["Salaries", "Provident Fund Payable", "TDS Payable", "Employer PF Payable", "Employer PF Cost", "Payroll Payable"].iter().zip(before) {
        assert_eq!(t.gl(a).await, was, "{a}");
    }
    assert!(t.list(&t.admin, "SalarySlip", json!({"payroll_entry": jan.to_string(), "docstatus": 1})).await.is_empty());

    // ───────────── January again, paid by the company's own entry, not from here ─────────────
    extra(&ben, "4000", "2027-01-30", false).await;
    // Dev is paid with January the 1,000 Ben's correction adds.
    extra(&dev, "4000", "2027-01-30", false).await;
    extra(&dev, "1000", "2027-01-31", false).await;
    let jan = make_run("2027-01-01", "2027-01-31", false).await;
    submit_slips(jan).await;
    let jan_total = dec(&t.get("PayrollEntry", jan).await["total_net_pay"]);
    let elsewhere = |reference: &'static str| {
        let (t, pm) = (&t, &pm);
        async move { t.act(pm, "PayrollEntry", jan, "record_payment_elsewhere", json!({"posting_date": "2027-02-01", "reference": reference})).await }
    };
    elsewhere("JV-OLD-1").await;
    t.act(&pm, "PayrollEntry", jan, "undo_payment_elsewhere", json!({})).await;
    assert_eq!(t.get("PayrollEntry", jan).await["status"], json!("Slips Submitted"), "undone while nothing took it over");
    elsewhere("JV-2027-014").await;
    let run = t.get("PayrollEntry", jan).await;
    assert_eq!(
        (run["status"].clone(), run["paid_elsewhere_reference"].clone(), dec(&run["paid_elsewhere_amount"])),
        (json!("Paid"), json!("JV-2027-014"), jan_total)
    );
    assert_eq!(payable().await, -jan_total, "nothing is posted: the company's own entry pays it");
    refused(t.k.run_action(&pm, "PayrollEntry", jan, "make_bank_entry", json!({})).await, "recorded as paid elsewhere (JV-2027-014, 2027-02-01)");
    // The company's own entry.
    let own = t
        .ins(
            &acct,
            "JournalEntry",
            json!({"company": t.c(), "posting_date": "2027-02-01", "entry_type": "Journal Entry", "remark": "Salaries January, JV-2027-014",
                   "accounts": [{"account": t.account("Payroll Payable").await, "debit": money(jan_total)}, {"account": t.account("Primary Bank").await, "credit": money(jan_total)}]}),
        )
        .await;
    t.submit(&acct, "JournalEntry", uid(&own)).await;
    assert_eq!(payable().await, Decimal::ZERO);
    // Ben's January corrected after it: only the difference is owed.
    let j_ben = slip_of(&ben, "2027-01-01").await;
    let fix = t.act(&pm, "SalarySlip", uid(&j_ben), "make_correction", json!({})).await;
    extra(&ben, "1000", "2027-01-31", false).await;
    let j_fixed = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    assert_eq!(dec(&j_fixed["paid_amount"]), net(&j_ben), "what was paid elsewhere");
    let owed_jan = net(&j_fixed) - net(&j_ben);
    assert_eq!(payable().await, -owed_jan);
    refused(t.k.run_action(&pm, "PayrollEntry", jan, "undo_payment_elsewhere", json!({})).await, "can no longer be undone");

    // ───────────── February and March by bank: the year's tax agrees with Dev's ─────────────
    let feb = make_run("2027-02-01", "2027-02-28", false).await;
    submit_slips(feb).await;
    let (rows, total) = file(feb).await;
    assert_eq!(rows["Ben Test"], net(&slip_of(&ben, "2027-02-01").await) + owed_jan, "February and what January still owed him");
    assert_eq!(pay(feb).await, total);
    let mar = make_run("2027-03-01", "2027-03-31", false).await;
    submit_slips(mar).await;
    let total = file(mar).await.1;
    assert_eq!(pay(mar).await, total);
    assert_eq!(payable().await, Decimal::ZERO);
    let year = |e: &Doc| {
        let t = &t;
        let e = e["id"].clone();
        async move {
            let slips = t.list(&t.admin, "SalarySlip", json!({"employee": e, "docstatus": 1})).await;
            (slips.iter().map(|s| dec(&s["taxable_earnings"])).sum::<Decimal>(), slips.iter().map(|s| dec(&s["income_tax_deducted"])).sum::<Decimal>())
        }
    };
    let (ben_year, dev_year) = (year(&ben).await, year(&dev).await);
    assert_eq!(ben_year, dev_year, "the same pay, the same year's tax, to the cent");
    assert!(ben_year.1 > Decimal::ZERO);
}

// ───────────────────────────── arrears ─────────────────────────────

/// A revision backdated three months, for an employee paid all along and for one who joined in the
/// middle of the first of them; upward and downward, paid off-cycle and with the next slip, made
/// again, a period corrected after its arrears were paid, and a revision withdrawn before its arrears
/// were paid. The year's income tax agrees with the same pay given without a backdate.
async fn arrears_scenario() {
    let t = T::new("hrarr").await;
    let pm = t.user("pm@hrarr.test", &["Payroll Manager"]).await;
    let comp = |abbr: &str| {
        let t = &t;
        let abbr = abbr.to_string();
        async move { t.one("SalaryComponent", json!({"abbr": abbr})).await }
    };
    let (bs, hra, sa, it) = (comp("BS").await, comp("HRA").await, comp("SA").await, comp("IT").await);
    let account = |name: &str, root: &str| json!({"account_name": name, "company": t.c(), "root_type": root});
    let pf_acc = t.ins(&t.admin, "Account", account("Provident Fund Payable", "Liability")).await;
    let epf_acc = t.ins(&t.admin, "Account", account("Employer PF Payable", "Liability")).await;
    t.k.update(&pm, "SalaryComponent", uid(&it), obj(json!({"accounts": [{"company": t.c(), "account": t.account("TDS Payable").await}]})), None)
        .await
        .unwrap();
    let pf = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Provident Fund 12", "abbr": "PFX", "component_type": "Deduction", "amount_based_on_formula": true, "formula": "BS * 0.12",
                   "depends_on_payment_days": false, "exempted_from_income_tax": true, "accounts": [{"company": t.c(), "account": pf_acc["id"]}]}),
        )
        .await;
    let epf = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Employer PF 12", "abbr": "EPFX", "component_type": "Employer Contribution", "amount_based_on_formula": true,
                   "formula": "BS * 0.12", "depends_on_payment_days": false, "accounts": [{"company": t.c(), "account": epf_acc["id"]}]}),
        )
        .await;
    let line = |c: &Doc| json!({"salary_component": c["id"]});
    let st = t
        .ins_submit(
            &pm,
            "SalaryStructure",
            json!({"structure_name": "Standard", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [line(&bs), line(&hra), line(&sa)],
                   "deductions": [line(&pf), line(&it)], "employer_contributions": [line(&epf)]}),
        )
        .await;
    let slab = t
        .ins(&pm, "IncomeTaxSlab", json!({"slab_name": "New Regime", "effective_from": "2026-04-01", "standard_tax_exemption_amount": "75000",
            "slabs": [{"from_amount": "0", "to_amount": "400000", "percent_deduction": "0"}, {"from_amount": "400000", "to_amount": "800000", "percent_deduction": "5"},
                      {"from_amount": "800000", "to_amount": "1200000", "percent_deduction": "10"}, {"from_amount": "1200000", "to_amount": "0", "percent_deduction": "15"}]}))
        .await;
    t.ins(&pm, "PayrollPeriod", json!({"period_name": "FY 2026-27", "company": t.c(), "start_date": "2026-04-01", "end_date": "2027-03-31"})).await;
    let assign = |e: &Doc, from: &str, base: &str| {
        let (t, pm) = (&t, &pm);
        let v = json!({"employee": e["id"], "salary_structure": st["id"], "from_date": from, "base": base, "income_tax_slab": slab["id"]});
        async move { t.ins_submit(pm, "SalaryStructureAssignment", v).await }
    };
    let months: Vec<(String, String)> = (0..12)
        .map(|i| {
            let first = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap().checked_add_months(chrono::Months::new(i)).unwrap();
            let last = first.checked_add_months(chrono::Months::new(1)).unwrap() - chrono::Duration::days(1);
            (first.to_string(), last.to_string())
        })
        .collect();
    let draft = |e: &Doc, m: usize| {
        let (t, pm, e, (from, to)) = (&t, &pm, e["id"].clone(), months[m].clone());
        async move { t.ins(pm, "SalarySlip", json!({"employee": e, "start_date": from, "end_date": to})).await }
    };
    let slip = |e: &Doc, m: usize| {
        let (t, pm, e, (from, to)) = (&t, &pm, e["id"].clone(), months[m].clone());
        async move { t.ins_submit(pm, "SalarySlip", json!({"employee": e, "start_date": from, "end_date": to})).await }
    };
    let amount = |s: &Doc, table: &str, abbr: &str| {
        s[table].as_array().unwrap().iter().find(|r| r["abbr"] == json!(abbr)).map(|r| dec(&r["amount"])).unwrap_or_default()
    };
    let arrears = |s: &Doc| -> Vec<(String, String, Decimal)> {
        s["arrears"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["period_start"].as_str().unwrap().to_string(), r["abbr"].as_str().unwrap().to_string(), dec(&r["amount"])))
            .collect()
    };
    let tax = |s: &Doc| amount(s, "deductions", "IT");

    // Raj is paid 40,000 from April; in October his raise to 50,000 from July is entered. Tom had the
    // same raise entered on time.
    let raj = t.employee("Raj", json!({})).await;
    let tom = t.employee("Tom", json!({})).await;
    assign(&raj, "2026-04-01", "40000").await;
    assign(&tom, "2026-04-01", "40000").await;
    assign(&tom, "2026-07-01", "50000").await;
    let (mut year_raj, mut year_tom) = (Vec::new(), Vec::new());
    for m in 0..6 {
        year_raj.push(slip(&raj, m).await);
        year_tom.push(slip(&tom, m).await);
    }
    let salaries = t.gl("Salaries").await;
    let rev = assign(&raj, "2026-07-01", "50000").await;

    // Paid off-cycle on 10 October: each of July, August and September as paid (BS 20,000 → 25,000,
    // HRA 40% of it, SA the rest; the provident fund and the employer's share on BS).
    let off = |e: &Doc| {
        let (t, pm) = (&t, &pm);
        let v = json!({"employee": e["id"], "start_date": "2026-10-10", "off_cycle": true});
        async move { t.ins(pm, "SalarySlip", v).await }
    };
    let first = off(&raj).await;
    assert_eq!(first["end_date"], json!("2026-10-10"));
    let per = |abbr: &str, each: &str| -> Vec<(String, String, Decimal)> { (3..6).map(|m| (months[m].0.clone(), abbr.to_string(), n(each))).collect() };
    let mut want: Vec<(String, String, Decimal)> = [per("BS", "5000"), per("HRA", "2000"), per("SA", "3000"), per("PFX", "600"), per("EPFX", "600")].concat();
    want.sort();
    let mut got = arrears(&first);
    got.sort();
    assert_eq!(got, want);
    assert_eq!(
        (amount(&first, "earnings", "BS"), amount(&first, "deductions", "PFX"), amount(&first, "employer_contributions", "EPFX"), dec(&first["gross_pay"])),
        (n("15000"), n("1800"), n("1800"), n("30000"))
    );
    assert_eq!(first["arrears"][0]["salary_structure_assignment"], rev["id"]);
    // Made again: the same, once.
    t.k.delete(&pm, "SalarySlip", uid(&first), None).await.unwrap();
    let paid_off = t.submit(&pm, "SalarySlip", uid(&off(&raj).await)).await;
    let mut again = arrears(&paid_off);
    again.sort();
    assert_eq!(again, want);
    assert_eq!(t.gl("Salaries").await, salaries + n("30000") + n("1800"), "posted as pay, and the employer's share as its cost");
    assert_eq!(
        t.gl("Employer PF Payable").await,
        -(year_raj.iter().chain(year_tom.iter()).map(|s| amount(s, "employer_contributions", "EPFX")).sum::<Decimal>() + n("1800"))
    );
    // Taxed when paid: the tax the 28,200 more taxable brings to the year.
    assert_eq!(dec(&paid_off["taxable_earnings"]), n("28200"));
    assert!(tax(&paid_off) > Decimal::ZERO);
    refused(t.k.cancel(&pm, "SalaryStructureAssignment", uid(&rev), None).await, "computed from this assignment");

    // October and after pay none again; a period made again after its arrears were paid counts them.
    let oct = slip(&raj, 6).await;
    assert!(arrears(&oct).is_empty() && amount(&oct, "earnings", "BS") == n("25000"), "{:?}", arrears(&oct));
    let aug = year_raj[4].clone();
    t.k.cancel(&pm, "SalarySlip", uid(&aug), None).await.expect("August cancelled");
    let aug_again = slip(&raj, 4).await;
    assert_eq!(amount(&aug_again, "earnings", "BS"), n("20000"), "25,000 less the 5,000 paid as arrears");
    assert_eq!((dec(&aug_again["gross_pay"]), dec(&aug_again["net_pay"]) + tax(&aug_again)), (dec(&aug["gross_pay"]), dec(&aug["net_pay"]) + tax(&aug)));
    year_raj[4] = aug_again;
    year_raj.push(paid_off);
    year_raj.push(oct);
    let nov = slip(&raj, 7).await;
    assert!(arrears(&nov).is_empty(), "{:?}", arrears(&nov));
    year_raj.push(nov);
    for m in 6..12 {
        year_tom.push(slip(&tom, m).await);
    }
    for m in 8..12 {
        year_raj.push(slip(&raj, m).await);
    }
    // The year: the same pay, the same taxable income and the same tax as Tom's.
    let total = |list: &[Doc], f: &str| list.iter().map(|s| dec(&s[f])).sum::<Decimal>();
    assert_eq!((total(&year_raj, "gross_pay"), total(&year_raj, "taxable_earnings")), (total(&year_tom, "gross_pay"), total(&year_tom, "taxable_earnings")));
    assert_eq!(year_raj.iter().map(tax).sum::<Decimal>(), year_tom.iter().map(tax).sum::<Decimal>());
    assert!(year_tom.iter().map(tax).sum::<Decimal>() > Decimal::ZERO);

    // Ria joined on 16 July at 40,000. A raise to 45,000 is entered by mistake and withdrawn before
    // anything paid it; then her pay is lowered to 30,000 from 20 July, which counts for all of July
    // (as a slip made now for July would), on the 16 days she was paid for.
    let ria = t.employee("Ria", json!({"date_of_joining": "2026-07-16"})).await;
    assign(&ria, "2026-07-16", "40000").await;
    let july = slip(&ria, 3).await;
    assert_eq!((dec(&july["payment_days"]), amount(&july, "earnings", "BS")), (n("16"), n("10322.58")));
    slip(&ria, 4).await;
    slip(&ria, 5).await;
    let wrong = assign(&ria, "2026-07-20", "45000").await;
    let oct = draft(&ria, 6).await;
    assert_eq!(arrears(&oct).len(), 15, "three periods, five components");
    t.k.cancel(&pm, "SalaryStructureAssignment", uid(&wrong), None).await.expect("withdrawn");
    let oct = t.k.update(&pm, "SalarySlip", uid(&oct), obj(json!({})), None).await.unwrap();
    assert!(arrears(&oct).is_empty(), "nothing left behind");
    assign(&ria, "2026-07-20", "30000").await;
    let oct = t.submit(&pm, "SalarySlip", uid(&oct)).await;
    let bs_arrears: Vec<(String, Decimal)> = arrears(&oct).into_iter().filter(|a| a.1 == "BS").map(|a| (a.0, a.2)).collect();
    assert_eq!(
        bs_arrears,
        vec![("2026-07-01".to_string(), n("7741.94") - n("10322.58")), ("2026-08-01".to_string(), n("-5000")), ("2026-09-01".to_string(), n("-5000"))]
    );
    assert_eq!(amount(&oct, "earnings", "BS"), n("15000") - n("2580.64") - n("10000"), "October's 15,000 less what July to September paid too much");
    let row = oct["earnings"].as_array().unwrap().iter().find(|r| r["abbr"] == json!("BS")).unwrap().clone();
    assert_eq!(dec(&row["arrears_amount"]), n("-12580.64"));
    assert!(arrears(&slip(&ria, 7).await).is_empty());

    // An off-cycle run finds who has arrears due: Uma, whose raise from May is entered after June was
    // paid; not Raj or Ria, whose arrears are paid, nor Tom.
    let uma = t.employee("Uma", json!({})).await;
    assign(&uma, "2026-04-01", "40000").await;
    for m in 0..3 {
        slip(&uma, m).await;
    }
    assign(&uma, "2026-05-01", "50000").await;
    let pe = t.ins(&pm, "PayrollEntry", json!({"company": t.c(), "posting_date": "2026-07-10", "start_date": "2026-07-10", "off_cycle": true})).await;
    let pe = t.act(&pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await;
    let listed: Vec<Value> = pe["employees"].as_array().unwrap().iter().map(|r| r["employee"].clone()).collect();
    assert_eq!(listed, vec![uma["id"].clone()]);
    t.submit(&pm, "PayrollEntry", vid(&pe)).await;
    t.act(&pm, "PayrollEntry", vid(&pe), "create_salary_slips", json!({})).await;
    let hers = t.one("SalarySlip", json!({"employee": uma["id"], "off_cycle": true}));
    let hers = t.get("SalarySlip", uid(&hers.await)).await;
    assert_eq!(arrears(&hers).iter().filter(|a| a.1 == "BS").map(|a| a.2).sum::<Decimal>(), n("10000"), "May and June");
}

/// A run paid in part twice the old way (a bank entry for less than its net pay), upgraded: each
/// entry's share of the slips in the bank file's order, the rest paid after the upgrade exactly, and a
/// slip the first entry paid in part corrected for only its difference.
async fn upgrade_scenario() {
    let t = T::new("hrold").await;
    let pm = t.user("pm@hrold.test", &["Payroll Manager"]).await;
    let acct = t.user("acct@hrold.test", &["Accounts Manager"]).await;
    let flat = t
        .ins(
            &pm,
            "SalaryComponent",
            json!({"component_name": "Flat Pay", "abbr": "FLAT", "component_type": "Earning", "amount_based_on_formula": true,
            "formula": "base", "depends_on_payment_days": true, "is_tax_applicable": false}),
        )
        .await;
    let bonus = t.one("SalaryComponent", json!({"abbr": "BON"})).await;
    let st = t
        .ins_submit(
            &pm,
            "SalaryStructure",
            json!({"structure_name": "Flat", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [{"salary_component": flat["id"]}]}),
        )
        .await;
    let mut people = Vec::new();
    for (first, base) in [("Asha", "30000"), ("Ben", "20000"), ("Chen", "10000")] {
        let e = t.employee(first, json!({"bank_name": "HDFC", "bank_account_no": format!("00{base}"), "ifsc_code": "HDFC0000001"})).await;
        t.ins_submit(&pm, "SalaryStructureAssignment", json!({"employee": e["id"], "salary_structure": st["id"], "from_date": "2026-04-01", "base": base}))
            .await;
        people.push(e);
    }
    let pe = t.ins(&pm, "PayrollEntry", json!({"company": t.c(), "posting_date": "2026-09-30", "start_date": "2026-09-01", "end_date": "2026-09-30"})).await;
    let run = vid(&t.act(&pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await);
    t.submit(&pm, "PayrollEntry", run).await;
    t.act(&pm, "PayrollEntry", run, "create_salary_slips", json!({})).await;
    t.act(&pm, "PayrollEntry", run, "submit_salary_slips", json!({})).await;
    let (payable, bank) = (t.account("Payroll Payable").await, t.account("Primary Bank").await);
    // Two bank entries of the old kind: 35,000 then 8,000 of the run's 60,000.
    let mut old = Vec::new();
    for (day, amount) in [("2026-10-01", "35000"), ("2026-10-02", "8000")] {
        let je = t
            .ins_submit(
                &acct,
                "JournalEntry",
                json!({"company": t.c(), "posting_date": day, "entry_type": "Journal Entry", "remark": "Salaries",
                "accounts": [{"account": payable, "debit": amount}, {"account": bank, "credit": amount}]}),
            )
            .await;
        old.push(uid(&je));
    }
    {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        for je in &old {
            sqlx::query("update e_journal_entry set ext = ext || jsonb_build_object('hr_reference_type', 'Payroll Entry', 'hr_reference', $3::text) where tenant_id = $1 and id = $2")
                .bind(t.tenant)
                .bind(je)
                .bind(run.to_string())
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        sqlx::query("update e_payroll_entry set paid_amount = 43000 where tenant_id = $1 and id = $2")
            .bind(t.tenant)
            .bind(run)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("update e_salary_slip set paid_amount = null where tenant_id = $1").bind(t.tenant).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("../packages/payroll_erp/sql/001_salary_payments.sql"))).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    let shares: Vec<(Uuid, String, Decimal)> = {
        let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
        sqlx::query_as(
            "select p.source, s.employee_name, p.amount from pay_salary_payment p join e_salary_slip s on s.tenant_id = p.tenant_id and s.id = p.salary_slip \
             where p.tenant_id = $1 order by s.employee_name, p.amount desc",
        )
        .bind(t.tenant)
        .fetch_all(&mut *tx)
        .await
        .unwrap()
    };
    assert_eq!(
        shares,
        vec![(old[0], "Asha Test".into(), n("30000")), (old[1], "Ben Test".into(), n("8000")), (old[0], "Ben Test".into(), n("5000"))],
        "in the bank file's order"
    );
    let slip_of = |e: &Doc| {
        let t = &t;
        let f = json!({"employee": e["id"], "docstatus": 1});
        async move { t.get("SalarySlip", uid(&t.one("SalarySlip", f).await)).await }
    };
    assert_eq!(dec(&slip_of(&people[1]).await["paid_amount"]), n("13000"));
    // After the upgrade, the rest exactly: Ben's 7,000 and Chen's 10,000.
    let f = t.act(&pm, "PayrollEntry", run, "bank_file", json!({"format": "CSV", "from_account": "50100012345678"})).await;
    assert_eq!((f["payments"].clone(), dec(&f["total"])), (json!(2), n("17000")));
    refused(t.k.run_action(&pm, "PayrollEntry", run, "make_bank_entry", json!({"amount": "10000"})).await, "pays what is due, 17000");
    let je = t.act(&pm, "PayrollEntry", run, "make_bank_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&je)).await;
    let pe = t.get("PayrollEntry", run).await;
    assert_eq!((pe["status"].clone(), dec(&pe["paid_amount"]), t.gl("Payroll Payable").await), (json!("Paid"), n("60000"), Decimal::ZERO));
    // Ben's slip, paid in part by the first old entry, corrected: only his 1,500 more is owed.
    let ben = slip_of(&people[1]).await;
    let fix = t.act(&pm, "SalarySlip", uid(&ben), "make_correction", json!({})).await;
    t.ins_submit(
        &pm,
        "AdditionalSalary",
        json!({"employee": people[1]["id"], "salary_component": bonus["id"], "amount": "1500", "payroll_date": "2026-09-30"}),
    )
    .await;
    let fixed = t.submit(&pm, "SalarySlip", vid(&fix)).await;
    assert_eq!((dec(&fixed["paid_amount"]), t.gl("Payroll Payable").await), (n("20000"), n("-1500")));
    let je = t.act(&pm, "PayrollEntry", run, "make_bank_entry", json!({})).await;
    let je = t.submit(&acct, "JournalEntry", vid(&je)).await;
    assert_eq!(je["accounts"].as_array().unwrap().iter().map(|l| dec(&l["debit"])).sum::<Decimal>(), n("1500"));
    assert_eq!(t.gl("Payroll Payable").await, Decimal::ZERO);
}

fn money(v: Decimal) -> String {
    v.to_string()
}
