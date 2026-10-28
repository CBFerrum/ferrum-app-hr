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
            packages.extend(ferrum_app_hr::packages().expect("app packages"));
            let catalog = Catalog::from_packages(packages).expect("catalog");
            let mut hooks = HookRegistry::default();
            ferrum_domain::register(&mut hooks);
            ferrum_app_hr::register(&mut hooks);
            for source in ferrum_app_hr::reports() {
                ferrum_domain::reports::add_source(source);
            }
            let k = Kernel::boot(db, catalog, hooks, KernelSettings::default(), None).await.expect("boot");
            ferrum_domain::maintenance(&k).await.expect("journal partitions");
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
    async fn new(slug: &str) -> T {
        let k = kernel().await;
        let tenant = k
            .create_tenant(NewTenant {
                slug: slug.into(),
                name: format!("{slug} Ltd"),
                admin_email: format!("admin@{slug}.test"),
                admin_password: PW.into(),
                admin_name: None,
                packages: ["core", "accounting", "hr", "payroll"].iter().map(|s| s.to_string()).collect(),
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
    ferrum_app_hr::maintenance(&t.k).await.expect("maintenance");
    let expired: Decimal =
        t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": p25["id"], "transaction_type": "Expiry"})).await.iter().map(|l| dec(&l["leaves"])).sum();
    assert_eq!(expired, n("-15"), "what was left of 2025 expired");
    // An allocation that ended recently with leaves left: the scheduler expires them.
    let today = chrono::Utc::now().date_naive();
    let (start, end) = ((today - chrono::Duration::days(100)).to_string(), (today - chrono::Duration::days(10)).to_string());
    let ended = t.ins_submit(&hr, "LeaveAllocation", alloc(uid(&pia), &sick, &start, &end, "5", false)).await;
    ferrum_app_hr::maintenance(&t.k).await.expect("maintenance");
    let lapsed: Vec<Doc> = t.list(&t.admin, "LeaveLedgerEntry", json!({"allocation": ended["id"], "transaction_type": "Expiry"})).await;
    assert_eq!((lapsed.len(), lapsed.first().map(|l| dec(&l["leaves"]))), (1, Some(n("-5"))), "unused leave expires at the end");
    let p26 = t.ins_submit(&hr, "LeaveAllocation", alloc(eid, &privilege, "2026-01-01", "2026-12-31", "18", true)).await;
    assert_eq!((dec(&p26["unused_leaves"]), dec(&p26["total_leaves_allocated"])), (n("15"), n("33")), "15 carried forward");
    ferrum_app_hr::maintenance(&t.k).await.expect("maintenance is idempotent");
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
    refused(t.try_ins(&hr, "EmployeeCheckin", punch(&b, "IN", "2026-09-02T09:00:00+05:30")).await, "duplicate");

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
    ferrum_app_hr::maintenance(&t.k).await.expect("maintenance");
    assert_eq!(att(&d, "2026-09-07").await["status"], json!("Absent"));
    assert!(t.list(&t.admin, "Attendance", json!({"employee": d["id"], "attendance_date": "2026-09-05"})).await.is_empty(), "weekly off");
    assert_eq!(t.get("ShiftType", uid(&general)).await["last_processed_date"], json!("2026-09-07"));
    assert_eq!(t.get("ShiftType", uid(&night)).await["last_processed_date"], json!("2026-09-06"), "the night of the 7th ends after the last sync");
    ferrum_app_hr::maintenance(&t.k).await.expect("maintenance again");
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
    let pay = t.act(&hr, "EmployeeAdvance", uid(&adv), "make_payment_entry", json!({})).await;
    assert_eq!(pay["docstatus"], json!(0), "a draft for accounts");
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
        .ins(&me, "ExpenseClaim", json!({"employee": e["id"], "expenses": [{"expense_date": "2026-10-20", "expense_type": food["id"], "amount": "300"}]}))
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
    assert!(masked.get("bank_account_no").is_none(), "HR users do not see bank details");

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
    let own_reg = t.report(&p1u, "salary_register", &[("from_date", "2026-09-01"), ("to_date", "2026-09-30")]).await.expect("own register");
    assert_eq!(own_reg["rows"].as_array().unwrap().len(), 1);

    // Paying, then unwinding in order.
    let bank_entry = t.act(&pm, "PayrollEntry", pe_id, "make_bank_entry", json!({})).await;
    t.submit(&acct, "JournalEntry", vid(&bank_entry)).await;
    let pe = t.get("PayrollEntry", pe_id).await;
    assert_eq!((pe["status"].clone(), dec(&pe["paid_amount"])), (json!("Paid"), n("192147.14")));
    assert_eq!(t.gl("Payroll Payable").await, Decimal::ZERO);
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
        "duplicate",
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
}
