//! Letters, the payslip and payroll files as document templates (Ferrum BD-21), against a real PostgreSQL
//! (`FERRUM_HR_TEST_DATABASE_URL`, reset once). Files are kept in memory, mail is captured with its attachments, and
//! no PDF service is configured, so pages are kept as HTML.

use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, ListQuery};
use ferrum_meta::Catalog;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock, Mutex};
use uuid::Uuid;

const PW: &str = "Correct-Horse-9";

static RT: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap());
static KERNEL: tokio::sync::OnceCell<Arc<Kernel>> = tokio::sync::OnceCell::const_new();
/// Mail sent: its recipient, its subject and each file attached (name, bytes).
static MAILED: Mutex<Vec<(String, String, Vec<(String, Vec<u8>)>)>> = Mutex::new(Vec::new());
static FILES: LazyLock<Arc<MemFiles>> = LazyLock::new(Default::default);

struct Mailer;

#[async_trait::async_trait]
impl ferrum_kernel::Mailer for Mailer {
    async fn send(&self, to: &str, subject: &str, _html: &str) -> ferrum_kernel::Result<()> {
        MAILED.lock().unwrap().push((to.into(), subject.into(), vec![]));
        Ok(())
    }
    fn can_attach(&self) -> bool {
        true
    }
    async fn send_with(&self, to: &str, subject: &str, _html: &str, attachments: &[ferrum_kernel::MailAttachment]) -> ferrum_kernel::Result<()> {
        MAILED.lock().unwrap().push((to.into(), subject.into(), attachments.iter().map(|a| (a.file_name.clone(), a.data.clone())).collect()));
        Ok(())
    }
}

