//! Thumbnail object storage abstractions.

mod s3;

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;
use url::Url;

pub use s3::S3ThumbnailStore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredThumbnail {
    pub width: u32,
    pub height: u32,
}

#[async_trait]
pub trait ThumbnailStore: Send + Sync {
    async fn head(&self, key: &str) -> Result<Option<StoredThumbnail>>;
    async fn get(&self, key: &str) -> Result<Vec<u8>>;
    async fn put(&self, key: &str, bytes: Vec<u8>, thumbnail: &StoredThumbnail) -> Result<()>;
    async fn presign_get(&self, key: &str, ttl: Duration) -> Result<Url>;
    async fn delete(&self, key: &str) -> Result<()>;
}

type MemoryObjects = HashMap<String, (Vec<u8>, StoredThumbnail)>;

/// Deterministic test store that also makes service tests independent of S3.
#[derive(Clone, Default)]
pub struct MemoryThumbnailStore {
    objects: Arc<RwLock<MemoryObjects>>,
}

impl MemoryThumbnailStore {
    pub async fn object_count(&self) -> usize {
        self.objects.read().await.len()
    }
}

#[async_trait]
impl ThumbnailStore for MemoryThumbnailStore {
    async fn head(&self, key: &str) -> Result<Option<StoredThumbnail>> {
        Ok(self
            .objects
            .read()
            .await
            .get(key)
            .map(|(_, metadata)| metadata.clone()))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        self.objects
            .read()
            .await
            .get(key)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| anyhow::anyhow!("Thumbnail object not found: {key}"))
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, thumbnail: &StoredThumbnail) -> Result<()> {
        self.objects
            .write()
            .await
            .insert(key.to_owned(), (bytes, thumbnail.clone()));
        Ok(())
    }

    async fn presign_get(&self, key: &str, _ttl: Duration) -> Result<Url> {
        Ok(Url::parse(&format!(
            "https://thumbnail.test/{}",
            urlencoding::encode(key)
        ))?)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.objects.write().await.remove(key);
        Ok(())
    }
}
