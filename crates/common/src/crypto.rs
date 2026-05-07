/*!
Crypto things
*/
use ring::aead::BoundKey;
use ring::pbkdf2;
use std::num::NonZeroU32;

struct OneNonceSequence {
    inner: Option<ring::aead::Nonce>,
}
impl OneNonceSequence {
    fn new(inner: ring::aead::Nonce) -> Self {
        Self { inner: Some(inner) }
    }
}
impl ring::aead::NonceSequence for OneNonceSequence {
    fn advance(&mut self) -> std::result::Result<ring::aead::Nonce, ring::error::Unspecified> {
        self.inner.take().ok_or(ring::error::Unspecified)
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Length of an AES-256-GCM nonce in bytes.
pub const NONCE_LEN: usize = 12;

/// Length of a PBKDF2 salt in bytes.
pub const SALT_LEN: usize = 64;

pub const SALT_PREFIX: &[u8] = b"salt:";
pub const SALT_PREFIX_LEN: usize = SALT_PREFIX.len();

pub const KEY_ID_PREFIX: &[u8] = b"key_id:";
pub const KEY_ID_PREFIX_LEN: usize = KEY_ID_PREFIX.len();

// ---------------------------------------------------------------------------
// Key / KeyRef — named encryption key types
// ---------------------------------------------------------------------------

/// A named AES-256-GCM key derived from a config string.
///
/// Parsed from the `"[id:]material"` format used in environment variables:
/// - `"v1:my-secret"` → `id = "v1"`,      key derived from `"my-secret"`
/// - `"my-secret"`    → `id = "default"`, key derived from `"my-secret"`
///
/// The 32-byte AES key is the SHA-256 hash of the key-material portion.
#[derive(Clone)]
pub struct Key {
    /// Logical key identifier used to select the correct key during decryption.
    pub id: String,
    key_bytes: [u8; 32],
}

impl Key {
    /// Parse `s` using the `"[id:]material"` format described above.
    pub fn parse(s: &str) -> Self {
        let (id, material) = match s.split_once(':') {
            Some((id, material)) => (id.to_string(), material),
            None => ("default".to_string(), s),
        };
        let digest = sha256(material.as_bytes());
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&digest);
        Self { id, key_bytes }
    }

    /// Borrow this key as a [`KeyRef`].
    pub fn as_key_ref(&self) -> KeyRef<'_> {
        KeyRef {
            id: &self.id,
            key: &self.key_bytes,
        }
    }
}

impl From<String> for Key {
    fn from(s: String) -> Self {
        Key::parse(&s)
    }
}

impl<'de> serde::Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Key::parse(&s))
    }
}

/// A borrowed reference to a [`Key`], suitable for passing to encrypt / decrypt.
#[derive(Copy, Clone)]
pub struct KeyRef<'a> {
    /// Logical key identifier.
    pub id: &'a str,
    /// Raw key bytes (must be exactly 32 bytes for AES-256-GCM).
    pub key: &'a [u8],
}

// ---------------------------------------------------------------------------
// Random helpers
// ---------------------------------------------------------------------------

/// Return a `Vec` of secure random bytes of size `n`.
pub fn rand_bytes(n: usize) -> crate::Result<Vec<u8>> {
    use ring::rand::SecureRandom;
    let mut buf = vec![0; n];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| "Error getting random bytes")?;
    Ok(buf)
}

/// Generate a fresh [`NONCE_LEN`]-byte AES-GCM nonce.
pub fn new_nonce() -> crate::Result<Vec<u8>> {
    rand_bytes(NONCE_LEN)
}

/// Generate a fresh [`SALT_LEN`]-byte PBKDF2 salt.
pub fn new_salt() -> crate::Result<Vec<u8>> {
    rand_bytes(SALT_LEN)
}

// ---------------------------------------------------------------------------
// HMAC / hash
// ---------------------------------------------------------------------------

pub fn hmac_sign(s: &str, key: &[u8]) -> String {
    let s_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&s_key, s.as_bytes());
    hex::encode(tag)
}

pub fn hmac_verify(text: &str, sig: &str, key: &[u8]) -> bool {
    let Ok(sig) = hex::decode(sig) else {
        return false;
    };
    let s_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(&s_key, text.as_bytes(), &sig).is_ok()
}

