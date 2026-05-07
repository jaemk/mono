//! S3 storage backend for paste content.
//!
//! ## Blob format
//!
//! Every object stored in S3 is a binary blob with the following layout:
//!
//! ```text
//! $version . $header . $content
//! ```
//!
//! - `$version`  – msgpack-serialised [`BlobVersion`], base64url-encoded (no padding)
//! - `$header`   – msgpack-serialised header struct for the version (e.g. [`BlobHeaderV1`]),
//!   base64url-encoded (no padding)
//! - `$content`  – raw ciphertext bytes
//! - `.`         – ASCII dot byte (`0x2E`) used as the section separator
//!
//! The [`BlobHeaderV1`] header carries the HMAC signature of the plaintext,
//! the AES-GCM nonce, and — for user-key-encrypted pastes — the PBKDF2 salt.

use anyhow::anyhow;
use base64::Engine as _;

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;
use aws_sdk_s3::primitives::ByteStream;
use serde::{Deserialize, Serialize};

const CURRENT_VERSION: u32 = 1;
const SEP: u8 = b'.';

// ---------------------------------------------------------------------------
// Version envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct BlobVersion {
    version: u32,
}

// ---------------------------------------------------------------------------
// Decrypt trait
// ---------------------------------------------------------------------------

/// Implemented by each versioned header; encapsulates decrypt logic.
pub trait BlobHeaderDecrypt {
    /// Decrypt `ciphertext` and return the plaintext bytes.
    ///
    /// `user_enc_key` is the user-supplied password (bytes).  Pass `None` when
    /// the paste was encrypted with the server's default key.
    /// `keys` is the server-side key ring; the correct key is selected by the
    /// ID stored in the blob header.
    /// `aad` is the Additional Authenticated Data bound to this ciphertext.
    fn decrypt(
        &self,
        ciphertext: &[u8],
        user_enc_key: Option<&[u8]>,
        keys: &[common::crypto::KeyRef<'_>],
        aad: &[u8],
    ) -> anyhow::Result<Vec<u8>>;

    /// HMAC-SHA256 hex signature of the original plaintext, for post-decrypt
    /// verification.
    fn sig(&self) -> &str;
}

// ---------------------------------------------------------------------------
// BlobHeaderV1
// ---------------------------------------------------------------------------

/// Metadata stored in the header section of every V1 S3 blob.
#[derive(Debug, Serialize, Deserialize)]
pub struct BlobHeaderV1 {
    /// HMAC-SHA256 hex signature of the **plaintext** content.
    pub sig: String,
    /// Base64url-encoded (no padding) AES-GCM nonce (12 bytes).
    pub nonce: String,
    /// Base64url-encoded (no padding) PBKDF2 salt (64 bytes).
    ///
    /// `None` means the server's default key was used (no KDF required).
    pub salt: Option<String>,
    /// ID of the server-side key used to encrypt this blob.
    ///
    /// `None` on legacy blobs — treated as `"default"` during decryption.
    #[serde(default)]
    pub key_id: Option<String>,
}

impl BlobHeaderV1 {
    /// Encrypt `plaintext` and produce a `(BlobHeaderV1, ciphertext)` pair.
    ///
    /// If `user_enc_key` is `Some`, PBKDF2-HMAC-SHA512 key derivation is used
    /// with a fresh random salt; otherwise `key` is used directly and its ID
    /// is stored in the header for later key-ring lookup.
    pub fn encrypt(
        plaintext: &[u8],
        signing_key: &[u8],
        user_enc_key: Option<&[u8]>,
        key: common::crypto::KeyRef<'_>,
        aad: &[u8],
    ) -> anyhow::Result<(Self, Vec<u8>)> {
        let sig =
            common::crypto::hmac_sign(std::str::from_utf8(plaintext).unwrap_or(""), signing_key);

        if let Some(user_key) = user_enc_key {
            let pwenc = common::crypto::encrypt_with_pw_aad(plaintext, user_key, aad)
                .map_err(|e| anyhow!("encryption error: {e}"))?;
            let header = BlobHeaderV1 {
                sig,
                nonce: B64.encode(pwenc.nonce()),
                salt: pwenc.salt().map(|s| B64.encode(s)),
                key_id: None,
            };
            Ok((header, pwenc.ciphertext().to_vec()))
        } else {
            let enc = common::crypto::encrypt_with_aad(plaintext, key, aad)
                .map_err(|e| anyhow!("encryption error: {e}"))?;
            let header = BlobHeaderV1 {
                sig,
                nonce: B64.encode(enc.nonce()),
                salt: None,
                key_id: Some(key.id.to_string()),
            };
            Ok((header, enc.ciphertext().to_vec()))
        }
    }
}

impl BlobHeaderDecrypt for BlobHeaderV1 {
    fn decrypt(
        &self,
        ciphertext: &[u8],
        user_enc_key: Option<&[u8]>,
        keys: &[common::crypto::KeyRef<'_>],
        aad: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        if let Some(salt_b64) = &self.salt {
            let user_key = user_enc_key.ok_or_else(|| anyhow!("decryption failure"))?;
            let salt = B64
                .decode(salt_b64)
                .map_err(|e| anyhow!("base64 salt: {e}"))?;
            let nonce = B64
                .decode(&self.nonce)
                .map_err(|e| anyhow!("base64 nonce: {e}"))?;
            let pwenc = common::crypto::Encrypted::Pw {
                ciphertext: ciphertext.to_vec(),
                nonce,
                salt,
            };
            common::crypto::decrypt_with_pw_aad(&pwenc, user_key, aad)
                .map_err(|_| anyhow!("failed decrypting content"))
        } else {
            let nonce = B64
                .decode(&self.nonce)
                .map_err(|e| anyhow!("base64 nonce: {e}"))?;
            // Legacy blobs without key_id default to "default".
            let key_id = self.key_id.as_deref().unwrap_or("default");
            let enc = common::crypto::Encrypted::Key {
                id: key_id.to_string(),
                ciphertext: ciphertext.to_vec(),
                nonce,
            };
            common::crypto::decrypt_with_aad(&enc, keys, aad)
                .map_err(|_| anyhow!("decryption failure"))
        }
    }

    fn sig(&self) -> &str {
        &self.sig
    }
}

// ---------------------------------------------------------------------------
// Encode / decode
// ---------------------------------------------------------------------------

/// Encode a `(BlobHeaderV1, ciphertext)` pair into the blob format.
///
/// Output: `base64url(msgpack(BlobVersion)) . base64url(msgpack(header)) . ciphertext`
pub fn encode_blob(header: &BlobHeaderV1, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let version_bytes = rmp_serde::to_vec(&BlobVersion {
        version: CURRENT_VERSION,
    })
    .map_err(|e| anyhow!("msgpack version: {e}"))?;
    let header_bytes = rmp_serde::to_vec(header).map_err(|e| anyhow!("msgpack header: {e}"))?;

    let version_b64 = B64.encode(&version_bytes);
    let header_b64 = B64.encode(&header_bytes);

    let mut out =
        Vec::with_capacity(version_b64.len() + 1 + header_b64.len() + 1 + ciphertext.len());
    out.extend_from_slice(version_b64.as_bytes());
    out.push(SEP);
    out.extend_from_slice(header_b64.as_bytes());
    out.push(SEP);
    out.extend_from_slice(ciphertext);
    Ok(out)
}

/// Decode a blob back into a versioned header (as `Box<dyn BlobHeaderDecrypt>`) and
/// the raw ciphertext bytes.
pub fn decode_blob(blob: &[u8]) -> anyhow::Result<(Box<dyn BlobHeaderDecrypt>, Vec<u8>)> {
    // Split on first '.'
    let sep1 = blob
        .iter()
        .position(|&b| b == SEP)
        .ok_or_else(|| anyhow!("missing first '.' separator in blob"))?;
    let version_b64 = &blob[..sep1];
    let rest = &blob[sep1 + 1..];

    // Decode version
    let version_bytes = B64
        .decode(version_b64)
        .map_err(|e| anyhow!("base64 version: {e}"))?;
    let blob_version: BlobVersion =
        rmp_serde::from_slice(&version_bytes).map_err(|e| anyhow!("msgpack version: {e}"))?;

    // Split on second '.'
    let sep2 = rest
        .iter()
        .position(|&b| b == SEP)
        .ok_or_else(|| anyhow!("missing second '.' separator in blob"))?;
    let header_b64 = &rest[..sep2];
    let ciphertext = rest[sep2 + 1..].to_vec();

    // Decode header based on version
    let header_bytes = B64
        .decode(header_b64)
        .map_err(|e| anyhow!("base64 header: {e}"))?;

    let header: Box<dyn BlobHeaderDecrypt> = match blob_version.version {
        1 => {
            let h: BlobHeaderV1 = rmp_serde::from_slice(&header_bytes)
                .map_err(|e| anyhow!("msgpack header v1: {e}"))?;
            Box::new(h)
        }
        v => return Err(anyhow!("unsupported blob version: {v}")),
    };

    Ok((header, ciphertext))
}

// ---------------------------------------------------------------------------
// S3 operations
// ---------------------------------------------------------------------------

/// Upload `data` to `bucket` at `key`, replacing any existing object.
pub async fn put_object(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(data))
        .send()
        .await
        .map_err(|e| anyhow!("S3 put_object error for key {key:?}: {e}"))?;
    Ok(())
}

/// Download the object at `key` from `bucket` and return its bytes.
pub async fn get_object(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> anyhow::Result<Vec<u8>> {
    let resp = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow!("S3 get_object error for key {key:?}: {e}"))?;
    let bytes = resp
        .body
        .collect()
        .await
        .map_err(|e| anyhow!("S3 read body error for key {key:?}: {e}"))?
        .into_bytes();
    Ok(bytes.to_vec())
}

/// Delete the object at `key` from `bucket`.  A missing object is not an error.
pub async fn delete_object(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> anyhow::Result<()> {
    client
        .delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow!("S3 delete_object error for key {key:?}: {e}"))?;
    Ok(())
}

/// Build an S3 client pointed at the given custom endpoint.
///
/// Credentials are read from the standard AWS environment variables
/// (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`).
pub async fn create_client(endpoint_url: &str, region: &str) -> aws_sdk_s3::Client {
    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
        .endpoint_url(endpoint_url)
        .load()
        .await;
    let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
        .region(aws_sdk_s3::config::Region::new(region.to_owned()))
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> BlobHeaderV1 {
        BlobHeaderV1 {
            sig: "deadbeef".to_string(),
            nonce: B64.encode(b"123456789012"), // 12 bytes
            salt: None,
            key_id: None,
        }
    }

    #[test]
    fn roundtrip_without_salt() {
        let header = sample_header();
        let ciphertext = b"some ciphertext bytes";
        let blob = encode_blob(&header, ciphertext).unwrap();
        let (dec_header, dec_ct) = decode_blob(&blob).unwrap();
        assert_eq!(dec_header.sig(), header.sig);
        assert_eq!(dec_ct, ciphertext);
    }

    #[test]
    fn roundtrip_with_salt() {
        let header = BlobHeaderV1 {
            sig: "aabbcc".to_string(),
            nonce: B64.encode(b"123456789012"),
            salt: Some(B64.encode(b"1234567890123456")),
            key_id: None,
        };
        let ciphertext = b"encrypted";
        let blob = encode_blob(&header, ciphertext).unwrap();
        let (_, dec_ct) = decode_blob(&blob).unwrap();
        assert_eq!(dec_ct, ciphertext);
    }

    #[test]
    fn decode_rejects_missing_separator() {
        assert!(decode_blob(b"nodothere").is_err());
    }

    #[test]
    fn decode_rejects_short_blob() {
        assert!(decode_blob(&[0u8; 3]).is_err());
    }

    #[test]
    fn decode_rejects_wrong_version() {
        // Build a blob with version=99
        let bad_ver = rmp_serde::to_vec(&BlobVersion { version: 99 }).unwrap();
        let ver_b64 = B64.encode(&bad_ver);
        let hdr_bytes = rmp_serde::to_vec(&sample_header()).unwrap();
        let hdr_b64 = B64.encode(&hdr_bytes);
        let mut blob = Vec::new();
        blob.extend_from_slice(ver_b64.as_bytes());
        blob.push(SEP);
        blob.extend_from_slice(hdr_b64.as_bytes());
        blob.push(SEP);
        blob.extend_from_slice(b"ct");
        assert!(decode_blob(&blob).is_err());
    }

    #[test]
    fn roundtrip_with_empty_ciphertext() {
        let header = sample_header();
        let blob = encode_blob(&header, b"").unwrap();
        let (dec_header, dec_ct) = decode_blob(&blob).unwrap();
        assert_eq!(dec_header.sig(), header.sig);
        assert_eq!(dec_ct, b"");
    }

    #[test]
    fn blob_has_two_dot_separators() {
        let blob = encode_blob(&sample_header(), b"ct").unwrap();
        let dot_count = blob.iter().filter(|&&b| b == SEP).count();
        assert_eq!(dot_count, 2, "blob must contain exactly two '.' separators");
    }

    #[test]
    fn roundtrip_preserves_long_ciphertext() {
        let ciphertext: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let blob = encode_blob(&sample_header(), &ciphertext).unwrap();
        let (_, dec_ct) = decode_blob(&blob).unwrap();
        assert_eq!(dec_ct, ciphertext);
    }

    #[test]
    fn encrypt_decrypt_default_key_roundtrip() {
        let plaintext = b"hello world";
        let signing_key = b"signing-key-32-bytes-padding-xxx";
        let aes_key_bytes = common::crypto::sha256(b"config-enc-key");
        let aes_key = common::crypto::KeyRef {
            id: "default",
            key: &aes_key_bytes,
        };
        let aad = 1i32.to_be_bytes();

        let (header, ct) =
            BlobHeaderV1::encrypt(plaintext, signing_key, None, aes_key, &aad).unwrap();
        let blob = encode_blob(&header, &ct).unwrap();
        let (dec_header, dec_ct) = decode_blob(&blob).unwrap();
        let plain = dec_header.decrypt(&dec_ct, None, &[aes_key], &aad).unwrap();
        assert_eq!(plain, plaintext);
    }

    #[test]
    fn encrypt_decrypt_user_key_roundtrip() {
        let plaintext = b"secret paste";
        let signing_key = b"signing-key-32-bytes-padding-xxx";
        let aes_key_bytes = common::crypto::sha256(b"config-enc-key");
        let aes_key = common::crypto::KeyRef {
            id: "default",
            key: &aes_key_bytes,
        };
        let aad = 2i32.to_be_bytes();
        let user_key = b"my-password";

        let (header, ct) =
            BlobHeaderV1::encrypt(plaintext, signing_key, Some(user_key), aes_key, &aad).unwrap();
        assert!(header.salt.is_some(), "user-key encryption must set a salt");
        let blob = encode_blob(&header, &ct).unwrap();
        let (dec_header, dec_ct) = decode_blob(&blob).unwrap();
        let plain = dec_header
            .decrypt(&dec_ct, Some(user_key), &[aes_key], &aad)
            .unwrap();
        assert_eq!(plain, plaintext);
    }

    #[test]
    fn decrypt_user_key_requires_key() {
        let plaintext = b"needs a key";
        let signing_key = b"signing-key-32-bytes-padding-xxx";
        let aes_key_bytes = common::crypto::sha256(b"config-enc-key");
        let aes_key = common::crypto::KeyRef {
            id: "default",
            key: &aes_key_bytes,
        };
        let aad = 3i32.to_be_bytes();

        let (header, ct) =
            BlobHeaderV1::encrypt(plaintext, signing_key, Some(b"pw"), aes_key, &aad).unwrap();
        let blob = encode_blob(&header, &ct).unwrap();
        let (dec_header, dec_ct) = decode_blob(&blob).unwrap();
        // No user key supplied → must fail with "decryption failure"
        let result = dec_header.decrypt(&dec_ct, None, &[aes_key], &aad);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("decryption failure"));
    }
}
