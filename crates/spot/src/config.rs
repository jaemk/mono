use common::utils::env_or;
use std::io::Read;

pub struct Config {
    pub version: String,
    pub db_url: String,
    pub enc_key: common::crypto::Key,
    pub spotify_client_id: String,
    pub spotify_secret_id: String,
    pub auth_expiration_seconds: u32,
    pub poll_interval_seconds: u64,
    pub poll_lock_timeout_seconds: u64,
    pub real_hostname: Option<String>,
    pub real_domain: Option<String>,
    pub host: String,
    pub port: u16,
    pub ssl: bool,
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
            db_url: std::env::var("SPOT_DATABASE_URL").expect("SPOT_DATABASE_URL must be set"),
            enc_key: common::crypto::Key::parse(
                &std::env::var("SPOT_ENC_KEY").expect("SPOT_ENC_KEY must be set"),
            ),
            spotify_client_id: std::env::var("SPOT_SPOTIFY_CLIENT_ID")
                .expect("SPOT_SPOTIFY_CLIENT_ID must be set"),
            spotify_secret_id: std::env::var("SPOT_SPOTIFY_SECRET_ID")
                .expect("SPOT_SPOTIFY_SECRET_ID must be set"),
            auth_expiration_seconds: env_or("SPOT_AUTH_EXPIRATION_SECONDS", "43200")
                .parse()
                .expect("invalid SPOT_AUTH_EXPIRATION_SECONDS"),
            poll_interval_seconds: env_or("SPOT_POLL_INTERVAL_SECONDS", "10")
                .parse()
                .expect("invalid SPOT_POLL_INTERVAL_SECONDS"),
            poll_lock_timeout_seconds: env_or("SPOT_POLL_LOCK_TIMEOUT_SECONDS", "60")
                .parse()
                .expect("invalid SPOT_POLL_LOCK_TIMEOUT_SECONDS"),
            real_hostname: std::env::var("SPOT_REAL_HOSTNAME").ok(),
            real_domain: std::env::var("SPOT_REAL_DOMAIN").ok(),
            host: env_or("SPOT_HOST", "0.0.0.0"),
            port: env_or("SPOT_PORT", "3003")
                .parse()
                .expect("invalid SPOT_PORT"),
            ssl: env_or("SPOT_SSL", "false") == "true",
        }
    }

    pub fn redirect_host(&self) -> String {
        self.real_hostname
            .clone()
            .unwrap_or_else(|| "https://spotie.app".to_string())
    }

    pub fn spotify_redirect_url(&self) -> String {
        format!("{}/spot/auth", self.redirect_host())
    }

    pub fn domain(&self) -> String {
        self.real_domain
            .clone()
            .unwrap_or_else(|| self.host.clone())
    }
}
