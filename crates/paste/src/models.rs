use chrono::{DateTime, Duration, Utc};
use rand::distr::Alphanumeric;
use rand::RngExt;
use sqlx::FromRow;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::storage::{self, BlobHeaderV1};
use crate::Config;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// After this many days the bucket lifecycle policy has already deleted the S3
/// object, so the DB row can be committed without a successful S3 delete.
const S3_BUCKET_TTL_DAYS: i64 = 30;

// ---------------------------------------------------------------------------
// Key helpers
// ---------------------------------------------------------------------------

/// Generate a new random key
fn gen_key(n_chars: usize) -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(n_chars)
        .map(|c| (c as char).to_ascii_lowercase())
        .filter(|c| !matches!(*c, 'l' | '1' | 'i' | 'o' | '0'))
        .collect::<String>()
}

/// Create a new paste.key, making sure it isn't already in use
async fn get_new_key(pool: &common::db::DbPool) -> anyhow::Result<String> {
    let mut n_chars = 5;
    let mut new_key = gen_key(n_chars);
    while Paste::exists(pool, &new_key).await? {
        n_chars += 1;
        new_key = gen_key(n_chars);
    }
    Ok(new_key)
}

// ---------------------------------------------------------------------------
// Internal DB row (no content column — content lives in S3)
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct PasteRow {
    pub id: i32,
    pub key: String,
    pub storage_uri: String,
    pub content_type: String,
    pub date_created: DateTime<Utc>,
    pub date_viewed: DateTime<Utc>,
    pub exp_date: Option<DateTime<Utc>>,
    /// Populated by the sweeper when a paste is enqueued for deletion.
    /// Only materialised here so that `FromRow` doesn't error on the column;
    /// the value is not used in application logic.
    #[allow(dead_code)]
    pub date_queued: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Public structs
// ---------------------------------------------------------------------------

pub struct NewPaste {
    pub content: String,
    pub content_type: String,
}

impl NewPaste {
    /// Insert a new paste.
    ///
    /// Content is **always** encrypted before being written to S3:
    /// - If `user_encryption_key` is `Some`, the user's password is used
    ///   (PBKDF2-HMAC-SHA512 key derivation + fresh salt stored in the blob).
    /// - Otherwise the server's `config.encryption_key` is used directly
    ///   (SHA-256 of the config value → 32-byte AES key, no extra salt).
    ///
    /// The database row ID is included as AES-GCM Additional Authenticated
    /// Data (AAD) so the ciphertext is cryptographically bound to this row.
    pub async fn insert(
        self,
        pool: &common::db::DbPool,
        s3: &aws_sdk_s3::Client,
        config: &Config,
        ttl_seconds: Option<u32>,
        user_encryption_key: Option<&str>,
    ) -> anyhow::Result<Paste> {
        let key = get_new_key(pool).await?;

        let now = Utc::now();
        let exp_date = ttl_seconds.map(|secs| {
            now.checked_add_signed(Duration::seconds(secs as i64))
                .expect("invalid date operation")
        });

        // Open a transaction.  It auto-rolls back on drop if not committed.
        let mut tx = pool.begin().await?;

        // Insert the DB row inside the transaction to obtain the auto-generated
        // `id`, which we use as AAD.  The row is not visible to other readers
        // until we commit (after S3 succeeds).
        let row = sqlx::query_as::<_, PasteRow>(
            "INSERT INTO pastes (key, storage_uri, content_type, date_created, date_viewed, exp_date)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, key, storage_uri, content_type, date_created, date_viewed, exp_date, date_queued",
        )
        .bind(&key)
        .bind(&key) // storage_uri == paste key
        .bind(&self.content_type)
        .bind(now)
        .bind(now)
        .bind(exp_date)
        .fetch_one(&mut *tx)
        .await?;

        // AAD = big-endian bytes of the row id.
        let aad = row.id.to_be_bytes();

        // Encrypt content, compute HMAC signature, and build the blob.
        let enc_key = config.encryption_key.as_key_ref();
        let (header, ciphertext) = BlobHeaderV1::encrypt(
            self.content.as_bytes(),
            config.signing_key.as_bytes(),
            user_encryption_key.map(|k| k.as_bytes()),
            enc_key,
            &aad,
        )?;

        let blob = storage::encode_blob(&header, &ciphertext)?;

        // Upload to S3.  On failure the transaction is dropped and auto-rolled
        // back, so no DB row is committed.
        storage::put_object(s3, &config.s3_bucket, &row.storage_uri, blob).await?;

        // S3 succeeded — commit the DB row.
        tx.commit().await?;

        Ok(Paste {
            id: row.id,
            key: row.key,
            content: self.content,
            content_type: row.content_type,
            date_created: row.date_created,
            date_viewed: row.date_viewed,
            exp_date: row.exp_date,
        })
    }
}

#[derive(Debug)]
pub struct Paste {
    pub id: i32,
    pub key: String,
    pub content: String,
    pub content_type: String,
    pub date_created: DateTime<Utc>,
    pub date_viewed: DateTime<Utc>,
    pub exp_date: Option<DateTime<Utc>>,
}

/// Returns `true` if an S3 error indicates the object was not found.
///
/// Standard S3 `DeleteObject` returns 204 even for non-existent keys, so this
/// is mainly defensive for Tigris / non-standard S3 behaviour.
fn s3_error_is_not_found(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("NoSuchKey") || s.contains("no such key") || s.contains("404")
}

/// A paste that has been identified for deletion and queued to the deletion
/// worker via the [`Paste::queue_outdated_for_deletion`] channel.
pub struct DeletionRequest {
    pub id: i32,
    pub storage_uri: String,
    /// When the paste was first created; used to decide whether the S3
    /// bucket lifecycle policy has already cleaned up the object.
    pub date_created: DateTime<Utc>,
}

impl Paste {
    pub async fn exists(pool: &common::db::DbPool, key: &str) -> anyhow::Result<bool> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pastes WHERE key = $1)")
            .bind(key)
            .fetch_one(pool)
            .await?;
        Ok(exists)
    }

    /// Query the DB for expired / stale pastes and enqueue each one on
    /// `deletion_tx` for the deletion worker to process.
    ///
    /// The DB rows are **not** deleted here; deletion happens transactionally
    /// in [`Paste::attempt_deletion`] after the corresponding S3 object is
    /// confirmed removed.  Returns the number of pastes enqueued.
    ///
    /// To avoid continuously re-queuing the same paste while the deletion
    /// worker is catching up, rows whose `date_queued` is less than one hour
    /// ago are skipped.  `date_queued` is stamped atomically by this query so
    /// concurrent sweeper instances cannot double-queue the same row.
    pub async fn queue_outdated_for_deletion(
        pool: &common::db::DbPool,
        deletion_tx: &mpsc::Sender<DeletionRequest>,
        max_cutoff: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<u64> {
        #[derive(FromRow)]
        struct OutdatedRow {
            id: i32,
            storage_uri: String,
            date_created: DateTime<Utc>,
        }

        // Atomically stamp date_queued = now() on all qualifying rows and
        // return them.  Rows that were queued within the last hour are excluded
        // so the deletion worker has time to process them before they appear
        // again.
        let rows: Vec<OutdatedRow> = sqlx::query_as(
            "UPDATE pastes
             SET date_queued = $3
             WHERE id IN (
                 SELECT id FROM pastes
                 WHERE
                     ((exp_date IS NOT NULL AND exp_date < $1) OR date_viewed < $2)
                     AND (date_queued IS NULL OR date_queued < $3 - INTERVAL '1 hour')
             )
             RETURNING id, storage_uri, date_created",
        )
        .bind(now)
        .bind(max_cutoff)
        .bind(now)
        .fetch_all(pool)
        .await?;

        let count = rows.len() as u64;

        for row in rows {
            let req = DeletionRequest {
                id: row.id,
                storage_uri: row.storage_uri,
                date_created: row.date_created,
            };
            if let Err(e) = deletion_tx.try_send(req) {
                let id = e.into_inner().id;
                warn!("Deletion queue full, skipping paste id={id}");
            }
        }

        Ok(count)
    }

    /// Atomically delete one paste from both the DB and S3.
    ///
    /// Opens a DB transaction, deletes the row, then attempts the S3 delete.
    ///
    /// | S3 outcome | Transaction |
    /// |---|---|
    /// | Success | Commit |
    /// | Object not found (already gone) | Commit |
    /// | Other error, paste ≥ 30 days old | Commit (bucket TTL cleaned S3) |
    /// | Other error, paste < 30 days old | Rollback (retry on next sweep) |
    ///
    /// If the DB row is already gone (a previous attempt succeeded), the
    /// function returns `Ok(())` immediately.
    pub async fn attempt_deletion(
        pool: &common::db::DbPool,
        s3: &aws_sdk_s3::Client,
        config: &Config,
        req: &DeletionRequest,
    ) -> anyhow::Result<()> {
        let mut tx = pool.begin().await?;

        let rows_affected = sqlx::query("DELETE FROM pastes WHERE id = $1")
            .bind(req.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        if rows_affected == 0 {
            // Row was already deleted by a previous successful attempt.
            // Let the transaction drop (auto-rollback; harmless no-op).
            return Ok(());
        }

        // Attempt S3 deletion.
        match storage::delete_object(s3, &config.s3_bucket, &req.storage_uri).await {
            Ok(_) => {
                tx.commit().await?;
                Ok(())
            }
            Err(ref e) if s3_error_is_not_found(e) => {
                // Object already gone — safe to commit the DB deletion.
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let age_days = (Utc::now() - req.date_created).num_days();
                if age_days >= S3_BUCKET_TTL_DAYS {
                    // The bucket lifecycle policy has already removed the
                    // object, so we can commit even without an S3 success.
                    info!(
                        "S3 deletion failed for paste id={} (age={age_days}d ≥ {S3_BUCKET_TTL_DAYS}d TTL); \
                         committing DB deletion: {e}",
                        req.id
                    );
                    tx.commit().await?;
                    Ok(())
                } else {
                    // Transient S3 error — roll back so the sweeper retries.
                    warn!(
                        "S3 deletion failed for paste id={}, rolling back for retry: {e}",
                        req.id
                    );
                    tx.rollback().await?;
                    Err(anyhow::anyhow!(
                        "S3 deletion failed for paste id={}: {e}",
                        req.id
                    ))
                }
            }
        }
    }

    /// Update `date_viewed`, fetch from S3, decrypt, verify signature, and return.
    ///
    /// `user_enc_key` is the user-supplied password, required only when the
    /// paste was stored with user-key encryption (i.e. the blob header has a salt).
    pub async fn touch_and_get(
        pool: &common::db::DbPool,
        s3: &aws_sdk_s3::Client,
        config: &Config,
        key: &str,
        user_enc_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let row = sqlx::query_as::<_, PasteRow>(
            "UPDATE pastes SET date_viewed = $1 WHERE key = $2
             RETURNING id, key, storage_uri, content_type, date_created, date_viewed, exp_date, date_queued",
        )
        .bind(Utc::now())
        .bind(key)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("paste not found"))?;

        // Expiry check — route through attempt_deletion for transactional cleanup.
        if let Some(exp_date) = row.exp_date {
            if exp_date <= Utc::now() {
                let req = DeletionRequest {
                    id: row.id,
                    storage_uri: row.storage_uri.clone(),
                    date_created: row.date_created,
                };
                if let Err(e) = Paste::attempt_deletion(pool, s3, config, &req).await {
                    warn!("Failed to clean up expired paste id={}: {e}", req.id);
                }
                return Err(anyhow::anyhow!("paste expired"));
            }
        }

        // Fetch blob from S3 and parse header.
        let blob = storage::get_object(s3, &config.s3_bucket, &row.storage_uri).await?;
        let (header, ciphertext) = storage::decode_blob(&blob)?;

        // AAD must match what was used during encryption.
        let aad = row.id.to_be_bytes();

        // Decrypt — the header version drives the decryption logic.
        let plaintext_bytes = header.decrypt(
            &ciphertext,
            user_enc_key.map(|k| k.as_bytes()),
            &[config.encryption_key.as_key_ref()],
            &aad,
        )?;

        let content =
            String::from_utf8(plaintext_bytes).map_err(|e| anyhow::anyhow!("utf8: {e}"))?;

        // Verify HMAC signature from the blob header.
        if !common::crypto::hmac_verify(&content, header.sig(), config.signing_key.as_bytes()) {
            error!("HMAC verification failed for paste key={key}");
            return Err(anyhow::anyhow!("decryption failure"));
        }

        Ok(Paste {
            id: row.id,
            key: row.key,
            content,
            content_type: row.content_type,
            date_created: row.date_created,
            date_viewed: row.date_viewed,
            exp_date: row.exp_date,
        })
    }
}