#[derive(Default)]
struct MemFiles(Mutex<std::collections::HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl ferrum_kernel::files::FileStore for MemFiles {
    async fn put(&self, key: &str, data: &[u8]) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().insert(key.to_string(), data.to_vec());
        Ok(())
    }
    async fn get(&self, key: &str) -> ferrum_kernel::Result<Vec<u8>> {
        self.0.lock().unwrap().get(key).cloned().ok_or_else(|| Error::NotFound("object".into()))
    }
    async fn delete(&self, key: &str) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

async fn kernel() -> Arc<Kernel> {
    KERNEL
        .get_or_init(|| async {
            let url = std::env::var("FERRUM_HR_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_hr_test".into());
            let db = Db::connect(DbConfig { url, max_connections: 24, ..Default::default() }).await.expect("connect");
            sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
            let mut packages = ferrum_domain::packages().expect("platform packages");
            packages.extend(ferrum_app_erp::packages().expect("ERP's packages"));
            packages.extend(ferrum_studio::packages().expect("studio's packages"));
            packages.extend(ferrum_app_hr::packages().expect("app packages"));
            let catalog = Catalog::from_packages(packages).expect("catalog");
            let mut hooks = HookRegistry::default();
            ferrum_domain::register(&mut hooks);
            ferrum_app_erp::register(&mut hooks);
            ferrum_studio::register(&mut hooks);
            ferrum_app_hr::register(&mut hooks);
            for source in ferrum_app_erp::reports().into_iter().chain(ferrum_app_hr::reports()).chain(ferrum_studio::reports()) {
                ferrum_domain::reports::add_source(source);
            }
            let k = Kernel::boot(db, catalog, hooks, KernelSettings::default(), Some(Arc::new(Mailer))).await.expect("boot");
            k.set_file_store(FILES.clone());
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

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap_or_else(|| panic!("not an object: {v}"))
}

fn uid(doc: &Doc) -> Uuid {
    doc["id"].as_str().unwrap().parse().unwrap()
}

fn describe(e: &Error) -> String {
    match e {
        Error::Validation(f) => f.iter().map(|x| format!("{}: {}", x.field, x.message)).collect::<Vec<_>>().join("; "),
        other => other.to_string(),
    }
}

struct T {
    k: Arc<Kernel>,
    tenant: Uuid,
    admin: Ctx,
    company: Uuid,
}

impl T {
    async fn new(slug: &str, packages: &[&str]) -> T {
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
        t.k.update(&t.admin, "Currency", uid(&inr), obj(json!({"symbol": "₹", "precision": 2})), None).await.unwrap();
        let co = t
            .ins(&t.admin.clone(), "Company", json!({"company_name": format!("{slug} Pvt Ltd"), "abbr": slug.to_uppercase(), "default_currency": inr["id"]}))
            .await;
        t.company = uid(&co);
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

    async fn ins(&self, ctx: &Ctx, entity: &str, v: Value) -> Doc {
        self.k.insert(ctx, entity, obj(v)).await.unwrap_or_else(|e| panic!("insert {entity}: {}", describe(&e)))
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

    async fn act(&self, ctx: &Ctx, entity: &str, id: Uuid, action: &str, params: Value) -> Value {
        self.k.run_action(ctx, entity, id, action, params).await.unwrap_or_else(|e| panic!("{entity}.{action}: {}", describe(&e)))
    }

    async fn employee(&self, first: &str, extra: Value) -> Doc {
        let mut v = obj(json!({"first_name": first, "last_name": "Test", "company": self.c(), "date_of_joining": "2024-06-01"}));
        v.extend(obj(extra));
        // The administrator as the workspace is now: its own fields included.
        let admin = self.k.load_ctx(self.tenant, self.admin.user, None, "it".into()).await.expect("ctx");
        self.ins(&admin, "Employee", Value::Object(v)).await
    }

    /// The stored text of a file named by `/files/{id}`.
    fn file_text(&self, url: &str) -> String {
        let id = url.strip_prefix("/files/").unwrap_or(url);
        let key = format!("tenants/{}/{id}", self.tenant);
        String::from_utf8(FILES.0.lock().unwrap().get(&key).cloned().unwrap_or_else(|| panic!("no file {key}"))).unwrap()
    }

    async fn drain(&self) {
        while self.k.dispatch_outbox_once().await.unwrap_or(0) > 0 {}
    }
}

#[test]
fn letters_and_certificates() {
    run(letters_scenario());
}

async fn letters_scenario() {
    let t = T::new("hrletters", &["hr_erp", "payroll_erp"]).await;
    t.k.update(
        &t.admin,
        "Company",
        t.company,
        obj(json!({"acc_address_line1": "12 Example Road", "acc_city": "Pune", "country": "Nowhere", "acc_email": "hr@letters.test"})),
        None,
    )
    .await
    .unwrap();
    let hr = t.user("hr@hrletters.test", &["HR Manager"]).await;
    let clerk = t.user("clerk@hrletters.test", &["HR User"]).await;
    let pm = t.user("pm@hrletters.test", &["Payroll Manager"]).await;
    let eng = t.ins(&t.admin, "Designation", json!({"designation_name": "Engineer"})).await;
    let dept = t.ins(&t.admin, "Department", json!({"department_name": "Platform", "company": t.c()})).await;
    let me = t.user("priya@hrletters.test", &["Employee"]).await;
    let priya = t
        .employee(
            "Priya",
            json!({"user": "priya@hrletters.test", "personal_email": "priya.home@letters.test", "designation": eng["id"], "department": dept["id"]}),
        )
        .await;
    let priya_id = uid(&priya);

    // An appointment letter: one button, a numbered register entry, a file on the employee and among their documents.
    let out = t.act(&clerk, "Employee", priya_id, "issue_appointment_letter", json!({"send_mail": "Yes"})).await;
    let letters = t.list(&hr, "EmployeeLetter", json!({})).await;
    assert_eq!(letters.len(), 1, "{out}");
    let first = &letters[0];
    assert_eq!(first["number"], json!("APT-2026-27-0001"), "a series for the type, by financial year");
    assert_eq!(first["recipient_email"], json!("priya.home@letters.test"));
    assert_eq!(first["issued_by"], json!("clerk@hrletters.test"));
    let doc = t.one("EmployeeDocument", json!({"employee": priya_id.to_string()})).await;
    assert_eq!(doc["document_number"], json!("APT-2026-27-0001"));
    let page = t.file_text(doc["file"].as_str().unwrap());
    for want in ["APT-2026-27-0001", "Priya Test", "Engineer", "Platform", "June 1, 2024", "12 Example Road", "Pune", "Nowhere"] {
        assert!(page.contains(want), "{want} in {page}");
    }
    assert!(!page.contains("None") && !page.contains("acc_"), "{page}");
    // The employee finds it among their own documents.
    assert_eq!(t.list(&me, "EmployeeDocument", json!({})).await.len(), 1);
    t.drain().await;
    let mailed = MAILED.lock().unwrap().clone();
    let m = mailed.iter().find(|m| m.1.contains("APT-2026-27-0001")).unwrap_or_else(|| panic!("{mailed:?}"));
    assert_eq!(m.0, "priya.home@letters.test");
    assert!(m.2.len() == 1 && String::from_utf8_lossy(&m.2[0].1).contains("Priya Test"), "the file went with the mail");

    // Each type its own series; a second of the same type goes on.
    t.act(&hr, "Employee", priya_id, "issue_employment_certificate", json!({})).await;
    t.act(&hr, "Employee", priya_id, "issue_appointment_letter", json!({})).await;
    let numbers: Vec<String> = t.list(&hr, "EmployeeLetter", json!({})).await.iter().map(|l| l["number"].as_str().unwrap().to_string()).collect();
    for want in ["APT-2026-27-0001", "APT-2026-27-0002", "EMC-2026-27-0001"] {
        assert!(numbers.contains(&want.to_string()), "{want} in {numbers:?}");
    }
    // A workspace that renames the type or its prefix keeps the button working.
    let ty = t.one("EmployeeDocumentType", json!({"letter_code": "employment"})).await;
    t.k.update(&t.admin, "EmployeeDocumentType", uid(&ty), obj(json!({"type_name": "Certificate of employment", "letter_prefix": "CE"})), None).await.unwrap();
    t.act(&hr, "Employee", priya_id, "issue_employment_certificate", json!({})).await;
    assert!(t.list(&hr, "EmployeeLetter", json!({})).await.iter().any(|l| l["number"] == json!("CE-2026-27-0001")));
    assert_eq!(t.list(&t.admin, "EmployeeDocumentType", json!({"letter_code": "employment"})).await.len(), 1);

    // Who may issue which: an employee issues none; a clerk no salary certificate.
    let denied = t.k.run_action(&me, "Employee", priya_id, "issue_appointment_letter", json!({})).await;
    assert!(matches!(denied, Err(Error::Forbidden(_))), "{denied:?}");

    // A warning needs its reason and is HR's alone to see.
    assert!(t.k.run_action(&hr, "Employee", priya_id, "issue_warning_letter", json!({})).await.is_err());
    t.act(&hr, "Employee", priya_id, "issue_warning_letter", json!({"reason": "Late three times in a month"})).await;
    let all = t.list(&hr, "EmployeeLetter", json!({})).await;
    let warning = all
        .iter()
        .find(|l| l["number"] == json!("WRN-2026-27-0001"))
        .unwrap_or_else(|| panic!("the warning is in the register: {:?}", all.iter().map(|l| l["number"].clone()).collect::<Vec<_>>()));
    assert!(
        t.file_text(t.one("EmployeeDocument", json!({"document_number": "WRN-2026-27-0001"})).await["file"].as_str().unwrap()).contains("Late three times")
    );
    assert_eq!(warning["recipient_name"], json!("Priya Test"));
    assert_eq!(t.list(&me, "EmployeeDocument", json!({"document_number": "WRN-2026-27-0001"})).await.len(), 0, "the type shows warnings to HR only");

    // The employee asks for a certificate: HR is told.
    t.act(&me, "Employee", priya_id, "request_letter", json!({"letter": "Salary certificate", "note": "For a loan"})).await;

    // HR is told, through the message template, with who asked and for what.
    let sends = t.k.message_log(&hr, Some(("Employee", priya_id)), None, 50).await.unwrap();
    assert!(format!("{sends:?}").contains("hr.letter_requested"), "{sends:?}");

    // A salary certificate comes from the salary assignment: HR and payroll only, the amount in figures and words.
    let st = {
        let flat = t
            .ins(
                &t.admin,
                "SalaryComponent",
                json!({"component_name": "Flat", "abbr": "FLT", "component_type": "Earning", "amount": "100000", "depends_on_payment_days": false}),
            )
            .await;
        t.ins(
            &t.admin,
            "SalaryStructure",
            json!({"structure_name": "Flat", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [{"salary_component": flat["id"]}]}),
        )
        .await
    };
    let st = t.k.submit(&t.admin, "SalaryStructure", uid(&st), None).await.unwrap();
    let ssa = t
        .ins(
            &t.admin,
            "SalaryStructureAssignment",
            json!({"employee": priya_id, "salary_structure": st["id"], "from_date": "2026-04-01", "base": "1234567.50"}),
        )
        .await;
    let ssa = t.k.submit(&t.admin, "SalaryStructureAssignment", uid(&ssa), None).await.unwrap();
    let denied = t.k.run_action(&clerk, "SalaryStructureAssignment", uid(&ssa), "issue_salary_certificate", json!({})).await;
    assert!(matches!(denied, Err(Error::Forbidden(_))), "a clerk may not issue a salary certificate: {denied:?}");
    t.act(&pm, "SalaryStructureAssignment", uid(&ssa), "issue_salary_certificate", json!({})).await;
    let cert = t.one("EmployeeDocument", json!({"document_number": "SAL-2026-27-0001"})).await;
    let page = t.file_text(cert["file"].as_str().unwrap());
    for want in ["₹1,234,567.50", "one million two hundred thirty-four thousand five hundred sixty-seven rupees and fifty paise", "monthly", "Priya Test"] {
        assert!(page.contains(want), "{want} in {page}");
    }

    // A separation's relieving letter, and an offer's letter (to an applicant who is no employee yet).
    let sep = t
        .ins(
            &t.admin,
            "EmployeeSeparation",
            json!({"employee": priya_id, "reason": "Resignation", "resignation_letter_date": "2026-10-01", "relieving_date": "2026-11-30"}),
        )
        .await;
    let sep = t.k.submit(&t.admin, "EmployeeSeparation", uid(&sep), None).await.unwrap();
    t.act(&hr, "EmployeeSeparation", uid(&sep), "issue_relieving_letter", json!({"send_mail": "Yes"})).await;
    let rel = t.one("EmployeeDocument", json!({"document_number": "REL-2026-27-0001"})).await;
    let page = t.file_text(rel["file"].as_str().unwrap());
    for want in ["November 30, 2026", "October 1, 2026", "Priya Test", "Engineer", "June 1, 2024"] {
        assert!(page.contains(want), "{want} in {page}");
    }
    t.drain().await;
    assert!(
        MAILED.lock().unwrap().iter().any(|m| m.0 == "priya.home@letters.test" && m.1.contains("REL-2026-27-0001") && m.2.len() == 1),
        "a leaver's letter goes to their own address"
    );
    let opening = t.ins(&t.admin, "JobOpening", json!({"job_title": "Engineer", "company": t.c(), "designation": eng["id"]})).await;
    let applicant = t.ins(&t.admin, "JobApplicant", json!({"applicant_name": "Kiran Rao", "email_id": "kiran@mail.test", "job_opening": opening["id"]})).await;
    let offer = t.ins(&t.admin, "JobOffer", json!({"job_applicant": applicant["id"], "company": t.c(), "designation": eng["id"], "offer_date": "2026-11-10", "expected_joining_date": "2026-12-01", "ctc": "1800000", "terms": "Probation of three months."})).await;
    let offer = t.k.submit(&t.admin, "JobOffer", uid(&offer), None).await.unwrap();
    t.act(&hr, "JobOffer", uid(&offer), "issue_offer_letter", json!({"send_mail": "Yes"})).await;
    let register = t.list(&hr, "EmployeeLetter", json!({})).await;
    let o = register.iter().find(|l| l["number"] == json!("OFR-2026-27-0001")).expect("the offer letter is in the register");
    assert_eq!((o["recipient_name"].clone(), o["employee"].clone()), (json!("Kiran Rao"), Value::Null));
    t.drain().await;
    let mailed = MAILED.lock().unwrap().clone();
    let m = mailed.iter().find(|m| m.1.contains("OFR-2026-27-0001")).expect("the offer was mailed");
    assert_eq!(m.0, "kiran@mail.test");
    let text = String::from_utf8_lossy(&m.2[0].1).to_string();
    assert!(text.contains("Kiran Rao") && text.contains("1,800,000.00") && text.contains("Probation of three months."), "{text}");

    // Serial numbers under concurrency: every letter its own number, none twice, none skipped.
    let mut staff = vec![];
    for i in 0..6 {
        staff.push(uid(&t.employee(&format!("Dev{i}"), json!({"designation": eng["id"]})).await));
    }
    let mut jobs = vec![];
    for id in staff {
        let (k, hr) = (t.k.clone(), hr.clone());
        jobs.push(tokio::spawn(async move { k.run_action(&hr, "Employee", id, "issue_employment_certificate", json!({})).await }));
    }
    for j in jobs {
        j.await.unwrap().expect("issued");
    }
    let mut serials: Vec<String> = t
        .list(&hr, "EmployeeLetter", json!({}))
        .await
        .iter()
        .filter_map(|l| l["number"].as_str().map(str::to_string))
        .filter(|n| n.starts_with("CE-"))
        .collect();
    serials.sort();
    let want: Vec<String> = (1..=7).map(|i| format!("CE-2026-27-{i:04}")).collect();
    assert_eq!(serials, want, "gapless, never twice");
}

#[test]
fn letters_without_erp_and_edited_by_a_workspace() {
    run(alone_letters_scenario());
}

/// No letterhead fields without ERP: the letters still come out, and a workspace can edit each template (a save checks
/// every body against a record with every field, so one that named a field only ERP adds would be refused).
async fn alone_letters_scenario() {
    let t = T::new("hrnoerp", &["payroll"]).await;
    assert!(t.admin.schema.entity("Company").unwrap().field("acc_logo").is_none());
    let hr = t.user("hr@hrnoerp.test", &["HR Manager"]).await;
    let ann = t.employee("Ann", json!({"personal_email": "ann@home.test"})).await;
    t.act(&hr, "Employee", uid(&ann), "issue_appointment_letter", json!({})).await;
    let doc = t.one("EmployeeDocument", json!({"employee": uid(&ann).to_string()})).await;
    let page = t.file_text(doc["file"].as_str().unwrap());
    assert!(page.contains("Ann Test") && page.contains("hrnoerp Pvt Ltd") && !page.contains("None"), "{page}");

    // Every letter template can be saved as the workspace's own copy, and the copy is what the next letter uses.
    for key in
        ["hr.offer_letter", "hr.appointment_letter", "hr.employment_certificate", "hr.warning_letter", "hr.relieving_letter", "payroll.salary_certificate"]
    {
        let read = t.k.document_template(&t.admin, key).await.unwrap();
        let def: ferrum_meta::DocumentDef = serde_json::from_value(read["template"].clone()).unwrap();
        t.k.save_document_template(&t.admin, &def).await.unwrap_or_else(|e| panic!("{key}: {}", describe(&e)));
    }
    let read = t.k.document_template(&t.admin, "hr.appointment_letter").await.unwrap();
    let mut def: ferrum_meta::DocumentDef = serde_json::from_value(read["template"].clone()).unwrap();
    def.languages.get_mut("en").unwrap().body = "<p>Welcome {{ doc.first_name }}, ref {{ values.serial }}</p>".into();
    t.k.save_document_template(&t.admin, &def).await.unwrap();
    t.act(&hr, "Employee", uid(&ann), "issue_appointment_letter", json!({})).await;
    let docs = t.list(&t.admin, "EmployeeDocument", json!({})).await;
    let newest = docs.iter().find(|d| d["document_number"] == json!("APT-2026-27-0002")).unwrap();
    assert_eq!(t.file_text(newest["file"].as_str().unwrap()), "<p>Welcome Ann, ref APT-2026-27-0002</p>");
}

/// A payroll run's submitted slips in a workspace with HR and payroll only, one slip per employee named.
async fn slips(t: &T, pm: &Ctx, names: &[(&str, Value)], (start, end): (&str, &str)) -> Vec<Doc> {
    let st = match t.list(&t.admin, "SalaryStructure", json!({"structure_name": "Flat"})).await.pop() {
        Some(st) => st,
        None => {
            let pen = t
                .ins(
                    &t.admin,
                    "SalaryComponent",
                    json!({"component_name": "Pension", "abbr": "PEN", "component_type": "Deduction", "amount": "1800", "depends_on_payment_days": false}),
                )
                .await;
            let flat = t
                .ins(
                    &t.admin,
                    "SalaryComponent",
                    json!({"component_name": "Flat Pay", "abbr": "FLAT", "component_type": "Earning", "amount": "30000", "depends_on_payment_days": false}),
                )
                .await;
            let st = t
                .ins(
                    &t.admin,
                    "SalaryStructure",
                    json!({"structure_name": "Flat", "company": t.c(), "payroll_frequency": "Monthly", "earnings": [{"salary_component": flat["id"]}], "deductions": [{"salary_component": pen["id"]}]}),
                )
                .await;
            t.k.submit(&t.admin, "SalaryStructure", uid(&st), None).await.unwrap()
        }
    };
    for (first, extra) in names {
        let e = t.employee(first, extra.clone()).await;
        let a = t
            .ins(&t.admin, "SalaryStructureAssignment", json!({"employee": e["id"], "salary_structure": st["id"], "from_date": "2026-04-01", "base": "30000"}))
            .await;
        t.k.submit(&t.admin, "SalaryStructureAssignment", uid(&a), None).await.unwrap();
    }
    let pe = t.ins(pm, "PayrollEntry", json!({"company": t.c(), "posting_date": end, "start_date": start, "end_date": end})).await;
    let pe = t.act(pm, "PayrollEntry", uid(&pe), "get_employees", json!({})).await;
    let pe_id: Uuid = pe["id"].as_str().unwrap().parse().unwrap();
    t.k.submit(pm, "PayrollEntry", pe_id, None).await.unwrap();
    t.act(pm, "PayrollEntry", pe_id, "create_salary_slips", json!({})).await;
    t.act(pm, "PayrollEntry", pe_id, "submit_salary_slips", json!({})).await;
    t.list(&t.admin, "SalarySlip", json!({"payroll_entry": pe_id.to_string()})).await
}

#[test]
fn the_payslip_as_a_document_template() {
    run(payslip_scenario());
}

async fn payslip_scenario() {
    let t = T::new("hrpayslip", &["payroll"]).await;
    let pm = t.user("pm@hrpayslip.test", &["Payroll Manager"]).await;
    let me = t.user("asha@hrpayslip.test", &["Employee"]).await;
    t.user("bo@hrpayslip.test", &["Employee"]).await;
    t.user("cy@hrpayslip.test", &["Employee"]).await;
    let got = slips(
        &t,
        &pm,
        &[
            ("Asha", json!({"user": "asha@hrpayslip.test", "bank_name": "Test Bank", "bank_account_no": "0011223344", "tax_id": "ABCDE1234F"})),
            ("<b>Bo</b>", json!({"user": "bo@hrpayslip.test"})),
        ],
        ("2026-09-01", "2026-09-30"),
    )
    .await;
    assert_eq!(got.len(), 2);
    let asha = got.iter().find(|s| s["employee_name"] == json!("Asha Test")).unwrap();
    let id = uid(asha);

    // The template has the print format's content, whoever reads it.
    let strip = |s: &str| {
        let i = s.find("Printed ").expect("the print's last line");
        s[..i].lines().map(str::trim_end).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
    };
    for who in [&pm, &me] {
        let printed = t.k.render_print(who, "SalarySlip", id).await.unwrap();
        let made = t.k.render_document(who, "payroll.payslip", id, None, false).await.unwrap();
        assert_eq!(strip(&String::from_utf8(made.bytes).unwrap()), strip(&printed), "the template prints what the print format printed");
    }

    // The two coexist: a print uses the print format (a workspace's customised one included) until a workspace
    // makes a copy of the template its default.
    assert_eq!(t.k.default_document(&pm, "SalarySlip").await.unwrap(), None);
    assert!(t.k.document_choices(&pm, "SalarySlip").await.unwrap()["templates"].to_string().contains("payroll.payslip"));
    let read = t.k.document_template(&t.admin, "payroll.payslip").await.unwrap();
    let mut def: ferrum_meta::DocumentDef = serde_json::from_value(read["template"].clone()).unwrap();
    t.k.save_document_template(&t.admin, &def).await.expect("a workspace edits it: every body passes the check, with no letterhead fields");
    def.default = true;
    t.k.save_document_template(&t.admin, &def).await.unwrap();
    assert_eq!(t.k.default_document(&pm, "SalarySlip").await.unwrap(), Some("payroll.payslip".into()));
    t.k.reset_document_template(&t.admin, "payroll.payslip").await.unwrap();
    assert_eq!(t.k.default_document(&pm, "SalarySlip").await.unwrap(), None);

    // Mailed and filed: the file on the slip, one document among the employee's, the account number masked in what leaves.
    t.act(&pm, "SalarySlip", id, "mail_payslip", json!({})).await;
    t.drain().await;
    let docs = t.list(&me, "EmployeeDocument", json!({})).await;
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["document_number"], asha["number"]);
    let page = t.file_text(docs[0]["file"].as_str().unwrap());
    assert!(page.contains("Asha Test") && page.contains("•••• 3344") && !page.contains("0011223344") && !page.contains("ABCDE1234F"), "{page}");
    let mailed = MAILED.lock().unwrap().clone();
    let m = mailed.iter().find(|m| m.0 == "asha@hrpayslip.test").expect("mailed to the employee");
    assert_eq!(m.2.len(), 1);
    assert!(String::from_utf8_lossy(&m.2[0].1).contains("Asha Test"));
    // Again: the same document, its file replaced.
    t.act(&pm, "SalarySlip", id, "mail_payslip", json!({"send_mail": "No"})).await;
    t.drain().await;
    let docs = t.list(&me, "EmployeeDocument", json!({})).await;
    assert_eq!(docs.len(), 1, "mailing again does not file it twice");
    assert_eq!(
        mailed.iter().filter(|m| m.0 == "asha@hrpayslip.test").count(),
        MAILED.lock().unwrap().iter().filter(|m| m.0 == "asha@hrpayslip.test").count(),
        "No: filed, not mailed"
    );
    assert!(t.file_text(docs[0]["file"].as_str().unwrap()).contains("Asha Test"));
    // The employee sees the file they were mailed.
    assert!(t.k.read_file(&me, docs[0]["file"].as_str().unwrap().strip_prefix("/files/").unwrap().parse().unwrap()).await.is_ok());

    // Every slip mailed when payroll releases them: a workspace's own automation on the slips' submission, the same effects.
    ferrum_kernel::automation::save(
        &t.k,
        &t.admin,
        &obj(json!({"name": "Mail payslips", "entity": "SalarySlip", "event": "submitted", "actions": [
            {"type": "generate", "template": "payroll.payslip", "name": "'Payslip ' + doc.number", "values": {"mask": "true"}},
            {"type": "message", "template": "payroll.payslip_mail", "to": "expr:coalesce(links.employee.user, links.employee.company_email)", "channels": ["email"], "attachments": ["results[0]"]}]})),
    )
    .await
    .expect("the automation");
    let before = MAILED.lock().unwrap().len();
    let later = slips(&t, &pm, &[("Cy", json!({"user": "cy@hrpayslip.test"}))], ("2026-10-01", "2026-10-31")).await;
    t.drain().await;
    t.drain().await;
    assert!(!later.is_empty());
    assert!(MAILED.lock().unwrap().len() > before, "released slips were mailed by the automation");
    let cy = MAILED.lock().unwrap().iter().find(|m| m.0 == "cy@hrpayslip.test").cloned().expect("mailed to Cy");
    assert!(cy.2.len() == 1 && String::from_utf8_lossy(&cy.2[0].1).contains("Cy Test") && !cy.1.is_empty());
}

#[test]
fn payroll_files_from_reports() {
    run(files_scenario());
}

async fn files_scenario() {
    let t = T::new("hrfiles", &["payroll", "studio"]).await;
    let pm = t.user("pm@hrfiles.test", &["Payroll Manager"]).await;
    t.user("asha@hrfiles.test", &["Employee"]).await;
    t.user("bo@hrfiles.test", &["Employee"]).await;
    // The examples install as two packs, reports first: a template about a report is checked against the report as it
    // is committed, which a single pack's own, not yet committed, report is not.
    for pack in [include_str!("../packs/payroll-file-examples-1-reports.pack.json"), include_str!("../packs/payroll-file-examples-2-templates.pack.json")] {
        let pack: Value = serde_json::from_str(pack).unwrap();
        let plan = t.k.plan_pack(&t.admin, &pack).await.unwrap_or_else(|e| panic!("plan: {}", describe(&e)));
        assert_eq!(plan["can_install"], json!(true), "{plan}");
        let done = t.k.install_pack(&t.admin, &pack, plan["digest"].as_str().unwrap()).await.unwrap_or_else(|e| panic!("install: {}", describe(&e)));
        assert!(done["failed"].as_array().is_none_or(|f| f.is_empty()), "{done}");
    }
    // A workspace's own employee field is an identifier like any other: a column in its report, a name in its template.
    t.k.add_customization(
        &t.admin,
        "Employee",
        ferrum_meta::OverlayOp::AddField {
            field: serde_json::from_value(json!({"name": "x_member_id", "type": "data", "label": "Member Id"})).unwrap(),
            after: None,
            column: false,
        },
        None,
    )
    .await
    .expect("a field of the workspace's own");
    let admin = t.k.load_ctx(t.tenant, t.admin.user, None, "it".into()).await.unwrap();
    let pm = t.k.load_ctx(t.tenant, pm.user, None, "it".into()).await.unwrap();
    t.k.insert(
        &admin,
        "Report",
        obj(json!({"label": "Members", "kind": "table", "sharing": "roles", "roles": [{"role": "Payroll Manager"}],
            "spec": {"entity": "SalarySlip", "filters": {"docstatus": 1}, "fields": [{"field": "employee"}, {"field": "employee.x_member_id"}, {"field": "net_pay"}],
                     "date_range": {"field": "start_date"}, "sort": [{"by": "employee.x_member_id"}]}})),
    )
    .await
    .expect("report");
    let own: ferrum_meta::DocumentDef =
        serde_json::from_value(json!({"key": "acme.members", "label": "Members file", "entity": "SalarySlip", "output": "text",
        "roles": ["Payroll Manager"], "query": {"report": "Members"},
        "languages": {"en": {"body": "{% for r in rows %}{{ r['employee.x_member_id']|pad(10) }}{{ r.net_pay|fixed(2) }}\n{% endfor %}"}}}))
        .unwrap();
    t.k.save_document_template(&admin, &own).await.unwrap_or_else(|e| panic!("{}", describe(&e)));
    let got = slips(
        &t,
        &pm,
        &[
            ("Asha", json!({"user": "asha@hrfiles.test", "x_member_id": "M-100", "bank_account_no": "0011223344"})),
            ("Bo", json!({"user": "bo@hrfiles.test", "x_member_id": "M-200"})),
        ],
        ("2026-09-01", "2026-09-30"),
    )
    .await;
    let run = |key: &str, from: &str, to: &str| {
        let (k, pm, key, from, to) = (t.k.clone(), pm.clone(), key.to_string(), from.to_string(), to.to_string());
        async move {
            k.render_query_document(
                &pm,
                &ferrum_kernel::documents::QueryRun {
                    template: key,
                    params: json!({"from": from, "to": to}),
                    values: obj(json!({"period": "2026-09"})),
                    language: None,
                },
            )
            .await
        }
    };
    let slip_count = got.len();
    assert_eq!(slip_count, 2);
    let audit_count = |template: &'static str| {
        let k = t.k.clone();
        let tenant = t.tenant;
        async move {
            let mut tx = k.db.tenant_tx(tenant).await.unwrap();
            let n: i64 = sqlx::query_scalar("select count(*) from audit_event where tenant_id = $1 and action = 'document:export' and diff->>'template' = $2")
                .bind(tenant)
                .bind(template)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            n
        }
    };

    // A CSV of net pay per employee, for the period asked for: one row each, the count and total from the template.
    let net = run("payfile.net_pay", "2026-09-01", "2026-09-30").await.unwrap_or_else(|e| panic!("{}", describe(&e)));
    assert_eq!(net.content_type, "text/csv; charset=utf-8");
    assert_eq!(
        String::from_utf8_lossy(&net.bytes),
        "employee_number,name,account_number,net_pay\nHR-EMP-00001,Asha Test,0011223344,28200.00\nHR-EMP-00002,Bo Test,,28200.00\ncount,2,,56400.00\n"
    );
    assert_eq!(audit_count("payfile.net_pay").await, 1, "a file of data is an export on the audit trail");
    let again = run("payfile.net_pay", "2026-09-01", "2026-09-30").await.unwrap();
    assert_eq!(again.bytes, net.bytes, "made again, the same file; nothing kept, so nothing twice");
    assert_eq!(audit_count("payfile.net_pay").await, 2);
    let none = run("payfile.net_pay", "2026-10-01", "2026-10-31").await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&none.bytes),
        "employee_number,name,account_number,net_pay\ncount,0,,0.00\n",
        "a period with no slips is a file with no rows"
    );

    // The fixed-width contributions file: a header, a line for each employee, a trailer with the count and the total.
    let c = run("payfile.contributions", "2026-09-01", "2026-09-30").await.unwrap_or_else(|e| panic!("{}", describe(&e)));
    let text = String::from_utf8(c.bytes.clone()).unwrap();
    assert!(text.ends_with("\r\n") && c.content_type == "text/plain; charset=ascii", "{:?}", c.content_type);
    let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
    let line = |number: &str, name: &str| format!("D{number}{name:<24}00000030000.00000001800.00");
    assert_eq!(
        lines,
        ["HEXAMPLE 2026-09000002".to_string(), line("HR-EMP-00001", "Asha Test"), line("HR-EMP-00002", "Bo Test"), "T00000200000003600.00".to_string()]
    );
    assert!(lines[1..3].iter().all(|l| l.len() == 63), "fixed width");
    // Whoever may not run payroll may not have it.
    let me = t.k.load_ctx(t.tenant, t.k.login("hrfiles", "asha@hrfiles.test", PW).await.unwrap().user, None, "it".into()).await.unwrap();
    let denied = t.k.render_query_document(&me, &ferrum_kernel::documents::QueryRun { template: "payfile.net_pay".into(), ..Default::default() }).await;
    assert!(matches!(denied, Err(Error::Forbidden(_))), "{denied:?}");

    // The workspace's own identifier reaches its own file.
    let own = run("acme.members", "2026-09-01", "2026-09-30").await.unwrap();
    assert_eq!(String::from_utf8_lossy(&own.bytes), "M-100     28200.00\nM-200     28200.00\n");

    // A payroll run chosen: the report's filters name it.
    let entry = got[0]["payroll_entry"].as_str().unwrap().to_string();
    let by_run =
        t.k.render_query_document(
            &pm,
            &ferrum_kernel::documents::QueryRun {
                template: "payfile.net_pay".into(),
                params: json!({"filters": {"payroll_entry": entry}}),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&by_run.bytes).ends_with("count,2,,56400.00\n"));
    let other =
        t.k.render_query_document(
            &pm,
            &ferrum_kernel::documents::QueryRun {
                template: "payfile.net_pay".into(),
                params: json!({"filters": {"payroll_entry": Uuid::nil().to_string()}}),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&other.bytes).ends_with("count,0,,0.00\n"), "another run's slips are not in it");

    // An annual statement for each employee, mailed to each: the button's job makes and sends them, once per employee.
    t.k.run_action(&admin, "Employee", uid(&t.one("Employee", json!({"first_name": "Asha"})).await), "x_send_pay_statements", Value::Null)
        .await
        .expect("queued");
    assert_eq!(t.k.run_jobs_once(1).await.unwrap(), 1);
    t.drain().await;
    let mailed = MAILED.lock().unwrap().clone();
    for who in ["asha@hrfiles.test", "bo@hrfiles.test"] {
        let m = mailed.iter().find(|m| m.0 == who && m.1.contains("pay statement")).unwrap_or_else(|| panic!("{who} was mailed: {mailed:?}"));
        let page = String::from_utf8_lossy(&m.2[0].1).to_string();
        assert!(page.contains("Statement of pay, 2026-04-01 to 2027-03-31") && page.contains("28,200.00") && page.contains("30,000.00"), "{page}");
    }
    let page = String::from_utf8_lossy(&mailed.iter().find(|m| m.0 == "bo@hrfiles.test").unwrap().2[0].1).to_string();
    assert!(page.contains("Bo Test") && !page.contains("Asha"), "each their own: {page}");
    // Run again: each employee's statement replaced, not added.
    t.k.run_action(&admin, "Employee", uid(&t.one("Employee", json!({"first_name": "Asha"})).await), "x_send_pay_statements", Value::Null).await.unwrap();
    assert_eq!(t.k.run_jobs_once(1).await.unwrap(), 1);
    let mut tx = t.k.db.tenant_tx(t.tenant).await.unwrap();
    let kept: i64 = sqlx::query_scalar("select count(*) from file_object where document = 'payfile.annual_statement'").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(kept, 2, "one statement each, the earlier one replaced");

    // Saving a template about a report once the report has rows: the check runs the report with a limit of one row, which
    // the platform reads as more than a document may hold (told in the README as a known fault, not relied on here).
    let edited: ferrum_meta::DocumentDef = serde_json::from_value(t.k.document_template(&admin, "payfile.net_pay").await.unwrap()["template"].clone()).unwrap();
    if let Err(e) = t.k.save_document_template(&admin, &edited).await {
        assert!(describe(&e).contains("more than 10000 rows"), "{}", describe(&e));
    }
}
