use std::io::Read;

#[derive(Clone)]
pub struct Config {
    pub version: String,
    pub database_url: String,
    pub s3_bucket: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    /// Maximum size of a single upload in bytes (default 300 MB).
    pub upload_limit_bytes: usize,
    /// Seconds after which an uncommitted init_upload expires (default 300).
    pub upload_timeout_secs: i64,
    /// Default upload lifespan in seconds (default 7 days).
    pub upload_lifespan_secs_default: i64,
    /// Seconds after which an init_download token expires (default 300).
    pub download_timeout_secs: i64,
    /// How often the sweeper runs in seconds (default 120).
    pub expired_cleanup_interval_secs: u64,
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
        }
    }
}
