mod config;

use bdays::HolidayCalendar;
use cached::proc_macro::{cached, once};
use chrono::{Date, DateTime, Utc};
use std::convert::Infallible;
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

lazy_static::lazy_static! {
    pub static ref CONFIG: config::Config = config::Config::load();
}

mod ugh {
    use super::*;

    pub async fn dates_end() -> Result<impl warp::Reply, Infallible> {
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

        let d = dates_end(&CONFIG.start_date, &CONFIG.end_date);
        Ok(serde_json::to_string(&d).expect("failed to serialize dates"))
    }

    pub async fn index(accept: Option<String>) -> Result<impl warp::Reply, Infallible> {
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
            <pre>Round two!</pre>
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
                    .body(html.to_string())
                    .unwrap();
                return Ok(resp);
            }
        }

        // plain text response
        let now = Utc::now();
        let (days, hours, minutes, seconds) = timedelta(&now, &CONFIG.end_date);
        let bdays_left = count_business_days(now.date(), CONFIG.end_date.date());
        let bdays_done = count_business_days(CONFIG.start_date.date(), now.date());
        Ok(Response::new(format!("{days}d {hours}h {minutes}m {seconds}s\nbusiness days left: {bdays_left}\nbusiness days done: {bdays_done}\n")))
    }
}

#[tokio::main]
async fn main() {
    CONFIG.initialize();

    let filter = tracing_subscriber::filter::EnvFilter::new(&CONFIG.log_level);
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let status = warp::path("status").and(warp::get()).map(move || {
        #[derive(serde::Serialize)]
        struct Status<'a> {
            version: &'a str,
            ok: &'a str,
        }
        serde_json::to_string(&Status {
            version: &CONFIG.version,
            ok: "ok",
        })
        .expect("error serializing status")
    });

    let favicon = warp::path("favicon.ico")
        .and(warp::get())
        .and(warp::fs::file("static/think.jpg"));

    let localhost = warp::host::exact(&CONFIG.get_localhost_port())
        .or(warp::host::exact(&CONFIG.get_127_port()));
    let host_ugh = localhost.or(warp::host::exact("ugh.kominick.com"));

    let ugh_dates = warp::path!("dates" / "end")
        .and(host_ugh.clone())
        .and(warp::get())
        .and_then(move |_| async { ugh::dates_end().await });

    let ugh_index = warp::any()
        .and(warp::path::end())
        .and(host_ugh)
        .and(warp::header::optional::<String>("accept"))
        .and_then(move |_, accept: Option<String>| async { ugh::index(accept).await });

    let routes = ugh_index
        .or(ugh_dates)
        .or(favicon)
        .or(status)
        .with(warp::trace::request());

    let addr = CONFIG.get_host_port();
    warp::serve(routes)
        .run(
            addr.parse::<SocketAddr>()
                .map_err(|e| format!("invalid host/port: {addr}, {e}"))
                .unwrap(),
        )
        .await;
}
