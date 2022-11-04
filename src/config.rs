use chrono::{DateTime, Utc};
use std::io::Read;

fn env_or(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

pub struct Config {
    pub version: String,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub log_json: bool,

    pub start_date: DateTime<Utc>,
    pub end_date: DateTime<Utc>,
}
impl Config {
    pub fn load() -> Self {
        dotenv::dotenv().ok();
        let version = std::fs::File::open("commit_hash.txt")
            .map(|mut f| {
                let mut s = String::new();
                f.read_to_string(&mut s).expect("Error reading commit_hash");
                s.trim().to_string()
            })
            .unwrap_or_else(|_| "unknown".to_string());

        let start_date =
            std::env::var("START_DATE").unwrap_or_else(|_| "2021-01-01T00:00:00Z".to_string());
        let end_date =
            std::env::var("END_DATE").unwrap_or_else(|_| "2022-01-01T00:00:00Z".to_string());

        let start_date = DateTime::parse_from_rfc3339(&start_date)
            .map_err(|e| format!("error parsing start date: {start_date}, {e}"))
            .unwrap()
            .with_timezone(&Utc);
        let end_date = DateTime::parse_from_rfc3339(&end_date)
            .map_err(|e| format!("error parsing end date: {end_date}, {e}"))
            .unwrap()
            .with_timezone(&Utc);
        Self {
            version,
            host: env_or("HOST", "localhost"),
            port: env_or("PORT", "3003").parse().expect("invalid port"),
            log_level: env_or("LOG_LEVEL", "mono=info,tracing=info,warp=info"),
            log_json: env_or("LOG_JSON", "false") == "true",
            start_date,
            end_date,
        }
    }
    pub fn initialize(&self) {
        use crate::CONFIG;
        tracing::info!(
            version = %CONFIG.version,
            host = %CONFIG.host,
            port = %CONFIG.port,
            log_level = %CONFIG.log_level,
            log_json = %CONFIG.log_json,
            start_date = %CONFIG.start_date.to_rfc3339(),
            end_date = %CONFIG.end_date.to_rfc3339(),
            "initialized config",
        );
    }
    pub fn get_host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
    pub fn get_localhost_port(&self) -> String {
        format!("localhost:{}", self.port)
    }
    pub fn get_127_port(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}
