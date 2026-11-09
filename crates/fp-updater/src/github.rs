//! Finding the newest release for a channel in GitHub's release list
//! (`GET /repos/{owner}/{repo}/releases`).
//!
//! Both channels read this list: `releases/latest` skips pre-releases, can
//! lag behind a release just published (or point at one still uploading
//! its manifest), and follows GitHub's pre-release flag rather than the
//! version. Nothing read here is trusted: the manifest it leads to is still
//! signature-checked before use.

use semver::Version;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::manifest::Channel;

/// The asset every FramePlayerX release carries its manifest in.
pub const MANIFEST_ASSET: &str = "manifest.json";

/// The parts of a GitHub release the updater reads.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The `manifest.json` download URL of the highest-versioned release in
/// `releases_json` for `channel`: any release for beta, only versions
/// without a pre-release part (`1.2.0`, not `1.2.0-beta.1`) for stable,
/// whatever GitHub's pre-release flag says. Drafts, tags that aren't
/// versions (`v` prefix allowed) and releases without a manifest (yet) are
/// skipped. When none is left, [`Error::NoRelease`]: the list was read fine
/// but has nothing for the channel.
pub fn newest_manifest_url(releases_json: &[u8], channel: Channel) -> Result<String> {
    let releases: Vec<Release> = serde_json::from_slice(releases_json)
        .map_err(|e| Error::InvalidManifest(format!("GitHub release list: {e}")))?;
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version =
                Version::parse(r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name)).ok()?;
            if channel == Channel::Stable && !version.pre.is_empty() {
                return None;
            }
            let url = r
                .assets
                .into_iter()
                .find(|a| a.name == MANIFEST_ASSET)?
                .browser_download_url;
            Some((version, url))
        })
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
        .map(|(_, url)| url)
        .ok_or(Error::NoRelease(channel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, manifest: bool) -> serde_json::Value {
        let mut assets = vec![serde_json::json!({
            "name": "frameplayerx.zip",
            "browser_download_url": format!("https://example.com/{tag}/frameplayerx.zip"),
        })];
        if manifest {
            assets.push(serde_json::json!({
                "name": MANIFEST_ASSET,
                "browser_download_url": format!("https://example.com/{tag}/manifest.json"),
            }));
        }
        serde_json::json!({ "tag_name": tag, "draft": draft, "prerelease": tag.contains('-'), "assets": assets })
    }

    fn newest(releases: &[serde_json::Value]) -> Result<String> {
        newest_manifest_url(&serde_json::to_vec(releases).unwrap(), Channel::Beta)
    }

    fn newest_stable(releases: &[serde_json::Value]) -> Result<String> {
        newest_manifest_url(&serde_json::to_vec(releases).unwrap(), Channel::Stable)
    }

    #[test]
    fn stable_goes_by_the_version_not_the_flag() {
        // An alpha published without GitHub's pre-release flag (as older
        // CI did) after the stable release, the next beta, and a stable
        // release still uploading its manifest.
        let mut alpha = release("v0.5.1-alpha.1", false, true);
        alpha["prerelease"] = false.into();
        let list = [
            alpha,
            release("v0.6.0", false, false),
            release("v0.6.0-beta.2", false, true),
            release("v0.5.0", false, true),
            release("v0.4.0", false, true),
        ];
        assert_eq!(
            newest_stable(&list).unwrap(),
            "https://example.com/v0.5.0/manifest.json"
        );
        assert_eq!(
            newest(&list).unwrap(),
            "https://example.com/v0.6.0-beta.2/manifest.json"
        );
        assert!(matches!(
            newest_stable(&[release("v1.0.0-beta.1", false, true)]),
            Err(Error::NoRelease(Channel::Stable))
        ));
    }

    #[test]
    fn picks_the_highest_version_not_the_newest_entry() {
        // GitHub lists by creation date; beta.10 must beat beta.9 and the
        // stable release must beat its betas.
        let list = [
            release("v0.5.0-beta.9", false, true),
            release("v0.5.0-beta.10", false, true),
            release("v0.4.2", false, true),
        ];
        assert_eq!(
            newest(&list).unwrap(),
            "https://example.com/v0.5.0-beta.10/manifest.json"
        );
        let list = [
            release("v0.5.0-beta.10", false, true),
            release("v0.5.0", false, true),
        ];
        assert_eq!(
            newest(&list).unwrap(),
            "https://example.com/v0.5.0/manifest.json"
        );
    }

    #[test]
    fn skips_drafts_odd_tags_and_releases_without_manifest() {
        let list = [
            release("v0.4.0", false, true),
            release("v9.0.0", true, true),
            release("v0.6.0", false, false),
            release("nightly", false, true),
            release("0.5.0-beta.1", false, true),
        ];
        assert_eq!(
            newest(&list).unwrap(),
            "https://example.com/0.5.0-beta.1/manifest.json"
        );
        assert!(matches!(
            newest(&[release("v0.6.0", false, false)]),
            Err(Error::NoRelease(Channel::Beta))
        ));
        assert!(matches!(newest(&[]), Err(Error::NoRelease(Channel::Beta))));
        assert!(matches!(
            newest_manifest_url(b"{\"message\":\"rate limited\"}", Channel::Stable),
            Err(Error::InvalidManifest(_))
        ));
    }
}
