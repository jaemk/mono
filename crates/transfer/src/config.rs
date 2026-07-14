use std::io::Read;

#[derive(Clone)]
pub struct Config {
    pub version: String,
    pub database_url: String,
    pub s3_bucket: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub upload_limit_bytes: usize,
    pub upload_timeout_secs: i64,
    pub upload_lifespan_secs_default: i64,
    pub download_timeout_secs: i64,
    pub expired_cleanup_interval_secs: u64,
    pub google_client_id: Option<String>,
    pub smtp_config: Option<common::smtp::SmtpConfig>,
    pub base_url: String,
    pub registration_code_secs: i64,
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

        Self {
            version,
            database_url: common::utils::env_or(
                "TRANSFER_DATABASE_URL",
                "postgres://localhost/transfer",
            ),
            s3_bucket: common::utils::env_or("TRANSFER_S3_BUCKET", "kom-transfer"),
            s3_endpoint: common::utils::env_or("TRANSFER_S3_ENDPOINT", "https://t3.storage.dev"),
            s3_region: common::utils::env_or("TRANSFER_S3_REGION", "auto"),
            upload_limit_bytes: common::utils::env_or("TRANSFER_UPLOAD_LIMIT_BYTES", "314572800")
                .parse()
                .unwrap_or(314_572_800),
            upload_timeout_secs: common::utils::env_or("TRANSFER_UPLOAD_TIMEOUT_SECS", "300")
                .parse()
                .unwrap_or(300),
            upload_lifespan_secs_default: common::utils::env_or(
                "TRANSFER_UPLOAD_LIFESPAN_SECS_DEFAULT",
                "604800",
            )
            .parse()
            .unwrap_or(604_800),
            download_timeout_secs: common::utils::env_or("TRANSFER_DOWNLOAD_TIMEOUT_SECS", "300")
                .parse()
                .unwrap_or(300),
            expired_cleanup_interval_secs: common::utils::env_or(
                "TRANSFER_EXPIRED_CLEANUP_INTERVAL_SECS",
                "120",
            )
            .parse()
            .unwrap_or(120),
            google_client_id: std::env::var("TRANSFER_GOOGLE_CLIENT_ID").ok(),
            smtp_config: common::smtp::SmtpConfig::from_env().ok(),
            base_url: common::utils::env_or("TRANSFER_BASE_URL", "http://localhost:3000"),
            registration_code_secs: common::utils::env_or("TRANSFER_REGISTRATION_CODE_SECS", "600")
                .parse()
                .unwrap_or(600),
        }
    }

    pub fn smtp_enabled(&self) -> bool {
        self.smtp_config.is_some()
    }

    pub fn google_enabled(&self) -> bool {
        self.google_client_id.is_some()
    }
}
