use bdays::HolidayCalendar;
use cached::proc_macro::cached;
use chrono::{DateTime, TimeZone, Utc};
use std::net::SocketAddr;
use warp::{http::Response, Filter};

#[cached(time = 60, size = 5)]
fn count_business_days(mut start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    let cal = bdays::calendars::us::USSettlement;
    let mut count = 0;
    while start < end {
        if cal.is_bday(start) {
            count += 1;
        }
        start = start.checked_add_signed(chrono::Duration::days(1)).unwrap();
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
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "tracing=info,warp=info".to_owned());
    let filter = tracing_subscriber::filter::EnvFilter::new(filter);

    tracing_subscriber::fmt().with_env_filter(filter).init();

    let routes = warp::any()
        .and(warp::header::optional::<String>("accept"))
        .map(|accept: Option<String>| {
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
                                </div>
                            </body>
                            <script>
                                var countDownDate = Date.parse("2022-10-07T00:00:00Z");
                                function tick() {
                                  var now = Date.parse(new Date().toISOString());
                                  var distance = countDownDate - now;
                                  var days = Math.floor(distance / (1000 * 60 * 60 * 24));
                                  var hours = Math.floor((distance % (1000 * 60 * 60 * 24)) / (1000 * 60 * 60));
                                  var minutes = Math.floor((distance % (1000 * 60 * 60)) / (1000 * 60));
                                  var seconds = Math.floor((distance % (1000 * 60)) / 1000);
                                  document.getElementById("timer").innerHTML = days + "d " + hours + "h " + minutes + "m " + seconds + "s ";
                                  if (distance < 0) {
                                    clearInterval(x);
                                    document.getElementById("timer").innerHTML = "MADE IT";
                                  }
                                }
                                tick();
                                var x = setInterval(tick, 1000);
                            </script>
                        </html>
                    "##;
                    let resp = Response::builder()
                        .header("Content-Type", "text/html")
                        .body(html.to_string()).unwrap();
                    return resp;
                }
            }
            let start = Utc.ymd(2021, 10, 7).and_hms(0,0,0);
            let now = Utc::now();
            let done = Utc.ymd(2022, 10, 7).and_hms(0, 0, 0);
            let (days, hours, minutes, seconds) = timedelta(&now, &done);
            let bdays_left = count_business_days(now, done);
            let bdays_done = count_business_days(start, now);
            Response::new(format!("{days}d {hours}h {minutes}m {seconds}s\nbusiness days left: {bdays_left}\nbusiness days done: {bdays_done}"))
        })
        .with(warp::trace::request());

    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("PORT").unwrap_or_else(|_| "3003".to_string());
    let addr = format!("{host}:{port}");
    warp::serve(routes)
        .run(
            addr.parse::<SocketAddr>()
                .map_err(|e| format!("invalid host/port: {addr}, {e}"))
                .unwrap(),
        )
        .await;
}
