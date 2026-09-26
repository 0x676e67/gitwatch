use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use anyhow::{Context, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;

const REPOSITORY: &str = "https://api.github.com/repos/0x676e67/gitwatch/releases";
const DOWNLOADS: &str = "https://github.com/0x676e67/gitwatch/releases/download/";
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;

/// A published version and the official assets needed to install it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    version: String,
    #[serde(skip)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize)]
struct Response {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

impl Release {
    /// Checks the latest stable release, or an explicitly selected stable version.
    pub fn check(version: Option<&str>) -> Result<Self> {
        Self::query(version, Duration::from_secs(15))
    }

    pub(super) fn query(version: Option<&str>, timeout: Duration) -> Result<Self> {
        let endpoint = match version {
            Some(version) => format!("tags/v{}", Version::parse(version.trim_start_matches('v'))?),
            None => "latest".into(),
        };
        let response = agent(timeout)
            .get(format!("{REPOSITORY}/{endpoint}"))
            .header("Accept", "application/vnd.github+json")
            .call()
            .context("Cannot check GitHub releases")?;
        let bytes = limited(response.into_body().into_reader(), 2 * 1024 * 1024)?;
        Self::parse(&bytes, version)
    }

    fn parse(bytes: &[u8], requested: Option<&str>) -> Result<Self> {
        let response: Response = serde_json::from_slice(bytes)?;
        let version = Version::parse(
            response
                .tag_name
                .strip_prefix('v')
                .context("Invalid release tag")?,
        )?;
        ensure!(
            !response.draft && !response.prerelease && version.pre.is_empty(),
            "Only stable releases are supported"
        );
        if let Some(requested) = requested {
            ensure!(
                version == Version::parse(requested.trim_start_matches('v'))?,
                "Release version does not match the request"
            );
        }
        Ok(Self {
            version: version.to_string(),
            assets: response.assets,
        })
    }

    /// The release's semantic version, without the tag prefix.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The official page with release notes and manual downloads.
    pub fn url(&self) -> String {
        format!(
            "https://github.com/0x676e67/gitwatch/releases/tag/v{}",
            self.version
        )
    }

    /// Whether this release is newer than the running build.
    pub fn is_newer(&self) -> bool {
        Version::parse(&self.version)
            .ok()
            .zip(Version::parse(super::VERSION).ok())
            .is_some_and(|(release, current)| release > current)
    }

    pub(super) fn download(&self, directory: &Path) -> Result<std::path::PathBuf> {
        let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
        let name = format!(
            "gitwatch-v{}-{}.{}",
            self.version,
            super::target()?,
            extension
        );
        let archive = self.asset(&name)?;
        ensure!(
            archive.size > 0 && archive.size <= MAX_ARCHIVE,
            "Release archive is too large"
        );
        let sums = self.asset("SHA256SUMS")?;
        let client = agent(Duration::from_secs(180));
        let response = client
            .get(&sums.browser_download_url)
            .call()
            .context("Cannot download checksums")?;
        let bytes = limited(response.into_body().into_reader(), 64 * 1024)?;
        let expected = checksum(std::str::from_utf8(&bytes)?, &name)?;
        let path = directory.join(&name);
        let response = client
            .get(&archive.browser_download_url)
            .call()
            .context("Cannot download update")?;
        let mut reader = response.into_body().into_reader().take(MAX_ARCHIVE + 1);
        let mut file = File::create(&path)?;
        let mut digest = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            ensure!(size <= MAX_ARCHIVE, "Release archive is too large");
            digest.update(&buffer[..count]);
            file.write_all(&buffer[..count])?;
        }
        file.sync_all()?;
        ensure!(
            size == archive.size && format!("{:x}", digest.finalize()) == expected,
            "Update checksum does not match"
        );
        Ok(path)
    }

    fn asset(&self, name: &str) -> Result<&Asset> {
        let mut matching = self.assets.iter().filter(|asset| asset.name == name);
        let asset = matching
            .next()
            .context("The release is missing a required asset")?;
        ensure!(matching.next().is_none(), "Duplicate release asset");
        ensure!(
            asset.browser_download_url == format!("{DOWNLOADS}v{}/{name}", self.version),
            "Unexpected release download URL"
        );
        Ok(asset)
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .user_agent(concat!("gitwatch/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "Release response is too large");
    Ok(bytes)
}

fn checksum(sums: &str, name: &str) -> Result<String> {
    let mut found = None;
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(hash), Some(file), None) = (parts.next(), parts.next(), parts.next())
            && file.trim_start_matches('*') == name
        {
            ensure!(
                found.is_none() && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid release checksum"
            );
            found = Some(hash.to_ascii_lowercase());
        }
    }
    found.context("The archive is missing from SHA256SUMS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_and_checksums_are_bound_to_the_requested_asset() {
        let json = br#"{"tag_name":"v9.0.0","draft":false,"prerelease":false,"assets":[]}"#;
        assert!(Release::parse(json, Some("9.0.0")).unwrap().is_newer());
        assert!(Release::parse(json, Some("8.0.0")).is_err());
        assert!(
            Release::parse(
                br#"{"tag_name":"v9.0.0-beta.1","draft":false,"prerelease":true,"assets":[]}"#,
                None
            )
            .is_err()
        );
        let hash = "ab".repeat(32);
        assert_eq!(
            checksum(&format!("{hash}  good.zip\n"), "good.zip").unwrap(),
            hash
        );
        assert!(checksum(&format!("{hash}  other.zip\n"), "good.zip").is_err());
        assert!(checksum(&format!("{hash}  good.zip\n{hash}  good.zip"), "good.zip").is_err());
        assert!(checksum("abc  good.zip", "good.zip").is_err());
    }
}
