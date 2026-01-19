use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use chrono::Utc;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use tracing::info;
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, Result};

/// Manages backups in R2 (S3-compatible) storage
pub struct BackupManager {
    client: Client,
    bucket: String,
}

impl BackupManager {
    /// Create a new backup manager
    pub async fn new(config: &Config) -> Result<Self> {
        let endpoint = format!(
            "https://{}.r2.cloudflarestorage.com",
            config.r2_account_id
        );

        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .endpoint_url(&endpoint)
            .region(aws_sdk_s3::config::Region::new("auto"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                &config.r2_access_key_id,
                &config.r2_secret_access_key,
                None,
                None,
                "r2",
            ))
            .load()
            .await;

        let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(true)
            .build();

        let client = Client::from_conf(s3_config);

        info!("BackupManager initialized for bucket: {}", config.r2_bucket);

        Ok(Self {
            client,
            bucket: config.r2_bucket.clone(),
        })
    }

    /// Upload a database backup (SQL dump) to R2
    /// Returns (object_key, size_bytes)
    pub async fn upload_backup(&self, db_id: Uuid, sql_data: &[u8]) -> Result<(String, i64)> {
        // Compress the SQL dump
        let compressed = compress_gzip(sql_data)?;
        let size = compressed.len() as i64;

        // Generate key: backups/{db_id}/{timestamp}.sql.gz
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let key = format!("backups/{}/{}.sql.gz", db_id, timestamp);

        // Upload to R2
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(compressed))
            .content_type("application/gzip")
            .send()
            .await
            .map_err(|e| AppError::R2(format!("Failed to upload backup: {}", e)))?;

        info!(
            "Uploaded backup for {} to {} ({} bytes compressed)",
            db_id, key, size
        );

        Ok((key, size))
    }

    /// Download a backup from R2 and decompress it
    /// Returns the raw SQL data
    pub async fn download_backup(&self, key: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::R2(format!("Failed to download backup: {}", e)))?;

        let compressed = response
            .body
            .collect()
            .await
            .map_err(|e| AppError::R2(format!("Failed to read backup body: {}", e)))?
            .into_bytes()
            .to_vec();

        let sql_data = decompress_gzip(&compressed)?;

        info!(
            "Downloaded backup from {} ({} bytes -> {} bytes)",
            key,
            compressed.len(),
            sql_data.len()
        );

        Ok(sql_data)
    }

    /// Check if a backup exists
    pub async fn backup_exists(&self, key: &str) -> Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let service_error = e.into_service_error();
                if service_error.is_not_found() {
                    Ok(false)
                } else {
                    Err(AppError::R2(format!(
                        "Failed to check backup: {}",
                        service_error
                    )))
                }
            }
        }
    }

    /// Delete a backup from R2
    pub async fn delete_backup(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::R2(format!("Failed to delete backup: {}", e)))?;

        info!("Deleted backup: {}", key);

        Ok(())
    }
}

/// Compress data with gzip
fn compress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| AppError::BackupFailed(format!("Compression failed: {}", e)))?;
    encoder
        .finish()
        .map_err(|e| AppError::BackupFailed(format!("Compression finalize failed: {}", e)))
}

/// Decompress gzip data
fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| AppError::RestoreFailed(format!("Decompression failed: {}", e)))?;
    Ok(decompressed)
}
