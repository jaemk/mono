mod config;
use serde::Serialize;

use bdays::HolidayCalendar;
use cached::proc_macro::{cached, once};
use chrono::{Date, DateTime, Utc};
use std::convert::Infallible;
use std::net::SocketAddr;
use warp::http::StatusCode;
use warp::path::{FullPath, Tail};
use warp::{http::Response, Filter, Rejection, Reply};

lazy_static::lazy_static! {
    pub static ref CONFIG: config::Config = config::Config::load();
}

mod ugh {
    use super::*;
    use tokio::io::AsyncReadExt;

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
                let mut f = tokio::fs::File::open("static/ugh_index.html")
                    .await
                    .expect("failed opening ugh_index.html");
                let mut html = String::new();
                f.read_to_string(&mut html)
                    .await
                    .expect("failed reading ugh_index.html");
                let resp = Response::builder()
                    .header("Content-Type", "text/html")
                    .body(html)
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
    if CONFIG.log_json {
        tracing_subscriber::fmt()
            .json()
            .with_current_span(false)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

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
    let host_ugh_kom = localhost.clone().or(warp::host::exact("ugh.kominick.com"));
    let host_ip_kom = localhost.clone().or(warp::host::exact("ip.kominick.com"));
    let host_git_jaemk = localhost.clone().or(warp::host::exact("git.jaemk.me"));

    // jaemk.me
    let git_index = warp::get()
        .and(host_git_jaemk)
        .and(warp::header::optional("host"))
        .and(warp::path::tail())
        .map(move |_, host: Option<String>, path: Tail| {
            let path = path.as_str();
            tracing::info!("host: {:?}, path: {}", host, path);
            let uri = format!("https://github.com/jaemk/{path}");
            Response::builder()
                .header("Location", uri)
                .status(302)
                .body("")
                .unwrap()
        });

    // -- ip.kominick.com --
    let ip_index = warp::get()
        .and(host_ip_kom)
        .and(warp::header::optional("fly-client-ip"))
        .map(move |_, remote: Option<String>| {
            let ip = remote.unwrap_or_else(|| "unknown".into());
            Response::builder()
                .status(200)
                .body(format!("{ip}\n"))
                .unwrap()
        });

    // -- ugh.kominick.com --
    let ugh_dates = warp::path!("dates" / "end")
        .and(host_ugh_kom.clone())
        .and(warp::get())
        .and_then(move |_| async { ugh::dates_end().await });

    let ugh_index = warp::any()
        .and(warp::path::end())
        .and(host_ugh_kom)
        .and(warp::header::optional::<String>("accept"))
        .and_then(move |_, accept: Option<String>| async { ugh::index(accept).await });

    let routes = ugh_index
        .or(ugh_dates)
        .or(git_index)
        .or(favicon)
        .or(status)
        .or(ip_index)
        .recover(recover)
        .with(warp::wrap_fn(trace_wrapper));

    let addr = CONFIG.get_host_port();
    warp::serve(routes)
        .run(
            addr.parse::<SocketAddr>()
                .map_err(|e| format!("invalid host/port: {addr}, {e}"))
                .unwrap(),
        )
        .await;
}

/// https://github.com/seanmonstar/warp/blob/master/examples/rejections.rs
async fn recover(err: Rejection) -> Result<impl Reply, Infallible> {
    #[derive(Serialize)]
    struct ErrorMessage {
        code: u16,
        message: String,
    }
    let code;
    let message;

    if err.is_not_found() {
        code = StatusCode::NOT_FOUND;
        message = "NOT_FOUND";
    } else if err.find::<warp::reject::MethodNotAllowed>().is_some() {
        code = StatusCode::METHOD_NOT_ALLOWED;
        message = "METHOD_NOT_ALLOWED";
    } else {
        tracing::error!("unhandled rejection: {:?}", err);
        code = StatusCode::INTERNAL_SERVER_ERROR;
        message = "UNHANDLED_REJECTION";
    }

    let json = warp::reply::json(&ErrorMessage {
        code: code.as_u16(),
        message: message.into(),
    });
    Ok(warp::reply::with_status(json, code))
}

/// https://github.com/seanmonstar/warp/blob/master/examples/wrapping.rs
fn trace_wrapper<F, T>(
    filter: F,
) -> impl Filter<Extract = (impl Reply,)> + Clone + Send + Sync + 'static
where
    F: Filter<Extract = (T,), Error = Infallible> + Clone + Send + Sync + 'static,
    F::Extract: Reply,
    T: Reply,
{
    warp::any()
        .and(warp::path::full())
        .and(warp::header::optional("host"))
        .and(warp::header::optional("fly-client-ip"))
        .map(
            |path: FullPath, host: Option<String>, remote: Option<String>| {
                tracing::info!(
                    path = %path.as_str(),
                    host = %host.unwrap_or_default(),
                    remote = %remote.unwrap_or_default(),
                    "handling request",
                );
            },
        )
        .untuple_one()
        .and(filter)
        .map(|response| {
            tracing::debug!("request complete");
            response
        })
}
