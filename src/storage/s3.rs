use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::{
    config::{Credentials, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
};
use url::Url;

use crate::{
    config::ThumbnailConfig,
    storage::{StoredThumbnail, ThumbnailStore},
};

#[derive(Clone)]
pub struct S3ThumbnailStore {
    client: aws_sdk_s3::Client,
    signing_client: aws_sdk_s3::Client,
    bucket: String,
}

impl S3ThumbnailStore {
    pub async fn new(config: &ThumbnailConfig) -> Result<Self> {
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
            None,
            None,
            "tandoor-mcp",
        );
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials);
        if let Some(endpoint) = &config.endpoint {
            loader = loader.endpoint_url(endpoint);
        }
        let shared = loader.load().await;
        let service_config = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(config.force_path_style)
            .build();
        let signing_config = {
            let mut builder = aws_sdk_s3::config::Builder::from(&shared)
                .force_path_style(config.force_path_style);
            if let Some(public_base_url) = &config.public_base_url {
                builder = builder.endpoint_url(public_base_url.as_str());
            }
            builder.build()
        };
        Ok(Self {
            client: aws_sdk_s3::Client::from_conf(service_config),
            signing_client: aws_sdk_s3::Client::from_conf(signing_config),
            bucket: config.bucket.clone(),
        })
    }

    /// Verify the exact prefix permissions used by the thumbnail pipeline.
    ///
    /// A prefix-scoped IAM policy can legitimately reject `HeadBucket`, so use
    /// the same `ListBucket` condition documented for production instead.
    pub async fn check_bucket(&self, prefix: &str) -> Result<()> {
        self.client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(prefix)
            .max_keys(1)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Could not list S3 thumbnail prefix {prefix} in bucket {}",
                    self.bucket
                )
            })?;
        Ok(())
    }
}

#[async_trait]
impl ThumbnailStore for S3ThumbnailStore {
    async fn head(&self, key: &str) -> Result<Option<StoredThumbnail>> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => Ok(Some(metadata_dimensions(output.metadata()))),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_not_found()) =>
            {
                Ok(None)
            }
            Err(error) => Err(error).context("Could not inspect S3 thumbnail"),
        }
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .context("Could not read S3 thumbnail")?;
        Ok(output
            .body
            .collect()
            .await
            .context("Could not download S3 thumbnail")?
            .into_bytes()
            .to_vec())
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, thumbnail: &StoredThumbnail) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type("image/jpeg")
            .cache_control("private, max-age=86400")
            .metadata("width", thumbnail.width.to_string())
            .metadata("height", thumbnail.height.to_string())
            .body(ByteStream::from(bytes))
            .send()
            .await
            .context("Could not store S3 thumbnail")?;
        Ok(())
    }

    async fn presign_get(&self, key: &str, ttl: Duration) -> Result<Url> {
        let config =
            PresigningConfig::expires_in(ttl).context("Invalid thumbnail URL expiration")?;
        let request = self
            .signing_client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .response_content_type("image/jpeg")
            .presigned(config)
            .await
            .context("Could not sign thumbnail URL")?;
        Url::parse(request.uri()).context("S3 returned an invalid presigned URL")
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .context("Could not delete S3 thumbnail")?;
        Ok(())
    }
}

fn metadata_dimensions(metadata: Option<&HashMap<String, String>>) -> StoredThumbnail {
    let parse = |name| {
        metadata
            .and_then(|values| values.get(name))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    StoredThumbnail {
        width: parse("width"),
        height: parse("height"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run explicitly against MinIO after exporting the S3 variables documented
    /// in `docs/thumbnails.md`.
    #[tokio::test]
    #[ignore = "requires an S3-compatible service"]
    async fn minio_round_trip() {
        let config = ThumbnailConfig::from_env()
            .unwrap()
            .expect("S3_BUCKET must be configured");
        let store = S3ThumbnailStore::new(&config).await.unwrap();
        store.check_bucket(&config.prefix).await.unwrap();
        let key = format!("{}integration-test.jpg", config.prefix);
        let metadata = StoredThumbnail {
            width: 1,
            height: 1,
        };
        store
            .put(&key, b"\xff\xd8\xff\xd9".to_vec(), &metadata)
            .await
            .unwrap();
        assert_eq!(store.head(&key).await.unwrap(), Some(metadata));
        assert!(!store.get(&key).await.unwrap().is_empty());
        assert_eq!(
            store
                .presign_get(&key, Duration::from_secs(60))
                .await
                .unwrap()
                .scheme(),
            "http"
        );
        store.delete(&key).await.unwrap();
    }
}