/// Return the SHA-256 hash of `bytes`.
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    Vec::from(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/// Stretch `pw` into a 32-byte AES-256-GCM key using PBKDF2-HMAC-SHA512.
pub fn derive_encryption_key(pw: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA512,
        NonZeroU32::new(100_000).unwrap(),
        salt,
        pw,
        &mut out,
    );
    out
}

// ---------------------------------------------------------------------------
// AES-256-GCM helpers (no KDF — key must already be 32 bytes)
// ---------------------------------------------------------------------------

fn aes_seal(plaintext: &[u8], nonce: &[u8], key: &[u8], aad: &[u8]) -> crate::Result<Vec<u8>> {
    let alg = &ring::aead::AES_256_GCM;
    let nonce = ring::aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| "Encryption nonce not unique")?;
    let unbound =
        ring::aead::UnboundKey::new(alg, key).map_err(|_| "Error building sealing key")?;
    let mut key = ring::aead::SealingKey::new(unbound, OneNonceSequence::new(nonce));
    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(ring::aead::Aad::from(aad), &mut in_out)
        .map_err(|_| "Failed encrypting bytes")?;
    Ok(in_out)
}

fn aes_open(ciphertext: &[u8], nonce: &[u8], key: &[u8], aad: &[u8]) -> crate::Result<Vec<u8>> {
    let alg = &ring::aead::AES_256_GCM;
    let nonce = ring::aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| "Decryption nonce not unique")?;
    let unbound =
        ring::aead::UnboundKey::new(alg, key).map_err(|_| "Error building opening key")?;
    let mut key = ring::aead::OpeningKey::new(unbound, OneNonceSequence::new(nonce));
    let mut buf = ciphertext.to_vec();
    let plaintext = key
        .open_in_place(ring::aead::Aad::from(aad), &mut buf)
        .map_err(|_| "Failed decrypting bytes")?;
    Ok(plaintext.to_vec())
}

// ---------------------------------------------------------------------------
// Encrypted — unified ciphertext envelope
// ---------------------------------------------------------------------------

