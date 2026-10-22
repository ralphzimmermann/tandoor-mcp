//! Coordination between authenticated original-image fetching and thumbnail storage.

use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::{
    config::ThumbnailConfig,
    storage::{StoredThumbnail, ThumbnailStore},
    thumbnail::make_thumbnail,
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct ThumbnailRef {
    pub key: String,
    pub url: Url,
    pub expires_at: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct ThumbnailService {
    store: Arc<dyn ThumbnailStore>,
    prefix: String,
    max_edge: u32,
    url_ttl: Duration,
    resource_origin: String,
    known: Arc<Mutex<HashMap<String, StoredThumbnail>>>,
    generation_slots: Arc<Semaphore>,
}

impl ThumbnailService {
    pub fn new(
        store: Arc<dyn ThumbnailStore>,
        config: &ThumbnailConfig,
        resource_origin: String,
    ) -> Self {
        Self {
            store,
            prefix: config.prefix.clone(),
            max_edge: config.max_edge,
            url_ttl: config.url_ttl,
            resource_origin,
            known: Arc::new(Mutex::new(HashMap::new())),
            generation_slots: Arc::new(Semaphore::new(4)),
        }
    }

    pub fn resource_origin(&self) -> &str {
        &self.resource_origin
    }

    pub fn key_for(&self, recipe_id: i32, image_path: &str) -> String {
        let stable_path = Url::parse(image_path)
            .map(|mut url| {
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            })
            .unwrap_or_else(|_| {
                image_path
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(image_path)
                    .to_owned()
            });
        let digest = Sha256::digest(stable_path.as_bytes());
        format!(
            "{}{recipe_id}/{:x}-{}.jpg",
            self.prefix, digest, self.max_edge
        )
    }

    pub async fn thumbnail_for<F, Fut>(
        &self,
        recipe_id: i32,
        image_path: &str,
        fetch_original: F,
    ) -> Result<ThumbnailRef>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<Vec<u8>>> + Send,
    {
        let key = self.key_for(recipe_id, image_path);
        let cached = {
            let known = self.known.lock().await;
            known.get(&key).cloned()
        };
        let metadata = if let Some(metadata) = cached {
            metadata
        } else if let Some(metadata) = self.store.head(&key).await? {
            self.known
                .lock()
                .await
                .insert(key.clone(), metadata.clone());
            metadata
        } else {
            let _permit = self
                .generation_slots
                .acquire()
                .await
                .context("Thumbnail generator is unavailable")?;
            // Another request might have completed while this request waited.
            if let Some(metadata) = self.store.head(&key).await? {
                self.known
                    .lock()
                    .await
                    .insert(key.clone(), metadata.clone());
                metadata
            } else {
                let original = fetch_original().await?;
                let max_edge = self.max_edge;
                let generated =
                    tokio::task::spawn_blocking(move || make_thumbnail(&original, max_edge))
                        .await
                        .context("Thumbnail worker stopped")??;
                let metadata = StoredThumbnail {
                    width: generated.width,
                    height: generated.height,
                };
                self.store.put(&key, generated.bytes, &metadata).await?;
                self.known
                    .lock()
                    .await
                    .insert(key.clone(), metadata.clone());
                metadata
            }
        };

        let url = self.store.presign_get(&key, self.url_ttl).await?;
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_add(self.url_ttl)
            .as_secs();
        Ok(ThumbnailRef {
            key,
            url,
            expires_at,
            width: metadata.width,
            height: metadata.height,
        })
    }

    pub async fn bytes(&self, thumbnail: &ThumbnailRef) -> Result<Vec<u8>> {
        self.store.get(&thumbnail.key).await
    }

    pub async fn invalidate(&self, recipe_id: i32, image_path: &str) -> Result<()> {
        let key = self.key_for(recipe_id, image_path);
        self.known.lock().await.remove(&key);
        self.store.delete(&key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryThumbnailStore;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::{
        io::Cursor,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn config() -> ThumbnailConfig {
        ThumbnailConfig {
            bucket: "test".to_owned(),
            region: "us-east-1".to_owned(),
            endpoint: None,
            access_key_id: "test".to_owned(),
            secret_access_key: "test".to_owned(),
            force_path_style: false,
            prefix: "thumbnails/".to_owned(),
            public_base_url: None,
            url_ttl: Duration::from_secs(3600),
            max_edge: 512,
        }
    }

    fn source_image() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(800, 400, Rgb([1, 2, 3])))
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[tokio::test]
    async fn stable_key_ignores_query_and_cached_thumbnail_skips_fetch() {
        let store = Arc::new(MemoryThumbnailStore::default());
        let service = ThumbnailService::new(store.clone(), &config(), "https://test".to_owned());
        assert_eq!(
            service.key_for(7, "https://example.test/photo.png?token=one"),
            service.key_for(7, "https://example.test/photo.png?token=two")
        );

        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = calls.clone();
            service
                .thumbnail_for(7, "/photo.png", move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(source_image())
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.object_count().await, 1);

        service.invalidate(7, "/photo.png").await.unwrap();
        assert_eq!(store.object_count().await, 0);
    }
}
