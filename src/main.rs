use bdays::HolidayCalendar;
use cached::proc_macro::{cached, once};
use chrono::{Date, DateTime, Utc};
use std::net::SocketAddr;
use warp::{http::Response, Filter};

#[cached(time = 60, size = 5)]
fn count_business_days(mut start: Date<Utc>, end: Date<Utc>) -> i64 {
    tracing::debug!("calculating business days");
    let cal = bdays::calendars::us::USSettlement;
    let mut count = 0;
    while start < end {
        if cal.is_bday(start) {
            count += 1;
        }
        start = start.succ();
    }
    count
}

fn timedelta(start: &DateTime<Utc>, end: &DateTime<Utc>) -> (i64, i64, i64, i64) {
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

#[tokio::main]
async fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("PORT").unwrap_or_else(|_| "3003".to_string());
    let filter =
        std::env::var("LOG").unwrap_or_else(|_| "ugh=info,tracing=info,warp=info".to_owned());
    let start_date =
        std::env::var("START_DATE").unwrap_or_else(|_| "2021-01-01T00:00:00Z".to_string());
    let end_date = std::env::var("END_DATE").unwrap_or_else(|_| "2022-01-01T00:00:00Z".to_string());

    let addr = format!("{host}:{port}");
    let start_date = DateTime::parse_from_rfc3339(&start_date)
        .map_err(|e| format!("error parsing start date: {start_date}, {e}"))
        .unwrap()
        .with_timezone(&Utc);
    let end_date = DateTime::parse_from_rfc3339(&end_date)
        .map_err(|e| format!("error parsing end date: {end_date}, {e}"))
        .unwrap()
        .with_timezone(&Utc);

    let filter = tracing_subscriber::filter::EnvFilter::new(filter);
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let favicon = warp::path("favicon.ico")
        .and(warp::get())
        .and(warp::fs::file("static/think.jpg"));

    let dates = warp::path!("dates" / "end").and(warp::get()).map(move || {
        #[derive(serde::Serialize, PartialEq, Clone)]
        struct Dates {
            start: String,
            end: String,
            days_left: i64,
            business_days_left: i64,
            business_days_done: i64,
        }

        #[once(time = 30)]
        fn dates_end(start_date: &DateTime<Utc>, end_date: &DateTime<Utc>) -> Dates {
            tracing::debug!("calculating /dates/end info");
            let now = Utc::now();
            let (days_left, _, _, _) = timedelta(&now, end_date);
            let business_days_left = count_business_days(now.date(), end_date.date());
            let business_days_done = count_business_days(start_date.date(), now.date());
            Dates {
                start: start_date.to_rfc3339(),
                end: end_date.to_rfc3339(),
                days_left,
                business_days_left,
                business_days_done,
            }
        }

        let d = dates_end(&start_date, &end_date);
        serde_json::to_string(&d).unwrap()
    });

    let index = warp::any()
        .and(warp::path::end())
        .and(warp::header::optional::<String>("accept"))
        .map(move |accept: Option<String>| {
            if let Some(accept) = accept {
                if accept.to_lowercase().contains("text/html") {
                    let html = r##"
                        <html>
                            <meta charset="utf-8">
                            <meta name="viewport" content="width=device-width, initial-scale=1.0">
                            <head>
                                <meta name="viewport" content="width=device-width, initial-scale=1.0"/>
                                <title> how much longer? </title>
                            </head>
                            <body>
                                <div>
                                    <pre id="timer"></pre>
                                    <pre id="business_days_left"></pre>
                                    <pre id="business_days_done"></pre>
                                </div>
                            </body>
                            <script>
                                var timer = document.getElementById("timer");
                                var bdays_left = document.getElementById("business_days_left");
                                var bdays_done = document.getElementById("business_days_done");
                                var endDate = null;
                                function tick() {
                                    if (!endDate) { return; }
                                    var now = Date.parse(new Date().toISOString());
                                    var diff = endDate - now;
                                    var days = Math.floor(diff / (1000 * 60 * 60 * 24));
                                    var hours = Math.floor((diff % (1000 * 60 * 60 * 24)) / (1000 * 60 * 60));
                                    var minutes = Math.floor((diff % (1000 * 60 * 60)) / (1000 * 60));
                                    var seconds = Math.floor((diff % (1000 * 60)) / 1000);
                                    timer.innerHTML = days + "d " + hours + "h " + minutes + "m " + seconds + "s ";
                                    if (diff < 0) {
                                        clearInterval(x);
                                        timer.innerHTML = "MADE IT";
                                    }
                                }
                                setInterval(tick, 1000);

                                function refresh() {
                                    var r = new XMLHttpRequest();
                                    r.onreadystatechange = function() {
                                        if (r.readyState === XMLHttpRequest.DONE && r.status === 200) {
                                            var resp = JSON.parse(r.responseText);
                                            endDate = Date.parse(resp.end);
                                            if (resp.business_days_left <= 0) {
                                                bdays_left.innerHTML = "MADE IT";
                                            } else {
                                                bdays_left.innerHTML = resp.business_days_left + " business days left";
                                            }
                                            bdays_done.innerHTML = resp.business_days_done + " business days done";
                                            tick();
                                        }
                                    }
                                    r.open("GET", "/dates/end", true);
                                    r.send();
                                }
                                refresh();
                                setInterval(refresh, 1000 * 60 * 30);
                            </script>
                        </html>
                    "##;
                    let resp = Response::builder()
                        .header("Content-Type", "text/html")
                        .body(html.to_string()).unwrap();
                    return resp;
                }
            }

            // plain text response
            let now = Utc::now();
            let (days, hours, minutes, seconds) = timedelta(&now, &end_date);
            let bdays_left = count_business_days(now.date(), end_date.date());
            let bdays_done = count_business_days(start_date.date(), now.date());
            Response::new(format!("{days}d {hours}h {minutes}m {seconds}s\nbusiness days left: {bdays_left}\nbusiness days done: {bdays_done}\n"))
        });

    let routes = index.or(dates).or(favicon).with(warp::trace::request());

    tracing::info!(
        addr = %addr,
        start_date = %start_date.to_rfc3339(),
        end_date = %end_date.to_rfc3339(),
        "starting server",
    );
    warp::serve(routes)
        .run(
            addr.parse::<SocketAddr>()
                .map_err(|e| format!("invalid host/port: {addr}, {e}"))
                .unwrap(),
        )
        .await;
}
