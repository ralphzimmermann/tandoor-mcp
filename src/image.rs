//! Safe retrieval of externally hosted recipe images.
//!
//! Image URLs are untrusted MCP input. Requests are therefore HTTPS-only,
//! pinned to prevalidated public addresses, manually redirected, and streamed
//! through a hard size limit.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
    Url,
};

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;

/// Downloads one public HTTPS image and returns bytes plus its verified suffix.
pub async fn download_public_image(url: &str) -> Result<(Vec<u8>, &'static str)> {
    let mut current = Url::parse(url).context("Invalid image URL")?;

    for redirect_count in 0..=MAX_REDIRECTS {
        let (client, validated) = client_for_public_url(&current).await?;
        let mut response = client
            .get(validated.clone())
            .header("User-Agent", "tandoor-mcp")
            .send()
            .await
            .context("Failed to download image")?;

        if response.status().is_redirection() {
            ensure!(
                redirect_count < MAX_REDIRECTS,
                "Image URL redirected too many times"
            );
            let location = response
                .headers()
                .get(LOCATION)
                .context("Image redirect omitted Location")?
                .to_str()
                .context("Image redirect Location is not valid text")?;
            current = validated
                .join(location)
                .context("Image redirect URL is invalid")?;
            continue;
        }

        ensure!(
            response.status().is_success(),
            "Image download failed: {}",
            response.status()
        );
        if let Some(length) = response.headers().get(CONTENT_LENGTH) {
            let length: usize = length
                .to_str()
                .context("Invalid image Content-Length")?
                .parse()
                .context("Invalid image Content-Length")?;
            ensure!(length <= MAX_IMAGE_BYTES, "Image is larger than 20 MB");
        }

        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        // S3 objects are frequently uploaded without explicit image metadata,
        // resulting in application/octet-stream (or no Content-Type). The
        // signature check below remains authoritative, so accepting generic
        // object metadata does not allow non-image content through.
        ensure!(
            is_acceptable_image_media_type(&media_type),
            "Unsupported image content type: {media_type}"
        );

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed while reading image")?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_IMAGE_BYTES,
                "Image is larger than 20 MB"
            );
            bytes.extend_from_slice(&chunk);
        }

        let extension = verified_extension(&bytes)?;
        return Ok((bytes, extension));
    }

    unreachable!("redirect loop returns or errors")
}

async fn client_for_public_url(url: &Url) -> Result<(reqwest::Client, Url)> {
    ensure!(
        url.scheme() == "https",
        "Only HTTPS image URLs are supported"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Image URLs must not contain credentials"
    );
    let host = url.host_str().context("Image URL has no host")?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .context("Could not resolve image host")?
        .collect();
    ensure!(!addresses.is_empty(), "Image host resolved to no addresses");
    ensure!(
        addresses.iter().all(|address| is_public_ip(address.ip())),
        "Image host resolves to a private or reserved address"
    );

    // Pin this request to an address that was checked above. Redirects build a
    // fresh pinned client after validating the next host.
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(20))
        .resolve(host, addresses[0])
        .build()
        .context("Could not create image HTTP client")?;
    Ok((client, url.clone()))
}

fn verified_extension(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("jpg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok("webp");
    }
    bail!("Downloaded data does not have a supported image signature")
}

fn is_acceptable_image_media_type(media_type: &str) -> bool {
    media_type.is_empty()
        || media_type.starts_with("image/")
        || matches!(
            media_type,
            "application/octet-stream" | "binary/octet-stream"
        )
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn private_and_reserved_addresses_are_rejected() {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            "fd00::1".parse().unwrap(),
            "2001:db8::1".parse().unwrap(),
        ] {
            assert!(!is_public_ip(ip), "{ip} must be rejected");
        }
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn signatures_are_verified() {
        assert_eq!(verified_extension(b"\xff\xd8\xffrest").unwrap(), "jpg");
        assert_eq!(verified_extension(b"\x89PNG\r\n\x1a\nrest").unwrap(), "png");
        assert!(verified_extension(b"<html>").is_err());
    }

    #[test]
    fn generic_s3_content_types_still_require_an_image_signature() {
        assert!(is_acceptable_image_media_type("application/octet-stream"));
        assert!(is_acceptable_image_media_type("binary/octet-stream"));
        assert!(is_acceptable_image_media_type(""));
        assert!(is_acceptable_image_media_type("image/jpg"));
        assert!(!is_acceptable_image_media_type("text/html"));
        assert!(verified_extension(b"<html>").is_err());
    }
}
