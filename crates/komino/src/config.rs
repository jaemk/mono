// local/test fallbacks for KOMINO_SIGNING_KEY and KOMINO_ECDH_KEY; never valid
// in a deployment
const DEV_SIGNING_KEY: &str = "01234567890123456789012345678901";
pub const DEV_ECDH_KEY: &str = "6b6f6d696e6f2d6465762d656364682d6b65792d6e6f742d666f722d70726f64";

#[derive(Clone)]
pub struct Config {
    // gate for the whole subsite: when false the mono binary skips komino
    // initialization entirely (no db pool, no routes)
    pub enabled: bool,

    // key used to hmac the player id cookie
    pub signing_key: String,

    // hex P-256 scalar of the static key that seals private card values
    pub ecdh_key: String,

    pub database_url: String,

    // external hostname used to build share links
    pub real_hostname: String,
}

impl Config {
    pub fn load() -> Self {
        Self {
            enabled: common::utils::env_or("KOMINO_ENABLED", "false") == "true",
            signing_key: common::utils::env_or("KOMINO_SIGNING_KEY", DEV_SIGNING_KEY),
            ecdh_key: common::utils::env_or("KOMINO_ECDH_KEY", DEV_ECDH_KEY),
            database_url: common::utils::env_or(
                "KOMINO_DATABASE_URL",
                "postgres://localhost/komino",
            ),
            real_hostname: common::utils::env_or("KOMINO_REAL_HOSTNAME", "http://localhost:3000"),
        }
    }

    /// Reject settings that are only safe locally, so a missing secret fails
    /// the deploy instead of signing cookies or sealing cards with a public key.
    pub fn validate_for_deploy(&self) -> anyhow::Result<()> {
        if self.signing_key == DEV_SIGNING_KEY || self.signing_key.len() < 32 {
            anyhow::bail!("KOMINO_SIGNING_KEY must be set to a random value of at least 32 chars");
        }
        // compare the parsed scalar, so case or padding can't disguise the dev key
        let key = crate::sealed::ServerKey::from_hex(&self.ecdh_key).map_err(|e| {
            anyhow::anyhow!("KOMINO_ECDH_KEY must be a P-256 scalar of 64 hex chars: {e}")
        })?;
        if key.public_key == crate::sealed::ServerKey::from_hex(DEV_ECDH_KEY)?.public_key {
            anyhow::bail!("KOMINO_ECDH_KEY must be set to a random P-256 scalar (64 hex chars)");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(signing_key: &str, ecdh_key: &str) -> Config {
        Config {
            enabled: true,
            signing_key: signing_key.to_string(),
            ecdh_key: ecdh_key.to_string(),
            database_url: String::new(),
            real_hostname: String::new(),
        }
    }

    #[test]
    fn deploy_rejects_dev_or_short_signing_key() {
        let ecdh = "a1".repeat(32);
        assert!(config(DEV_SIGNING_KEY, &ecdh)
            .validate_for_deploy()
            .is_err());
        assert!(config("short", &ecdh).validate_for_deploy().is_err());
        assert!(config(&"a1".repeat(32), &ecdh)
            .validate_for_deploy()
            .is_ok());
    }

    #[test]
    fn deploy_rejects_dev_or_invalid_ecdh_key() {
        let signing = "a1".repeat(32);
        assert!(config(&signing, DEV_ECDH_KEY)
            .validate_for_deploy()
            .is_err());
        assert!(config(&signing, "not hex").validate_for_deploy().is_err());
        // zero is not a valid scalar
        assert!(config(&signing, &"00".repeat(32))
            .validate_for_deploy()
            .is_err());
        // the dev key in disguise is still the dev key
        for disguised in [DEV_ECDH_KEY.to_uppercase(), format!("  {DEV_ECDH_KEY}\n")] {
            assert!(config(&signing, &disguised).validate_for_deploy().is_err());
        }
        // exactly 32 bytes, not a shorter scalar left-padded by the parser
        assert!(config(&signing, &"a1".repeat(31))
            .validate_for_deploy()
            .is_err());
        assert!(config(&signing, &"a1".repeat(32))
            .validate_for_deploy()
            .is_ok());
    }
}