/// The result of any encryption operation.
///
/// Two variants:
/// - [`Encrypted::Key`] — encrypted with a raw 32-byte key (no KDF).
///   Call [`encode`](Encrypted::encode) to get
///   `base64(KEY_ID_PREFIX ‖ id ‖ \x00 ‖ nonce ‖ ciphertext)`.
/// - [`Encrypted::Pw`] — encrypted with a password (PBKDF2 KDF applied).
///   Call [`encode`](Encrypted::encode) to get
///   `base64(SALT_PREFIX ‖ salt ‖ nonce ‖ ciphertext)`.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub enum Encrypted {
    /// Key-based encryption.  `id` names the key used (may be empty).
    Key {
        id: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Password-based encryption.  `salt` was used for key derivation.
    Pw {
        salt: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
}

impl Encrypted {
    // --- accessors ---

    pub fn nonce(&self) -> &[u8] {
        match self {
            Self::Key { nonce, .. } | Self::Pw { nonce, .. } => nonce,
        }
    }

    pub fn ciphertext(&self) -> &[u8] {
        match self {
            Self::Key { ciphertext, .. } | Self::Pw { ciphertext, .. } => ciphertext,
        }
    }

    /// Returns the salt for [`Encrypted::Pw`] variants, `None` for `Key`.
    pub fn salt(&self) -> Option<&[u8]> {
        match self {
            Self::Pw { salt, .. } => Some(salt),
            Self::Key { .. } => None,
        }
    }

    /// Returns the key ID for [`Encrypted::Key`] variants, `None` for `Pw`.
    pub fn key_id(&self) -> Option<&str> {
        match self {
            Self::Key { id, .. } => Some(id),
            Self::Pw { .. } => None,
        }
    }

    // --- encode / decode ---

    /// Encode to a compact base64 string.
    ///
    /// - `Key`: `base64(KEY_ID_PREFIX ‖ id ‖ \x00 ‖ nonce ‖ ciphertext)`
    /// - `Pw`:  `base64(SALT_PREFIX ‖ salt ‖ nonce ‖ ciphertext)`
    pub fn encode(&self) -> String {
        let buf = match self {
            Self::Key {
                id,
                nonce,
                ciphertext,
            } => {
                let mut buf = Vec::with_capacity(
                    KEY_ID_PREFIX_LEN + id.len() + 1 + NONCE_LEN + ciphertext.len(),
                );
                buf.extend_from_slice(KEY_ID_PREFIX);
                buf.extend_from_slice(id.as_bytes());
                buf.push(0u8); // null separator between id and nonce
                buf.extend_from_slice(nonce);
                buf.extend_from_slice(ciphertext);
                buf
            }
            Self::Pw {
                salt,
                nonce,
                ciphertext,
            } => {
                let mut buf =
                    Vec::with_capacity(SALT_PREFIX_LEN + SALT_LEN + NONCE_LEN + ciphertext.len());
                buf.extend_from_slice(SALT_PREFIX);
                buf.extend_from_slice(salt);
                buf.extend_from_slice(nonce);
                buf.extend_from_slice(ciphertext);
                buf
            }
        };
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&buf)
    }

    /// Decode from a string produced by [`encode`].
    pub fn decode(s: &str) -> crate::Result<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| "Encrypted: base64 decode error")?;

        if bytes.starts_with(KEY_ID_PREFIX) {
            let rest = &bytes[KEY_ID_PREFIX_LEN..];
            // null byte separates the variable-length key id from the nonce
            let sep = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or("Encrypted: missing null separator after key_id")?;
            let id = String::from_utf8(rest[..sep].to_vec())
                .map_err(|_| "Encrypted: key_id is not valid UTF-8")?;
            let rest = &rest[sep + 1..];
            if rest.len() < NONCE_LEN {
                return Err("Encrypted: Key variant too short".into());
            }
            Ok(Self::Key {
                id,
                nonce: rest[..NONCE_LEN].to_vec(),
                ciphertext: rest[NONCE_LEN..].to_vec(),
            })
        } else if bytes.starts_with(SALT_PREFIX) {
            let rest = &bytes[SALT_PREFIX_LEN..];
            if rest.len() < SALT_LEN + NONCE_LEN {
                return Err("Encrypted: Pw variant too short".into());
            }
            Ok(Self::Pw {
                salt: rest[..SALT_LEN].to_vec(),
                nonce: rest[SALT_LEN..SALT_LEN + NONCE_LEN].to_vec(),
                ciphertext: rest[SALT_LEN + NONCE_LEN..].to_vec(),
            })
        } else {
            Err("Encrypted: unknown format prefix".into())
        }
    }
}

// ---------------------------------------------------------------------------
// Key-based encrypt / decrypt (no KDF; key must be 32 bytes)
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` with `key`.
///
/// The key ID is stored in the [`Encrypted::Key`] envelope so the correct key
/// can be selected during decryption.  For password-based encryption use
/// [`encrypt_with_pw`] instead.
pub fn encrypt(plaintext: &[u8], key: KeyRef<'_>) -> crate::Result<Encrypted> {
    encrypt_with_aad(plaintext, key, &[])
}

/// Encrypt `plaintext` with `key` and additional authenticated data (AAD).
pub fn encrypt_with_aad(plaintext: &[u8], key: KeyRef<'_>, aad: &[u8]) -> crate::Result<Encrypted> {
    let nonce = new_nonce()?;
    let ciphertext = aes_seal(plaintext, &nonce, key.key, aad)?;
    Ok(Encrypted::Key {
        id: key.id.to_string(),
        nonce,
        ciphertext,
    })
}

/// Decrypt an [`Encrypted::Key`] value.
///
/// `keys` is searched by the ID stored in the envelope; returns an error if no
/// matching key is found.
pub fn decrypt(enc: &Encrypted, keys: &[KeyRef<'_>]) -> crate::Result<Vec<u8>> {
    decrypt_with_aad(enc, keys, &[])
}

/// Decrypt an [`Encrypted::Key`] value with additional authenticated data.
pub fn decrypt_with_aad(
    enc: &Encrypted,
    keys: &[KeyRef<'_>],
    aad: &[u8],
) -> crate::Result<Vec<u8>> {
    match enc {
        Encrypted::Key {
            id,
            nonce,
            ciphertext,
        } => {
            let key = keys
                .iter()
                .find(|k| k.id == id)
                .ok_or_else(|| format!("decrypt: no key found with id {:?}", id))?;
            aes_open(ciphertext, nonce, key.key, aad)
        }
        Encrypted::Pw { .. } => Err("decrypt: expected Key variant, got Pw".into()),
    }
}

// ---------------------------------------------------------------------------
// Password-based encrypt / decrypt (PBKDF2 KDF applied automatically)
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` with `password` using a fresh salt and automatic
/// PBKDF2-HMAC-SHA512 key derivation.
pub fn encrypt_with_pw(plaintext: &[u8], password: &[u8]) -> crate::Result<Encrypted> {
    encrypt_with_pw_aad(plaintext, password, &[])
}