pub static CONTENT_TYPES: [&str; 147] = [
    "text",
    "abap",
    "abc",
    "actionscript",
    "ada",
    "apache_conf",
    "applescript",
    "asciidoc",
    "assembly_x86",
    "autohotkey",
    "batchfile",
    "bro",
    "c9search",
    "c_cpp",
    "cirru",
    "clojure",
    "cobol",
    "coffee",
    "coldfusion",
    "csharp",
    "css",
    "curly",
    "dart",
    "diff",
    "django",
    "d",
    "dockerfile",
    "dot",
    "drools",
    "eiffel",
    "ejs",
    "elixir",
    "elm",
    "erlang",
    "forth",
    "fortran",
    "ftl",
    "gcode",
    "gherkin",
    "gitignore",
    "glsl",
    "gobstones",
    "golang",
    "graphqlschema",
    "groovy",
    "haml",
    "handlebars",
    "haskell_cabal",
    "haskell",
    "haxe",
    "hjson",
    "html_elixir",
    "html",
    "html_ruby",
    "ini",
    "io",
    "jack",
    "jade",
    "java",
    "javascript",
    "jsoniq",
    "json",
    "jsp",
    "jsx",
    "julia",
    "kotlin",
    "latex",
    "lean",
    "less",
    "liquid",
    "lisp",
    "live_script",
    "livescript",
    "logiql",
    "lsl",
    "lua",
    "luapage",
    "lucene",
    "makefile",
    "markdown",
    "mask",
    "matlab",
    "maze",
    "mel",
    "mips_assembler",
    "mipsassembler",
    "mushcode",
    "mysql",
    "nix",
    "nsis",
    "objectivec",
    "ocaml",
    "pascal",
    "perl",
    "pgsql",
    "php",
    "pig",
    "plain_text",
    "powershell",
    "praat",
    "prolog",
    "properties",
    "protobuf",
    "python",
    "razor",
    "rdoc",
    "rhtml",
    "r",
    "rst",
    "ruby",
    "rust",
    "sass",
    "scad",
    "scala",
    "scheme",
    "scss",
    "sh",
    "sjs",
    "smarty",
    "snippets",
    "soy_template",
    "space",
    "sparql",
    "sql",
    "sqlserver",
    "stylus",
    "svg",
    "swift",
    "swig",
    "tcl",
    "tex",
    "textile",
    "text",
    "toml",
    "tsx",
    "turtle",
    "twig",
    "typescript",
    "vala",
    "vbscript",
    "velocity",
    "verilog",
    "vhdl",
    "wollok",
    "xml",
    "xquery",
    "yaml",
];

