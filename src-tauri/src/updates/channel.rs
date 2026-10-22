//! Update channel and build identity policy, independent of HTTP and UI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Nightly,
}

/// Canonical logical build identity. Ordering delegates directly to SemVer:
/// previous stable < next-target dev < next-target Nightly < final stable.
/// Numeric native installer versions are a separate projection of the target.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ReleaseIdentity(semver::Version);

impl ReleaseIdentity {
    pub(super) fn parse(tag: &str) -> Result<Self, String> {
        let invalid = || "The release build identity is invalid".to_string();
        let value = tag.strip_prefix('v').ok_or_else(invalid)?;
        let version = semver::Version::parse(value).map_err(|_| invalid())?;
        if version.to_string() != value || !version.build.is_empty() {
            return Err(invalid());
        }
        let pre = version.pre.as_str();
        if !pre.is_empty() && pre != "dev" {
            let build = pre.strip_prefix("nightly.").ok_or_else(invalid)?;
            let parts: Vec<_> = build.split('.').collect();
            if parts.len() != 2
                || !parts.iter().all(|value| {
                    value
                        .parse::<u64>()
                        .is_ok_and(|number| number > 0 && number.to_string() == *value)
                })
            {
                return Err(invalid());
            }
        }
        Ok(Self(version))
    }

    pub(super) fn published(
        tag: &str,
        prerelease: bool,
        draft: bool,
        channel: UpdateChannel,
    ) -> Result<Self, String> {
        let identity = Self::parse(tag)?;
        if draft
            || identity.0.pre.as_str() == "dev"
            || prerelease != identity.is_nightly()
            || (channel == UpdateChannel::Stable && prerelease)
        {
            return Err("The release does not belong to the selected update channel".into());
        }
        Ok(identity)
    }

    pub(super) fn is_nightly(&self) -> bool {
        self.0.pre.as_str().starts_with("nightly.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_never_accepts_nightly_or_mislabelled_releases() {
        assert!(
            ReleaseIdentity::published("v0.4.3-nightly.123.1", true, false, UpdateChannel::Stable)
                .is_err()
        );
        assert!(
            ReleaseIdentity::published(
                "v0.4.3-nightly.123.1",
                false,
                false,
                UpdateChannel::Nightly
            )
            .is_err()
        );
        assert!(ReleaseIdentity::published("v0.4.3", true, false, UpdateChannel::Nightly).is_err());
        assert!(ReleaseIdentity::published("v0.4.3", false, true, UpdateChannel::Stable).is_err());
    }

    #[test]
    fn semver_orders_development_nightly_and_final_release() {
        let previous = ReleaseIdentity::parse("v0.4.3").unwrap();
        let first = ReleaseIdentity::parse("v0.4.4-nightly.9.1").unwrap();
        let second = ReleaseIdentity::parse("v0.4.4-nightly.10.1").unwrap();
        let retry = ReleaseIdentity::parse("v0.4.4-nightly.10.2").unwrap();
        let stable = ReleaseIdentity::parse("v0.4.4").unwrap();
        assert!(first > previous);
        assert!(second > first);
        assert!(retry > second);
        assert!(
            stable > retry,
            "the final release must naturally replace its Nightly"
        );
        let development = ReleaseIdentity::parse("v0.4.4-dev").unwrap();
        assert!(development > previous && development < first);
        assert!(
            ReleaseIdentity::published("v0.4.4-dev", true, false, UpdateChannel::Nightly).is_err()
        );
    }

    #[test]
    fn malformed_builds_cannot_become_download_paths_or_candidates() {
        for tag in [
            "0.4.3",
            "v0.4.3-beta.1",
            "v0.4.3+sha",
            "v0.4.3-nightly.0.1",
            "v0.4.3-nightly.01.1",
            "v0.4.3-nightly.1.0",
            "v0.4.3-nightly.1.1/evil",
            "v0.4.3-nightly.18446744073709551616.1",
        ] {
            assert!(ReleaseIdentity::parse(tag).is_err(), "{tag}");
        }
    }

    #[test]
    fn channel_defaults_and_wire_values_are_explicit() {
        assert_eq!(UpdateChannel::default(), UpdateChannel::Stable);
        assert_eq!(
            serde_json::from_str::<UpdateChannel>("\"nightly\"").unwrap(),
            UpdateChannel::Nightly
        );
        assert!(serde_json::from_str::<UpdateChannel>("\"other\"").is_err());
    }
}
