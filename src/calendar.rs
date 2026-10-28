//! Dates: holidays, leave-day counting and working days. The counting is pure (unit-tested); the
//! loaders read holiday lists inside the caller's transaction.

use chrono::{NaiveDate, Weekday};
use ferrum_kernel::{Result, Tx};
use rust_decimal::Decimal;
use std::collections::BTreeSet;
use uuid::Uuid;

/// Every day from `from` to `to`, both included.
pub fn days(from: NaiveDate, to: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    from.iter_days().take_while(move |d| *d <= to)
}

/// Leave days between two dates (inclusive). Holidays and weekly offs inside the leave do not
/// count unless the leave type includes them; a half day counts 0.5 when it falls on a counted day.
pub fn leave_days(from: NaiveDate, to: NaiveDate, half_day: Option<NaiveDate>, holidays: &BTreeSet<NaiveDate>, include_holidays: bool) -> Decimal {
    let counted = |d: &NaiveDate| include_holidays || !holidays.contains(d);
    let mut total = Decimal::from(days(from, to).filter(counted).count() as u64);
    if let Some(h) = half_day
        && h >= from
        && h <= to
        && counted(&h)
    {
        total -= Decimal::new(5, 1);
    }
    total
}

/// Days in `[from, to]` that are not holidays.
pub fn working_days(from: NaiveDate, to: NaiveDate, holidays: &BTreeSet<NaiveDate>) -> i64 {
    days(from, to).filter(|d| !holidays.contains(d)).count() as i64
}

pub fn weekday(name: &str) -> Option<Weekday> {
    name.trim().parse::<Weekday>().ok()
}

pub fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// The holiday list that applies to an employee: their own, the shift being worked (if given),
/// their default shift's, else the company's default.
pub async fn holiday_list_for(tx: &mut Tx, tenant: Uuid, employee: Uuid, shift_list: Option<Uuid>) -> Result<Option<Uuid>> {
    let list: Option<Option<Uuid>> = sqlx::query_scalar(
        "select coalesce(e.holiday_list, $3, st.holiday_list, nullif(c.ext->>'hr_default_holiday_list', '')::uuid) from e_employee e \
         left join e_shift_type st on st.tenant_id = e.tenant_id and st.id = e.default_shift \
         left join e_company c on c.tenant_id = e.tenant_id and c.id = e.company where e.tenant_id = $1 and e.id = $2",
    )
    .bind(tenant)
    .bind(employee)
    .bind(shift_list)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(list.flatten())
}

/// Holidays of a list within `[from, to]`.
pub async fn holidays(tx: &mut Tx, tenant: Uuid, list: Option<Uuid>, from: NaiveDate, to: NaiveDate) -> Result<BTreeSet<NaiveDate>> {
    let Some(list) = list else { return Ok(BTreeSet::new()) };
    let days: Vec<NaiveDate> =
        sqlx::query_scalar("select holiday_date from e_holiday_list__holidays where tenant_id = $1 and parent_id = $2 and holiday_date between $3 and $4")
            .bind(tenant)
            .bind(list)
            .bind(from)
            .bind(to)
            .fetch_all(&mut **tx)
            .await?;
    Ok(days.into_iter().collect())
}

/// Holidays that apply to an employee within `[from, to]`.
pub async fn employee_holidays(tx: &mut Tx, tenant: Uuid, employee: Uuid, from: NaiveDate, to: NaiveDate) -> Result<BTreeSet<NaiveDate>> {
    let list = holiday_list_for(tx, tenant, employee, None).await?;
    holidays(tx, tenant, list, from, to).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn leave_skips_holidays_and_weekends_unless_included() {
        // Thu 2026-10-01 .. Tue 2026-10-06 with Gandhi Jayanti (Fri) and the weekend off.
        let hol: BTreeSet<NaiveDate> = ["2026-10-02", "2026-10-03", "2026-10-04"].iter().map(|s| d(s)).collect();
        assert_eq!(leave_days(d("2026-10-01"), d("2026-10-06"), None, &hol, false), Decimal::from(3));
        assert_eq!(leave_days(d("2026-10-01"), d("2026-10-06"), None, &hol, true), Decimal::from(6));
        // A half day on a counted day takes off half a day; on a holiday it changes nothing.
        assert_eq!(leave_days(d("2026-10-01"), d("2026-10-06"), Some(d("2026-10-05")), &hol, false), Decimal::new(25, 1));
        assert_eq!(leave_days(d("2026-10-01"), d("2026-10-06"), Some(d("2026-10-03")), &hol, false), Decimal::from(3));
        assert_eq!(leave_days(d("2026-10-05"), d("2026-10-05"), Some(d("2026-10-05")), &hol, false), Decimal::new(5, 1));
        assert_eq!(leave_days(d("2026-10-03"), d("2026-10-04"), None, &hol, false), Decimal::ZERO);
        assert_eq!(working_days(d("2026-10-01"), d("2026-10-31"), &hol), 28);
        assert_eq!(weekday("Sunday"), Some(Weekday::Sun));
        assert_eq!(weekday("Funday"), None);
    }
}