#[cfg(test)]
mod tests {
    use super::*;

    const AMBIGUOUS: &[char] = &['l', '1', 'i', 'o', '0'];

    #[test]
    fn test_gen_key_contains_no_ambiguous_chars() {
        for _ in 0..200 {
            let key = gen_key(20);
            for ch in key.chars() {
                assert!(
                    !AMBIGUOUS.contains(&ch),
                    "gen_key produced ambiguous char '{}' in key {:?}",
                    ch,
                    key
                );
            }
        }
    }

    #[test]
    fn test_gen_key_is_lowercase() {
        for _ in 0..100 {
            let key = gen_key(20);
            assert_eq!(
                key,
                key.to_lowercase(),
                "gen_key should produce only lowercase characters"
            );
        }
    }

    #[test]
    fn test_gen_key_length_at_most_n() {
        // Filtering may reduce length; it should never exceed n_chars.
        for n in [5, 8, 16] {
            let key = gen_key(n);
            assert!(
                key.len() <= n,
                "gen_key({}) produced key of length {} (expected <= {})",
                n,
                key.len(),
                n
            );
        }
    }

    #[test]
    fn test_gen_key_alphanumeric() {
        for _ in 0..50 {
            let key = gen_key(30);
            for ch in key.chars() {
                assert!(
                    ch.is_alphanumeric(),
                    "gen_key produced non-alphanumeric char '{}'",
                    ch
                );
            }
        }
    }

