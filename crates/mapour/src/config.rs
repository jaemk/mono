use std::io::Read;

#[derive(Clone)]
pub struct Config {
    pub version: String,

    // key used to hmac auth/anon/invite/verification tokens before storage
    pub signing_key: String,

    pub database_url: String,

    // when true, users must verify their email before logging in.
    // disabled by default until an smtp relay is configured.
    pub require_email_verification: bool,

    pub max_photo_bytes: usize,
    pub auth_token_days: i64,

    // external hostname used to build shareable links
    pub real_hostname: String,

    // S3 / Tigris storage for pin photos
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
            signing_key: common::utils::env_or(
                "MAPOUR_SIGNING_KEY",
                "01234567890123456789012345678901",
            ),
            database_url: common::utils::env_or(
                "MAPOUR_DATABASE_URL",
                "postgres://localhost/mapour",
            ),
            require_email_verification: common::utils::env_or(
                "MAPOUR_REQUIRE_EMAIL_VERIFICATION",
                "false",
            ) == "true",
            max_photo_bytes: common::utils::env_or("MAPOUR_MAX_PHOTO_BYTES", "5000000")
                .parse()
                .unwrap_or(5_000_000),
            auth_token_days: common::utils::env_or("MAPOUR_AUTH_TOKEN_DAYS", "30")
                .parse()
                .unwrap_or(30),
            real_hostname: common::utils::env_or("MAPOUR_REAL_HOSTNAME", "http://localhost:3003"),
            s3_bucket: common::utils::env_or("MAPOUR_S3_BUCKET", "kom-mapour"),
            s3_endpoint: common::utils::env_or("AWS_ENDPOINT_URL_S3", "https://t3.storage.dev"),
            s3_region: common::utils::env_or("AWS_REGION", "auto"),
        }
    }
}
