//! Update channel and build identity policy, independent of HTTP and UI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Nightly,
}

/// Native installer versions remain the workspace base version. The update feed
/// orders immutable builds separately, so a new build needs no stable version bump.
/// At the same base, nightly builds follow stable; returning to stable therefore
/// never presents an older same-base binary as an upgrade.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ReleaseIdentity {
    base: semver::Version,
    nightly: Option<(u64, u64)>,
}

impl ReleaseIdentity {
    pub(super) fn parse(tag: &str) -> Result<Self, String> {
        let invalid = || "The release build identity is invalid".to_string();
        let version = tag.strip_prefix('v').ok_or_else(invalid)?;
        let (base, nightly) = match version.split_once("-nightly.") {
            Some((base, build)) => {
                let (run, attempt) = build.split_once('.').ok_or_else(invalid)?;
                let number = |value: &str| -> Result<u64, String> {
                    let parsed = value.parse::<u64>().map_err(|_| invalid())?;
                    if parsed == 0 || parsed.to_string() != value {
                        return Err(invalid());
                    }
                    Ok(parsed)
                };
                (base, Some((number(run)?, number(attempt)?)))
            }
            None => (version, None),
        };
        let base = semver::Version::parse(base).map_err(|_| invalid())?;
        if !base.pre.is_empty() || !base.build.is_empty() {
            return Err(invalid());
        }
        Ok(Self { base, nightly })
    }

    pub(super) fn published(
        tag: &str,
        prerelease: bool,
        draft: bool,
        channel: UpdateChannel,
    ) -> Result<Self, String> {
        let identity = Self::parse(tag)?;
        if draft
            || prerelease != identity.nightly.is_some()
            || (channel == UpdateChannel::Stable && prerelease)
        {
            return Err("The release does not belong to the selected update channel".into());
        }
        Ok(identity)
    }

    pub(super) fn is_nightly(&self) -> bool {
        self.nightly.is_some()
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
    fn nightly_order_is_numeric_and_does_not_require_a_version_bump() {
        let stable = ReleaseIdentity::parse("v0.4.3").unwrap();
        let first = ReleaseIdentity::parse("v0.4.3-nightly.9.1").unwrap();
        let second = ReleaseIdentity::parse("v0.4.3-nightly.10.1").unwrap();
        let retry = ReleaseIdentity::parse("v0.4.3-nightly.10.2").unwrap();
        let next = ReleaseIdentity::parse("v0.4.4").unwrap();
        assert!(first > stable);
        assert!(second > first);
        assert!(retry > second);
        assert!(next > retry);
        assert!(
            stable < retry,
            "returning to stable must not silently downgrade"
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