    #[test]
    fn test_hmac_sign_and_verify_roundtrip() {
        let content = "hello, world!";
        let key = b"test-signing-key-32-bytes-padding";
        let sig = common::crypto::hmac_sign(content, key);
        assert!(
            common::crypto::hmac_verify(content, &sig, key),
            "hmac_verify should return true for valid signature"
        );
    }

    #[test]
    fn test_hmac_verify_fails_with_wrong_content() {
        let key = b"test-signing-key-32-bytes-padding";
        let sig = common::crypto::hmac_sign("original", key);
        assert!(
            !common::crypto::hmac_verify("tampered", &sig, key),
            "hmac_verify should return false when content has been changed"
        );
    }

    #[test]
    fn test_hmac_verify_fails_with_wrong_key() {
        let content = "some content";
        let sig = common::crypto::hmac_sign(content, b"key-one-32-bytes-padding-padding");
        assert!(
            !common::crypto::hmac_verify(content, &sig, b"key-two-32-bytes-padding-padding"),
            "hmac_verify should return false when signing key differs"
        );
    }

    #[test]
    fn test_encrypt_decrypt_with_password_roundtrip() {
        let plaintext = b"super secret message";
        let password = b"user-password";
        let enc = common::crypto::encrypt_with_pw(plaintext, password)
            .expect("encryption should succeed");
        let dec =
            common::crypto::decrypt_with_pw(&enc, password).expect("decryption should succeed");
        assert_eq!(
            dec, plaintext,
            "decrypted bytes should match original plaintext"
        );
    }

