//! Sealing private card values to a client's short-lived ECDH key (SEAL-*).
//!
//! The AES-256-GCM key is HKDF-SHA256 over the P-256 ECDH shared secret (its
//! x coordinate, as WebCrypto `deriveBits` returns it) between the server's
//! static key and the client's key, with a random salt per message. The
//! browser side lives in `assets/static/app.js`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use p256::{ecdh::diffie_hellman, PublicKey, SecretKey};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hkdf::{Salt, HKDF_SHA256};
use serde::Serialize;

pub const INFO: &[u8] = b"komino reveal v1";
const SALT_LEN: usize = 16;
const IV_LEN: usize = 12;

#[derive(Debug, PartialEq, Eq)]
pub struct SealError;

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("seal error")
    }
}

impl std::error::Error for SealError {}

/// The server's static key pair.
pub struct ServerKey {
    secret: SecretKey,
    /// Uncompressed SEC1 point, base64url.
    pub public_key: String,
    /// First 16 hex chars of the public point's sha256.
    pub kid: String,
}

/// A sealed response body.
#[derive(Debug, Serialize, Clone)]
pub struct Sealed {
    pub kid: String,
    pub salt: String,
    pub iv: String,
    pub ct: String,
}

pub fn public_point(key: &PublicKey) -> Vec<u8> {
    key.to_sec1_bytes().to_vec()
}

/// A client public key from base64url SEC1 bytes, checked to be on the curve.
pub fn parse_public(b64: &str) -> Option<PublicKey> {
    let bytes = B64.decode(b64).ok()?;
    PublicKey::from_sec1_bytes(&bytes).ok()
}

fn aes_key(secret: &SecretKey, peer: &PublicKey, salt: &[u8]) -> Result<LessSafeKey, SealError> {
    let shared = diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
    let prk = Salt::new(HKDF_SHA256, salt).extract(shared.raw_secret_bytes());
    let okm = prk.expand(&[INFO], &AES_256_GCM).map_err(|_| SealError)?;
    Ok(LessSafeKey::new(UnboundKey::from(okm)))
}

impl ServerKey {
    pub fn from_hex(hex_scalar: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(hex_scalar.trim())?;
        // the parser would left-pad a short scalar; require the full width
        if bytes.len() != 32 {
            anyhow::bail!("expected 32 bytes, got {}", bytes.len());
        }
        let secret = SecretKey::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("not a valid P-256 scalar"))?;
        let point = public_point(&secret.public_key());
        let kid = hex::encode(common::crypto::sha256(&point))[..16].to_string();
        Ok(Self {
            secret,
            public_key: B64.encode(point),
            kid,
        })
    }

    pub fn seal(
        &self,
        client: &PublicKey,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, SealError> {
        let salt = common::crypto::rand_bytes(SALT_LEN).map_err(|_| SealError)?;
        let iv = common::crypto::rand_bytes(IV_LEN).map_err(|_| SealError)?;
        self.seal_with(client, aad, plaintext, &salt, &iv)
    }

    /// [`ServerKey::seal`] with a given salt and iv, for fixed test vectors.
    pub fn seal_with(
        &self,
        client: &PublicKey,
        aad: &[u8],
        plaintext: &[u8],
        salt: &[u8],
        iv: &[u8],
    ) -> Result<Sealed, SealError> {
        let key = aes_key(&self.secret, client, salt)?;
        let nonce = Nonce::try_assume_unique_for_key(iv).map_err(|_| SealError)?;
        let mut buf = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::from(aad), &mut buf)
            .map_err(|_| SealError)?;
        Ok(Sealed {
            kid: self.kid.clone(),
            salt: B64.encode(salt),
            iv: B64.encode(iv),
            ct: B64.encode(buf),
        })
    }
}

