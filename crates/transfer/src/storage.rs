use anyhow::anyhow;
use aws_sdk_s3::primitives::ByteStream;
use futures::StreamExt;
use tokio_util::io::ReaderStream;

/// 8 MB multipart chunk — above the 5 MB S3 minimum.
const CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// Upload a raw byte stream to S3 using multipart upload.
///
/// Streams the Axum request body in [`CHUNK_SIZE`] chunks without buffering the
/// full file in memory. Aborts the multipart upload on any error.
///
/// Returns the number of bytes uploaded.
pub async fn multipart_upload(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    body: axum::body::Body,
    max_bytes: usize,
) -> anyhow::Result<u64> {
    let create = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow!("S3 create_multipart_upload error: {e:?}"))?;
    let upload_id = create
        .upload_id()
        .ok_or_else(|| anyhow!("S3 missing upload_id"))?
        .to_string();

    match upload_parts(client, bucket, key, &upload_id, body, max_bytes).await {
        Ok((parts, total_bytes)) => {
            client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(
                    aws_sdk_s3::types::CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send()
                .await
                .map_err(|e| anyhow!("S3 complete_multipart_upload error: {e:?}"))?;
            Ok(total_bytes)
        }
        Err(e) => {
            let _ = client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await;
            Err(e)
        }
    }
}

async fn upload_parts(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    body: axum::body::Body,
    max_bytes: usize,
) -> anyhow::Result<(Vec<aws_sdk_s3::types::CompletedPart>, u64)> {
    let mut parts: Vec<aws_sdk_s3::types::CompletedPart> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut part_number: i32 = 1;
    let mut chunk_buf: Vec<u8> = Vec::with_capacity(CHUNK_SIZE);

    let mut stream = body.into_data_stream();
    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|e| anyhow!("body stream error: {e}"))?;
        total_bytes += bytes.len() as u64;
        if total_bytes as usize > max_bytes {
            return Err(anyhow!("upload exceeds size limit"));
        }
        chunk_buf.extend_from_slice(&bytes);

        while chunk_buf.len() >= CHUNK_SIZE {
            let chunk: Vec<u8> = chunk_buf.drain(..CHUNK_SIZE).collect();
            let part = upload_one_part(client, bucket, key, upload_id, part_number, chunk).await?;
            parts.push(part);
            part_number += 1;
        }
    }

    if !chunk_buf.is_empty() {
        let part = upload_one_part(client, bucket, key, upload_id, part_number, chunk_buf).await?;
        parts.push(part);
    }

    Ok((parts, total_bytes))
}

async fn upload_one_part(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    part_number: i32,
    data: Vec<u8>,
) -> anyhow::Result<aws_sdk_s3::types::CompletedPart> {
    let resp = client
        .upload_part()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .part_number(part_number)
        .body(ByteStream::from(data))
        .send()
        .await
        .map_err(|e| anyhow!("S3 upload_part {part_number} error: {e}"))?;

    let etag = resp
        .e_tag()
        .ok_or_else(|| anyhow!("S3 upload_part {part_number} missing ETag"))?
        .to_string();

    Ok(aws_sdk_s3::types::CompletedPart::builder()
        .part_number(part_number)
        .e_tag(etag)
        .build())
}

/// Stream an S3 object directly into an Axum response body without buffering.
pub async fn get_object_stream(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> anyhow::Result<axum::body::Body> {
    let resp = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| anyhow!("S3 get_object error for key {key:?}: {e}"))?;

    let async_read = resp.body.into_async_read();
    Ok(axum::body::Body::from_stream(ReaderStream::new(async_read)))
}

/// Delete an S3 object. A missing object is not treated as an error.
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

/// Build an S3 client pointed at the configured endpoint.
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