/// Decrypt an [`Encrypted::Pw`] value with `password`.
pub fn decrypt_with_pw(enc: &Encrypted, password: &[u8]) -> crate::Result<Vec<u8>> {
    decrypt_with_pw_aad(enc, password, &[])
}

/// Encrypt `plaintext` with `password` and additional authenticated data (AAD).
pub fn encrypt_with_pw_aad(
    plaintext: &[u8],
    password: &[u8],
    aad: &[u8],
) -> crate::Result<Encrypted> {
    let nonce = new_nonce()?;
    let salt = new_salt()?;
    let key = derive_encryption_key(password, &salt);
    let ciphertext = aes_seal(plaintext, &nonce, &key, aad)?;
    Ok(Encrypted::Pw {
        salt,
        nonce,
        ciphertext,
    })
}

/// Decrypt an [`Encrypted::Pw`] value with `password` and additional authenticated data.
pub fn decrypt_with_pw_aad(enc: &Encrypted, password: &[u8], aad: &[u8]) -> crate::Result<Vec<u8>> {
    match enc {
        Encrypted::Pw {
            salt,
            nonce,
            ciphertext,
        } => {
            let key = derive_encryption_key(password, salt);
            aes_open(ciphertext, nonce, &key, aad)
        }
        Encrypted::Key { .. } => Err("decrypt_with_pw: expected Pw variant, got Key".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    // ---------------------------------------------------------------------------
    // Random helpers
    // ---------------------------------------------------------------------------

    #[test]
    fn rand_bytes_returns_correct_length() {
        for n in [0, 1, 12, 16, 32, 64] {
            let bytes = rand_bytes(n).expect("rand_bytes should succeed");
            assert_eq!(bytes.len(), n, "rand_bytes({n}) returned wrong length");
        }
    }

    #[test]
    fn new_nonce_is_12_bytes() {
        let nonce = new_nonce().expect("new_nonce should succeed");
        assert_eq!(
            nonce.len(),
            NONCE_LEN,
            "nonce must be {NONCE_LEN} bytes for AES-256-GCM"
        );
    }

    #[test]
    fn new_salt_is_64_bytes() {
        let salt = new_salt().expect("new_salt should succeed");
        assert_eq!(salt.len(), SALT_LEN, "salt must be {SALT_LEN} bytes");
    }

    #[test]
    fn rand_bytes_produces_different_values() {
        let a = rand_bytes(32).unwrap();
        let b = rand_bytes(32).unwrap();
        assert_ne!(a, b, "two random 32-byte values should differ");
    }

    // ---------------------------------------------------------------------------
    // HMAC / hash
    // ---------------------------------------------------------------------------

    #[test]
    fn hash_returns_32_bytes() {
        let digest = sha256(b"hello world");
        assert_eq!(digest.len(), 32, "SHA-256 output must be 32 bytes");
    }

    #[test]
    fn hash_is_deterministic() {
        let a = sha256(b"deterministic input");
        let b = sha256(b"deterministic input");
        assert_eq!(a, b, "hash of same input must be identical");
    }

    #[test]
    fn hash_differs_for_different_inputs() {
        let a = sha256(b"input-one");
        let b = sha256(b"input-two");
        assert_ne!(a, b);
    }

    #[test]
    fn hmac_sign_produces_hex_string() {
        let sig = hmac_sign("some text", b"any-key");
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hmac_sign_is_deterministic() {
        let key = b"stable-key-for-testing";
        let sig1 = hmac_sign("hello", key);
        let sig2 = hmac_sign("hello", key);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn hmac_sign_differs_for_different_keys() {
        let sig1 = hmac_sign("same text", b"key-alpha");
        let sig2 = hmac_sign("same text", b"key-beta");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn hmac_verify_roundtrip() {
        let key = b"verification-key";
        let sig = hmac_sign("verify me", key);
        assert!(hmac_verify("verify me", &sig, key));
    }

    #[test]
    fn hmac_verify_fails_for_tampered_sig() {
        let bad_sig = "0".repeat(64);
        assert!(!hmac_verify("anything", &bad_sig, b"key"));
    }

    #[test]
    fn hmac_verify_fails_for_invalid_hex() {
        assert!(!hmac_verify("anything", "not-hex!", b"key"));
    }

    // ---------------------------------------------------------------------------
    // derive_encryption_key
    // ---------------------------------------------------------------------------

    #[test]
    fn derive_encryption_key_is_deterministic() {
        let pw = b"password";
        let salt = b"0123456789abcdef"; // 16 bytes
        let k1 = derive_encryption_key(pw, salt);
        let k2 = derive_encryption_key(pw, salt);
        assert_eq!(k1, k2);
    }

    #[test]
    fn derive_encryption_key_produces_32_bytes() {
        let key = derive_encryption_key(b"pw", b"0123456789abcdef");
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn derive_encryption_key_differs_with_different_salt() {
        let pw = b"same-password";
        let k1 = derive_encryption_key(pw, b"0123456789abcdef");
        let k2 = derive_encryption_key(pw, b"fedcba9876543210");
        assert_ne!(k1, k2, "different salt must produce different key");
    }

    #[test]
    fn derive_encryption_key_differs_with_different_password() {
        let salt = b"0123456789abcdef";
        let k1 = derive_encryption_key(b"password-a", salt);
        let k2 = derive_encryption_key(b"password-b", salt);
        assert_ne!(k1, k2);
    }

    // ---------------------------------------------------------------------------
    // Key / KeyRef parsing
    // ---------------------------------------------------------------------------

    #[test]
    fn key_parse_with_id() {
        let k = Key::parse("v1:my-secret");
        assert_eq!(k.id, "v1");
    }

    #[test]
    fn key_parse_without_id_defaults_to_default() {
        let k = Key::parse("my-secret");
        assert_eq!(k.id, "default");
    }

    #[test]
    fn key_parse_same_material_same_bytes() {
        let k1 = Key::parse("id1:material");
        let k2 = Key::parse("id2:material");
        // Different ids but same material → same key bytes
        assert_eq!(k1.as_key_ref().key, k2.as_key_ref().key);
    }

    #[test]
    fn key_parse_different_material_different_bytes() {
        let k1 = Key::parse("mat-a");
        let k2 = Key::parse("mat-b");
        assert_ne!(k1.as_key_ref().key, k2.as_key_ref().key);
    }

    // ---------------------------------------------------------------------------
    // Encrypted — Key variant (key-based)
    // ---------------------------------------------------------------------------

    fn make_key_ref<'a>(id: &'a str, key_bytes: &'a [u8]) -> KeyRef<'a> {
        KeyRef { id, key: key_bytes }
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key_bytes = sha256(b"test-key");
        let k = make_key_ref("default", &key_bytes);
        let plaintext = b"hello, encrypt!";
        let enc = encrypt(plaintext, k).expect("encrypt should succeed");
        let dec = decrypt(&enc, &[k]).expect("decrypt should succeed");
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn decrypt_fails_with_wrong_key() {
        let key_bytes1 = sha256(b"key-one");
        let key_bytes2 = sha256(b"key-two");
        // Both have the same id so the lookup succeeds but AES-GCM auth fails
        let k1 = make_key_ref("default", &key_bytes1);
        let k2 = make_key_ref("default", &key_bytes2);
        let enc = encrypt(b"secret", k1).expect("encrypt should succeed");
        assert!(
            decrypt(&enc, &[k2]).is_err(),
            "wrong key should fail decryption"
        );
    }

    #[test]
    fn decrypt_fails_with_no_matching_key_id() {
        let key_bytes = sha256(b"k");
        let k = make_key_ref("v1", &key_bytes);
        let enc = encrypt(b"data", k).unwrap();
        let wrong_id = make_key_ref("v2", &key_bytes);
        assert!(
            decrypt(&enc, &[wrong_id]).is_err(),
            "missing key id should fail"
        );
    }

    #[test]
    fn decrypt_selects_correct_key_from_ring() {
        let kb1 = sha256(b"key-one");
        let kb2 = sha256(b"key-two");
        let k1 = make_key_ref("v1", &kb1);
        let k2 = make_key_ref("v2", &kb2);
        let enc = encrypt(b"payload", k2).unwrap();
        // k1 is in the ring but the envelope says "v2" → k2 is used
        let dec = decrypt(&enc, &[k1, k2]).unwrap();
        assert_eq!(dec, b"payload");
    }

    #[test]
    fn encrypt_produces_fresh_nonce_each_call() {
        let key_bytes = sha256(b"nonce-key");
        let k = make_key_ref("default", &key_bytes);
        let enc1 = encrypt(b"data", k).unwrap();
        let enc2 = encrypt(b"data", k).unwrap();
        assert_ne!(enc1.nonce(), enc2.nonce());
        assert_ne!(enc1.ciphertext(), enc2.ciphertext());
    }

    #[test]
    fn enc_encode_decode_roundtrip() {
        let key_bytes = sha256(b"encode-key");
        let k = make_key_ref("default", &key_bytes);
        let enc = encrypt(b"encode me", k).unwrap();
        let encoded = enc.encode();
        let decoded = Encrypted::decode(&encoded).expect("decode should succeed");
        assert_eq!(decoded.nonce(), enc.nonce());
        assert_eq!(decoded.ciphertext(), enc.ciphertext());
        let dec = decrypt(&decoded, &[k]).expect("decrypt after encode/decode should succeed");
        assert_eq!(dec, b"encode me");
    }

    #[test]
    fn enc_decode_rejects_invalid_base64() {
        assert!(Encrypted::decode("not-valid-base64!!!").is_err());
    }

    #[test]
    fn enc_decode_rejects_unknown_prefix() {
        let bad = base64::engine::general_purpose::STANDARD
            .encode(b"garbage data here that is long enough");
        assert!(Encrypted::decode(&bad).is_err());
    }

    #[test]
    fn key_id_roundtrip_through_encode_decode() {
        let key_bytes = sha256(b"named-key");
        let k = make_key_ref("my-key-v1", &key_bytes);
        let enc = encrypt(b"payload", k).unwrap();
        assert_eq!(enc.key_id(), Some("my-key-v1"));
        let decoded = Encrypted::decode(&enc.encode()).unwrap();
        assert_eq!(decoded.key_id(), Some("my-key-v1"));
        let dec = decrypt(&decoded, &[k]).unwrap();
        assert_eq!(dec, b"payload");
    }

    // ---------------------------------------------------------------------------
    // encrypt_with_aad / decrypt_with_aad
    // ---------------------------------------------------------------------------

    #[test]
    fn encrypt_decrypt_with_aad_roundtrip() {
        let key_bytes = sha256(b"aad-key");
        let k = make_key_ref("default", &key_bytes);
        let aad = b"additional-data";
        let pt = b"plaintext with aad";
        let enc = encrypt_with_aad(pt, k, aad).expect("encrypt_with_aad should succeed");
        let dec = decrypt_with_aad(&enc, &[k], aad).expect("decrypt_with_aad should succeed");
        assert_eq!(dec, pt);
    }

    #[test]
    fn decrypt_with_wrong_aad_fails() {
        let key_bytes = sha256(b"aad-key");
        let k = make_key_ref("default", &key_bytes);
        let enc = encrypt_with_aad(b"secured", k, b"aad-one").unwrap();
        assert!(
            decrypt_with_aad(&enc, &[k], b"aad-two").is_err(),
            "wrong AAD must fail"
        );
    }

    #[test]
    fn decrypt_with_empty_aad_fails_when_encrypted_with_nonempty() {
        let key_bytes = sha256(b"aad-key-2");
        let k = make_key_ref("default", &key_bytes);
        let enc = encrypt_with_aad(b"secured", k, b"non-empty-aad").unwrap();
        assert!(decrypt_with_aad(&enc, &[k], b"").is_err());
    }

    #[test]
    fn decrypt_key_variant_rejects_pw_decrypt() {
        let key_bytes = sha256(b"k");
        let k = make_key_ref("default", &key_bytes);
        let enc = encrypt(b"data", k).unwrap();
        assert!(decrypt_with_pw(&enc, b"pw").is_err());
    }

    // ---------------------------------------------------------------------------
    // Encrypted — Pw variant (password-based)
    // ---------------------------------------------------------------------------

    #[test]
    fn encrypt_pw_decrypt_pw_roundtrip() {
        let pw = b"my-passphrase";
        let pt = b"password protected content";
        let enc = encrypt_with_pw(pt, pw).expect("encrypt_with_pw should succeed");
        let dec = decrypt_with_pw(&enc, pw).expect("decrypt_with_pw should succeed");
        assert_eq!(dec, pt);
    }

    #[test]
    fn decrypt_pw_fails_with_wrong_password() {
        let enc = encrypt_with_pw(b"secret", b"correct-pw").unwrap();
        assert!(decrypt_with_pw(&enc, b"wrong-pw").is_err());
    }

    #[test]
    fn encrypt_pw_produces_fresh_salt_and_nonce_each_call() {
        let pw = b"pw";
        let enc1 = encrypt_with_pw(b"data", pw).unwrap();
        let enc2 = encrypt_with_pw(b"data", pw).unwrap();
        assert_ne!(
            enc1.salt(),
            enc2.salt(),
            "each encryption should use a fresh salt"
        );
        assert_ne!(
            enc1.nonce(),
            enc2.nonce(),
            "each encryption should use a fresh nonce"
        );
    }

    #[test]
    fn pw_enc_encode_decode_roundtrip() {
        let pw = b"encode-pw";
        let enc = encrypt_with_pw(b"encode test", pw).unwrap();
        let encoded = enc.encode();
        let decoded = Encrypted::decode(&encoded).expect("Encrypted::decode should succeed");
        assert_eq!(decoded.salt(), enc.salt());
        assert_eq!(decoded.nonce(), enc.nonce());
        assert_eq!(decoded.ciphertext(), enc.ciphertext());
        let dec = decrypt_with_pw(&decoded, pw).unwrap();
        assert_eq!(dec, b"encode test");
    }

    #[test]
    fn pw_enc_decode_rejects_too_short() {
        // SALT_PREFIX + fewer than SALT_LEN + NONCE_LEN bytes
        let mut raw = Vec::new();
        raw.extend_from_slice(SALT_PREFIX);
        raw.extend_from_slice(&[0u8; SALT_LEN + NONCE_LEN - 1]);
        assert!(
            Encrypted::decode(&base64::engine::general_purpose::STANDARD.encode(&raw)).is_err()
        );
    }

    #[test]
    fn decrypt_pw_variant_rejects_key_decrypt() {
        let enc = encrypt_with_pw(b"data", b"pw").unwrap();
        let key_bytes = sha256(b"k");
        let k = make_key_ref("default", &key_bytes);
        assert!(decrypt(&enc, &[k]).is_err());
    }

    // ---------------------------------------------------------------------------
    // encrypt_with_pw_aad / decrypt_with_pw_aad
    // ---------------------------------------------------------------------------

    #[test]
    fn encrypt_decrypt_pw_aad_roundtrip() {
        let pw = b"pw-aad";
        let aad = 42i32.to_be_bytes();
        let pt = b"pw+aad protected";
        let enc = encrypt_with_pw_aad(pt, pw, &aad).unwrap();
        let dec = decrypt_with_pw_aad(&enc, pw, &aad).unwrap();
        assert_eq!(dec, pt);
    }

    #[test]
    fn decrypt_pw_aad_fails_with_wrong_aad() {
        let pw = b"pw-aad";
        let enc = encrypt_with_pw_aad(b"data", pw, &1i32.to_be_bytes()).unwrap();
        assert!(decrypt_with_pw_aad(&enc, pw, &2i32.to_be_bytes()).is_err());
    }

    #[test]
    fn decrypt_pw_aad_fails_with_wrong_password() {
        let aad = 99i32.to_be_bytes();
        let enc = encrypt_with_pw_aad(b"data", b"real-pw", &aad).unwrap();
        assert!(decrypt_with_pw_aad(&enc, b"fake-pw", &aad).is_err());
    }
}
