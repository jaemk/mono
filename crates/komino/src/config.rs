// local/test fallback for KOMINO_SIGNING_KEY; never valid in a deployment
const DEV_SIGNING_KEY: &str = "01234567890123456789012345678901";

#[derive(Clone)]
pub struct Config {
    // gate for the whole subsite: when false the mono binary skips komino
    // initialization entirely (no db pool, no routes)
    pub enabled: bool,

    // key used to hmac the player id cookie
    pub signing_key: String,

    pub database_url: String,

    // external hostname used to build share links
    pub real_hostname: String,
}

impl Config {
    pub fn load() -> Self {
        Self {
            enabled: common::utils::env_or("KOMINO_ENABLED", "false") == "true",
            signing_key: common::utils::env_or("KOMINO_SIGNING_KEY", DEV_SIGNING_KEY),
            database_url: common::utils::env_or(
                "KOMINO_DATABASE_URL",
                "postgres://localhost/komino",
            ),
            real_hostname: common::utils::env_or("KOMINO_REAL_HOSTNAME", "http://localhost:3000"),
        }
    }

    /// Reject settings that are only safe locally, so a missing secret fails
    /// the deploy instead of signing cookies with a public key.
    pub fn validate_for_deploy(&self) -> anyhow::Result<()> {
        if self.signing_key == DEV_SIGNING_KEY || self.signing_key.len() < 32 {
            anyhow::bail!("KOMINO_SIGNING_KEY must be set to a random value of at least 32 chars");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(signing_key: &str) -> Config {
        Config {
            enabled: true,
            signing_key: signing_key.to_string(),
            database_url: String::new(),
            real_hostname: String::new(),
        }
    }

    #[test]
    fn deploy_rejects_dev_or_short_signing_key() {
        assert!(config(DEV_SIGNING_KEY).validate_for_deploy().is_err());
        assert!(config("short").validate_for_deploy().is_err());
        assert!(config(&"a1".repeat(32)).validate_for_deploy().is_ok());
    }
}