/// The client side of [`ServerKey::seal`]; the browser does the same with
/// WebCrypto. Used by tests.
pub fn open(
    client: &SecretKey,
    server_public_b64: &str,
    aad: &[u8],
    sealed: &Sealed,
) -> Result<Vec<u8>, SealError> {
    let server = parse_public(server_public_b64).ok_or(SealError)?;
    let salt = B64.decode(&sealed.salt).map_err(|_| SealError)?;
    let iv = B64.decode(&sealed.iv).map_err(|_| SealError)?;
    let mut buf = B64.decode(&sealed.ct).map_err(|_| SealError)?;
    let key = aes_key(client, &server, &salt)?;
    let nonce = Nonce::try_assume_unique_for_key(&iv).map_err(|_| SealError)?;
    let plain = key
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .map_err(|_| SealError)?;
    Ok(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    // shared with crates/komino/web/test/sealed.test.mjs, which decrypts the
    // same vector with WebCrypto
    pub const SERVER_D: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
    pub const CLIENT_D: &str = "519b423d715f8b581f4fa8ee59f4771a5b44c8130b4e3eacca54a56dda72b464";

    fn client() -> SecretKey {
        SecretKey::from_slice(&hex::decode(CLIENT_D).unwrap()).unwrap()
    }

    #[test]
    fn seal_round_trips_and_binds_the_aad() {
        let server = ServerKey::from_hex(SERVER_D).unwrap();
        let c = client();
        let sealed = server
            .seal(&c.public_key(), b"ABCDEF", b"{\"card\":7}")
            .unwrap();
        assert_eq!(sealed.kid, server.kid);
        assert_eq!(
            open(&c, &server.public_key, b"ABCDEF", &sealed).unwrap(),
            b"{\"card\":7}"
        );
        // another room, another key, or a flipped bit all fail
        assert!(open(&c, &server.public_key, b"ZZZZZZ", &sealed).is_err());
        let other = ServerKey::from_hex(&"11".repeat(32)).unwrap();
        assert!(open(&c, &other.public_key, b"ABCDEF", &sealed).is_err());
        let mut bad = sealed.clone();
        bad.ct
            .replace_range(0..1, if bad.ct.starts_with('A') { "B" } else { "A" });
        assert!(open(&c, &server.public_key, b"ABCDEF", &bad).is_err());
    }

    #[test]
    fn each_seal_uses_a_fresh_salt_and_iv() {
        let server = ServerKey::from_hex(SERVER_D).unwrap();
        let c = client().public_key();
        let a = server.seal(&c, b"", b"x").unwrap();
        let b = server.seal(&c, b"", b"x").unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.iv, b.iv);
        assert_ne!(a.ct, b.ct);
    }

    #[test]
    fn public_keys_must_be_points_on_the_curve() {
        let server = ServerKey::from_hex(SERVER_D).unwrap();
        assert!(parse_public(&server.public_key).is_some());
        assert!(parse_public("not base64!").is_none());
        assert!(parse_public(&B64.encode([4u8; 65])).is_none());
        assert!(ServerKey::from_hex("zz").is_err());
        assert!(ServerKey::from_hex(&"00".repeat(32)).is_err());
    }

    /// The same bytes WebCrypto opens in `web/test/sealed.test.mjs`.
    #[test]
    fn seal_matches_the_shared_vector() {
        let server = ServerKey::from_hex(SERVER_D).unwrap();
        assert_eq!(
            server.public_key,
            "BGD-1LolWp0xyWHrdMY1bWjASbiSO2H6bOZpYi5g8p-2eQP-EAi4vJmkGunpVii8ZPLxsgwtfp9Rd6PClNRGIpk"
        );
        assert_eq!(server.kid, "b18b86ce1389e46d");
        let sealed = server
            .seal_with(
                &client().public_key(),
                b"ABCDEF",
                b"{\"cards\":[{\"seat\":0,\"slot\":2,\"v\":5}]}",
                &[7u8; 16],
                &[9u8; 12],
            )
            .unwrap();
        assert_eq!(sealed.salt, "BwcHBwcHBwcHBwcHBwcHBw");
        assert_eq!(sealed.iv, "CQkJCQkJCQkJCQkJ");
        assert_eq!(
            sealed.ct,
            "BOIuu3b34FzCYk9QlGtjjUQIf33O57vqUZnIEvrFGz7UQ-JHHxmKNDNTmK9k1DoEV9_BBYY"
        );
    }
}
