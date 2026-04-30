use crate::CONFIG;
use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bdays::HolidayCalendar;
use cached::proc_macro::{cached, once};
use chrono::{DateTime, Utc};

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

pub async fn dates_end() -> impl IntoResponse {
    #[derive(serde::Serialize, PartialEq, Clone)]
    struct Dates {
        start: String,
        end: String,
        days_left: i64,
        business_days_left: i64,
        business_days_done: i64,
    }

    #[once(time = 30)]
    fn dates_end_calc(start_date: &DateTime<Utc>, end_date: &DateTime<Utc>) -> Dates {
        tracing::debug!("calculating /dates/end info");
        let now = Utc::now();
        let (days_left, _, _, _) = timedelta(&now, end_date);
        let business_days_left = count_business_days(now.date_naive(), end_date.date_naive());
        let business_days_done = count_business_days(start_date.date_naive(), now.date_naive());
        Dates {
            start: start_date.to_rfc3339(),
            end: end_date.to_rfc3339(),
            days_left,
            business_days_left,
            business_days_done,
        }
    }

    let d = dates_end_calc(&CONFIG.start_date, &CONFIG.end_date);
    axum::Json(d)
}

pub async fn index(headers: HeaderMap) -> impl IntoResponse {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    if let Some(accept) = accept {
        if accept.to_lowercase().contains("text/html") {
            match tokio::fs::read_to_string("static/ugh_index.html").await {
                Ok(html) => return ([(header::CONTENT_TYPE, "text/html")], html).into_response(),
                Err(e) => {
                    tracing::error!("error reading ugh_index.html: {:?}", e);
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
    }

    // plain text response
    let now = Utc::now();
    let (days, hours, minutes, seconds) = timedelta(&now, &CONFIG.end_date);
    let bdays_left = count_business_days(now.date_naive(), CONFIG.end_date.date_naive());
    let bdays_done = count_business_days(CONFIG.start_date.date_naive(), now.date_naive());
    Response::new(Body::from(format!(
        "{days}d {hours}h {minutes}m {seconds}s\nbusiness days left: {bdays_left}\nbusiness days done: {bdays_done}\n"
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
        let end = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap();   // Tuesday
        // Jan 2 (Monday) is a holiday (New Year's observed) in US settlement calendar
        // So business days between Jan 1 and Jan 3 should be 0 if Jan 2 is a holiday.
        let days = count_business_days(start, end);
        assert_eq!(days, 0);

        let start2 = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(); // Tuesday
        let end2 = NaiveDate::from_ymd_opt(2023, 1, 4).unwrap();   // Wednesday
        assert_eq!(count_business_days(start2, end2), 1);
    }
}
