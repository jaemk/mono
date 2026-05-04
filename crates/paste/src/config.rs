use serde::Deserialize;
use std::io::Read;

#[derive(Clone, Deserialize)]
pub struct Config {
    pub version: String,

    // key used for encrypting pastes when no user key is provided;
    // parsed from "[id:]material" — see [`common::crypto::Key`]
    pub encryption_key: common::crypto::Key,
    // key used to derive signature of paste content
    pub signing_key: String,

    pub max_paste_bytes: usize,
    pub max_paste_age_seconds: i64,

    pub database_url: String,

    // S3 / Tigris storage
    pub s3_bucket: String,
    pub s3_endpoint: String,
    pub s3_region: String,
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
            encryption_key: common::crypto::Key::parse(&common::utils::env_or(
                "PASTE_ENCRYPTION_KEY",
                "01234567890123456789012345678901",
            )),
            signing_key: common::utils::env_or(
                "PASTE_SIGNING_KEY",
                "01234567890123456789012345678901",
            ),
            max_paste_bytes: common::utils::env_or("MAX_PASTE_BYTES", "1000000")
                .parse()
                .unwrap_or(1_000_000),
            // 60 * 60 * 24 * 30
            max_paste_age_seconds: common::utils::env_or("MAX_PASTE_AGE_SECONDS", "2592000")
                .parse()
                .unwrap_or(2_592_000),
            database_url: common::utils::env_or("PASTE_DATABASE_URL", "postgres://localhost/paste"),
            s3_bucket: common::utils::env_or("PASTE_S3_BUCKET", "kom-paste"),
            s3_endpoint: common::utils::env_or("AWS_ENDPOINT_URL_S3", "https://t3.storage.dev"),
            s3_region: common::utils::env_or("AWS_REGION", "auto"),
        }
    }
}
