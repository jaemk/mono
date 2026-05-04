use crate::CONFIG;
use axum::{
    body::Body,
    response::{IntoResponse, Response},
};
use bdays::HolidayCalendar;
use cached::proc_macro::cached;
use chrono::{DateTime, Datelike, Utc};

#[cached(time = 60, size = 5)]
pub fn count_business_days(mut start: chrono::NaiveDate, end: chrono::NaiveDate) -> i64 {
    tracing::debug!("calculating business days");
    let cal = bdays::calendars::us::USSettlement;
    let mut count = 0;
    while start < end {
        if cal.is_bday(start) {
            count += 1;
        }
        start = start.succ_opt().unwrap_or(start);
    }
    count
}

pub fn timedelta(start: &DateTime<Utc>, end: &DateTime<Utc>) -> (i64, i64, i64, i64) {
    let diff = end.signed_duration_since(*start);
    let seconds = diff.num_seconds();

    let minutes = seconds / 60;
    let seconds = seconds % 60;

    let hours = minutes / 60;
    let minutes = minutes % 60;

    let days = hours / 24;
    let hours = hours % 24;
    (days, hours, minutes, seconds)
}

pub async fn index() -> impl IntoResponse {
    let now = Utc::now();
    let (days, hours, minutes, seconds) = timedelta(&CONFIG.start_date, &now);
    let end_of_year = chrono::NaiveDate::from_ymd_opt(now.year(), 12, 31).unwrap();
    let bdays_left = count_business_days(now.date_naive(), end_of_year);
    let bdays_done = count_business_days(CONFIG.start_date.date_naive(), now.date_naive());
    Response::new(Body::from(format!(
        "business days left in year: {bdays_left}\nbusiness days done: {bdays_done}\ntenure: {days}d {hours}h {minutes}m {seconds}s\n"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    #[test]
    fn test_timedelta_logic() {
        let start = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2023, 1, 2, 1, 1, 1).unwrap();
        let (d, h, m, s) = timedelta(&start, &end);
        assert_eq!(d, 1);
        assert_eq!(h, 1);
        assert_eq!(m, 1);
        assert_eq!(s, 1);
    }

    #[test]
    fn test_business_days_logic() {
        let start = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(); // Sunday
        let end = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(); // Tuesday
                                                                // Jan 2 (Monday) is a holiday (New Year's observed) in US settlement calendar
                                                                // So business days between Jan 1 and Jan 3 should be 0 if Jan 2 is a holiday.
        let days = count_business_days(start, end);
        assert_eq!(days, 0);

        let start2 = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(); // Tuesday
        let end2 = NaiveDate::from_ymd_opt(2023, 1, 4).unwrap(); // Wednesday
        assert_eq!(count_business_days(start2, end2), 1);
    }

    #[test]
    fn test_timedelta_same_instant_returns_zeros() {
        let t = Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap();
        let (d, h, m, s) = timedelta(&t, &t);
        assert_eq!((d, h, m, s), (0, 0, 0, 0));
    }

    #[test]
    fn test_timedelta_exactly_one_hour() {
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 1, 11, 0, 0).unwrap();
        let (d, h, m, s) = timedelta(&start, &end);
        assert_eq!((d, h, m, s), (0, 1, 0, 0));
    }

    #[test]
    fn test_timedelta_exactly_one_day() {
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap();
        let (d, h, m, s) = timedelta(&start, &end);
        assert_eq!((d, h, m, s), (1, 0, 0, 0));
    }

    #[test]
    fn test_timedelta_complex_duration() {
        // 2 days, 3 hours, 4 minutes, 5 seconds
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 3, 3, 4, 5).unwrap();
        let (d, h, m, s) = timedelta(&start, &end);
        assert_eq!(d, 2);
        assert_eq!(h, 3);
        assert_eq!(m, 4);
        assert_eq!(s, 5);
    }

    #[test]
    fn test_business_days_same_date_is_zero() {
        let d = NaiveDate::from_ymd_opt(2023, 6, 12).unwrap(); // Monday
        assert_eq!(count_business_days(d, d), 0, "same start and end = 0 days");
    }

    #[test]
    fn test_business_days_full_week() {
        // Mon–Fri of a non-holiday week
        let start = NaiveDate::from_ymd_opt(2023, 6, 12).unwrap(); // Monday
        let end = NaiveDate::from_ymd_opt(2023, 6, 17).unwrap(); // Saturday
                                                                 // Mon, Tue, Wed, Thu, Fri = 5 business days
        assert_eq!(count_business_days(start, end), 5);
    }

    #[test]
    fn test_business_days_weekend_only() {
        let start = NaiveDate::from_ymd_opt(2023, 6, 10).unwrap(); // Saturday
        let end = NaiveDate::from_ymd_opt(2023, 6, 12).unwrap(); // Monday
                                                                 // Saturday and Sunday are not business days
        assert_eq!(count_business_days(start, end), 0);
    }
}
