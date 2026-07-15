//! S3 storage for pin photos.  Photos are stored as raw bytes under
//! `photos/{org_public_id}/{random}` — the content type lives in the
//! `photos` db table.

use anyhow::anyhow;
use aws_sdk_s3::primitives::ByteStream;

/// Generate a fresh random object key for a photo in `org_public_id`.
pub fn new_photo_key(org_public_id: &str) -> anyhow::Result<String> {
    let rand = common::crypto::rand_bytes(16).map_err(|e| anyhow!("rng error: {e}"))?;
    Ok(format!("photos/{}/{}", org_public_id, hex::encode(rand)))
}

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

/// Delete the object at `key`.  A missing object is not an error.
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

/// Best-effort deletion of a batch of keys; failures are logged and skipped.
/// Used when pins/orgs are deleted and by the org-expiry sweeper.
pub async fn delete_objects_best_effort(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    keys: &[String],
) {
    for key in keys {
        if let Err(e) = delete_object(client, bucket, key).await {
            tracing::warn!("failed deleting photo object {key}: {e}");
        }
    }
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