    #[test]
    fn test_decrypt_with_wrong_password_fails() {
        let plaintext = b"another secret";
        let enc = common::crypto::encrypt_with_pw(plaintext, b"correct-password")
            .expect("encryption should succeed");
        let result = common::crypto::decrypt_with_pw(&enc, b"wrong-password");
        assert!(
            result.is_err(),
            "decryption with wrong password should fail"
        );
    }

    #[test]
    fn test_pwenc_encode_decode_roundtrip() {
        let plaintext = b"encode decode test";
        let enc =
            common::crypto::encrypt_with_pw(plaintext, b"pw").expect("encryption should succeed");
        let encoded = enc.encode();
        let decoded = common::crypto::Encrypted::decode(&encoded).expect("decode should succeed");
        let dec =
            common::crypto::decrypt_with_pw(&decoded, b"pw").expect("decryption should succeed");
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn test_encrypt_with_aad_roundtrip() {
        let key_bytes = common::crypto::sha256(b"some-config-key");
        let key = common::crypto::KeyRef {
            id: "default",
            key: &key_bytes,
        };
        let plaintext = b"content with aad";
        let aad = 42i32.to_be_bytes();
        let enc = common::crypto::encrypt_with_aad(plaintext, key, &aad)
            .expect("encryption should succeed");
        let dec = common::crypto::decrypt_with_aad(&enc, &[key], &aad)
            .expect("decryption should succeed");
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn test_encrypt_with_aad_wrong_aad_fails() {
        let key_bytes = common::crypto::sha256(b"some-config-key");
        let key = common::crypto::KeyRef {
            id: "default",
            key: &key_bytes,
        };
        let plaintext = b"content with aad";
        let enc = common::crypto::encrypt_with_aad(plaintext, key, &1i32.to_be_bytes())
            .expect("encryption should succeed");
        // Different AAD — decryption must fail
        let result = common::crypto::decrypt_with_aad(&enc, &[key], &2i32.to_be_bytes());
        assert!(result.is_err(), "wrong AAD should cause decryption failure");
    }

    #[test]
    fn test_encrypt_with_pw_aad_roundtrip() {
        let plaintext = b"user encrypted with aad";
        let password = b"hunter2";
        let aad = 7i32.to_be_bytes();
        let enc = common::crypto::encrypt_with_pw_aad(plaintext, password, &aad)
            .expect("encryption should succeed");
        let dec = common::crypto::decrypt_with_pw_aad(&enc, password, &aad)
            .expect("decryption should succeed");
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn test_content_types_includes_text() {
        assert!(
            CONTENT_TYPES.contains(&"text"),
            "CONTENT_TYPES should include 'text'"
        );
    }

    #[test]
    fn test_content_types_not_empty() {
        assert!(!CONTENT_TYPES.is_empty());
    }
}
