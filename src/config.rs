//! Runtime configuration for the optional S3-backed thumbnail cache.

use std::{env, time::Duration};

use anyhow::{bail, Context, Result};
use url::Url;

/// Settings used to generate, store, and serve recipe thumbnails.
#[derive(Clone, Debug)]
pub struct ThumbnailConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub force_path_style: bool,
    pub prefix: String,
    pub public_base_url: Option<Url>,
    pub url_ttl: Duration,
    pub max_edge: u32,
}

impl ThumbnailConfig {
    /// Load thumbnail settings. An absent `S3_BUCKET` disables the feature.
    pub fn from_env() -> Result<Option<Self>> {
        let Some(bucket) = non_empty("S3_BUCKET") else {
            return Ok(None);
        };
        let region = non_empty("S3_REGION").unwrap_or_else(|| "us-east-1".to_owned());
        let access_key_id =
            non_empty("S3_ACCESS_KEY_ID").context("S3_ACCESS_KEY_ID is required")?;
        let secret_access_key =
            non_empty("S3_SECRET_ACCESS_KEY").context("S3_SECRET_ACCESS_KEY is required")?;
        let endpoint = non_empty("S3_ENDPOINT");
        let force_path_style = parse_bool("S3_FORCE_PATH_STYLE", false)?;
        let mut prefix =
            non_empty("S3_THUMBNAIL_PREFIX").unwrap_or_else(|| "thumbnails/".to_owned());
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        if prefix.starts_with('/') || prefix.contains("..") {
            bail!("S3_THUMBNAIL_PREFIX must be a relative object-key prefix");
        }
        let public_base_url = non_empty("S3_PUBLIC_BASE_URL")
            .map(|value| Url::parse(&value).context("S3_PUBLIC_BASE_URL must be a valid URL"))
            .transpose()?;
        let ttl_secs = parse_u64("THUMBNAIL_URL_TTL_SECS", 86_400)?;
        if !(60..=604_800).contains(&ttl_secs) {
            bail!("THUMBNAIL_URL_TTL_SECS must be between 60 and 604800");
        }
        let max_edge = parse_u32("THUMBNAIL_MAX_EDGE", 512)?;
        if !(128..=2048).contains(&max_edge) {
            bail!("THUMBNAIL_MAX_EDGE must be between 128 and 2048");
        }

        Ok(Some(Self {
            bucket,
            region,
            endpoint,
            access_key_id,
            secret_access_key,
            force_path_style,
            prefix,
            public_base_url,
            url_ttl: Duration::from_secs(ttl_secs),
            max_edge,
        }))
    }

    /// HTTPS origin that must be allowed by the MCP Apps widget CSP.
    pub fn resource_origin(&self) -> Result<String> {
        let url = if let Some(url) = &self.public_base_url {
            url.clone()
        } else if let Some(endpoint) = &self.endpoint {
            Url::parse(endpoint).context("S3_ENDPOINT must be a valid URL")?
        } else {
            Url::parse(&format!(
                "https://{}.s3.{}.amazonaws.com",
                self.bucket, self.region
            ))?
        };
        let host = url.host_str().context("S3 image origin has no host")?;
        let port = url
            .port()
            .map_or_else(String::new, |port| format!(":{port}"));
        Ok(format!("{}://{host}{port}", url.scheme()))
    }
}

fn non_empty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_bool(name: &str, default: bool) -> Result<bool> {
    match non_empty(name).as_deref() {
        None => Ok(default),
        Some("true" | "1" | "yes") => Ok(true),
        Some("false" | "0" | "no") => Ok(false),
        Some(_) => bail!("{name} must be true or false"),
    }
}

fn parse_u64(name: &str, default: u64) -> Result<u64> {
    non_empty(name)
        .map(|value| {
            value
                .parse()
                .with_context(|| format!("{name} must be an integer"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_u32(name: &str, default: u32) -> Result<u32> {
    non_empty(name)
        .map(|value| {
            value
                .parse()
                .with_context(|| format!("{name} must be an integer"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}
