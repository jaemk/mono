use chrono::{DateTime, Utc};
use common::utils::env_or;
use std::io::Read;

lazy_static::lazy_static! {
    pub static ref CONFIG: Config = Config::load();
}

pub struct Config {
    pub version: String,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub log_json: bool,
    pub ssl: bool,
    // ugh
    pub start_date: DateTime<Utc>,
}

impl Config {
    pub fn load() -> Self {
        let version = std::fs::File::open("commit_hash.txt")
            .map(|mut f| {
                let mut s = String::new();
                f.read_to_string(&mut s).expect("Error reading commit_hash");
                s.trim().to_string()
            })
            .unwrap_or_else(|_| "unknown".to_string());

        let start_date_str =
            std::env::var("UGH_START_DATE").unwrap_or_else(|_| "2021-01-01T00:00:00Z".to_string());

        let start_date = DateTime::parse_from_rfc3339(&start_date_str)
            .map_err(|e| format!("error parsing start date: {start_date_str}, {e}"))
            .unwrap()
            .with_timezone(&Utc);

        Self {
            version,
            host: env_or("HOST", "0.0.0.0"),
            port: env_or("PORT", "3003").parse().expect("invalid port"),
            log_level: env_or("LOG_LEVEL", "info,sqlx=warn"),
            log_json: env_or("LOG_JSON", "false") == "true",
            ssl: env_or("SSL", "false") == "true",
            start_date,
        }
    }

    pub fn initialize(&self) {
        tracing::info!(
            version = %self.version,
            host = %self.host,
            port = %self.port,
            log_level = %self.log_level,
            log_json = %self.log_json,
            start_date = %self.start_date.to_rfc3339(),
            ssl = %self.ssl,
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
