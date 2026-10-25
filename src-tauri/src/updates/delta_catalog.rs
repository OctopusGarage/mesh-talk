//! Signed catalog policy. Callers must authenticate bytes before parsing.

use serde::Deserialize;

use super::{ReleaseAsset, ReleaseIdentity, native::payload_name};

pub(super) const MAX_CATALOG_SIZE: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

impl Artifact {
    fn valid(&self) -> bool {
        self.size > 0
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    pub fn matches(&self, asset: &ReleaseAsset) -> bool {
        self.name == asset.name
            && self.size == asset.size
            && asset.digest.as_deref() == Some(format!("sha256:{}", self.sha256).as_str())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Base {
    pub tag: String,
    pub size: u64,
    pub sha256: String,
    pub patch: Artifact,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    schema: u32,
    role: String,
    pub algorithm: String,
    tag: String,
    release_kind: String,
    os: String,
    arch: String,
    pub fallback: Artifact,
    pub target: Artifact,
    pub bases: Vec<Base>,
}

pub(super) fn catalog_name(tag: &str, os: &str, arch: &str) -> Option<String> {
    Some(format!("{}.delta.json", payload_name(tag, os, arch)?))
}

pub(super) fn worthwhile(full: u64, patch: u64, overhead: u64) -> bool {
    let total = u128::from(patch) + u128::from(overhead);
    total + 1024 * 1024 <= u128::from(full) && total * 4 <= u128::from(full) * 3
}

impl Catalog {
    /// Never call this with unauthenticated catalog bytes.
    pub fn parse(bytes: &[u8], tag: &str, os: &str, arch: &str) -> Result<Self, String> {
        let invalid = || "The signed delta catalog contradicts this release".to_owned();
        if bytes.len() as u64 > MAX_CATALOG_SIZE || !matches!(os, "macos" | "windows") {
            return Err(invalid());
        }
        let catalog: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let identity = ReleaseIdentity::parse(tag)?;
        let full = payload_name(tag, os, arch).ok_or_else(invalid)?;
        let raw = full.replacen("mesh-talk-update_", "mesh-talk-update-raw_", 1);
        if catalog.schema != 1
            || catalog.role != "mesh-talk-delta"
            || catalog.tag != tag
            || catalog.os != os
            || catalog.arch != arch
            || catalog.release_kind
                != if identity.is_nightly() {
                    "nightly"
                } else {
                    "stable"
                }
            || catalog.fallback.name != full
            || catalog.target.name != raw
            || !catalog.fallback.valid()
            || !catalog.target.valid()
            || catalog.bases.is_empty()
            || catalog.bases.len() > 2
        {
            return Err(invalid());
        }
        let platform = super::package_name(tag, os, arch).ok_or_else(invalid)?;
        let stem = platform
            .strip_prefix("mesh-talk_")
            .and_then(|s| s.strip_suffix(".zip"))
            .ok_or_else(invalid)?;
        let mut seen = std::collections::HashSet::new();
        for base in &catalog.bases {
            let base_identity = ReleaseIdentity::parse(&base.tag)?;
            let base_artifact = Artifact {
                name: String::new(),
                size: base.size,
                sha256: base.sha256.clone(),
            };
            if base_identity >= identity
                || base_identity.is_nightly() != identity.is_nightly()
                || base.tag.ends_with("-dev")
                || !seen.insert(&base.tag)
                || !base_artifact.valid()
                || !base.patch.valid()
                || base.patch.name != format!("mesh-talk-patch_{stem}_from-{}.zst", base.tag)
            {
                return Err(invalid());
            }
        }
        Ok(catalog)
    }

    pub fn matching_base(&self, tag: &str, size: u64, sha256: &str) -> Option<&Base> {
        self.bases
            .iter()
            .find(|base| base.tag == tag && base.size == size && base.sha256 == sha256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema":1,"role":"mesh-talk-delta","algorithm":"zstd-prefix-v1",
            "tag":"v0.4.5-nightly.42.2","release_kind":"nightly","os":"macos","arch":"aarch64",
            "fallback":{"name":"mesh-talk-update_v0.4.5-nightly.42.2_macos_arm64.app.tar.gz","size":10000000,"sha256":"a".repeat(64)},
            "target":{"name":"mesh-talk-update-raw_v0.4.5-nightly.42.2_macos_arm64.app.tar.gz","size":20000000,"sha256":"b".repeat(64)},
            "bases":[{"tag":"v0.4.5-nightly.42.1","size":100,"sha256":"c".repeat(64),"patch":{"name":"mesh-talk-patch_v0.4.5-nightly.42.2_macos_arm64_from-v0.4.5-nightly.42.1.zst","size":3000000,"sha256":"d".repeat(64)}}]
        })).unwrap()
    }
    #[test]
    fn accepts_exact_identity_and_matching_base() {
        let catalog =
            Catalog::parse(&fixture(), "v0.4.5-nightly.42.2", "macos", "aarch64").unwrap();
        assert!(
            catalog
                .matching_base("v0.4.5-nightly.42.1", 100, &"c".repeat(64))
                .is_some()
        );
        assert!(
            catalog
                .matching_base("v0.4.5-nightly.42.1", 100, &"d".repeat(64))
                .is_none()
        );
        assert!(
            catalog
                .matching_base("v0.4.5-nightly.42.1", 101, &"c".repeat(64))
                .is_none()
        );
    }
    #[test]
    fn rejects_authenticated_identity_contradictions_and_unsafe_declarations() {
        for (field, value) in [
            ("tag", json!("v0.4.5-nightly.42.3")),
            ("os", json!("windows")),
            ("arch", json!("x86_64")),
            ("role", json!("other")),
            ("release_kind", json!("stable")),
            ("schema", json!(2)),
        ] {
            let mut value_json: serde_json::Value = serde_json::from_slice(&fixture()).unwrap();
            value_json[field] = value;
            assert!(
                Catalog::parse(
                    &serde_json::to_vec(&value_json).unwrap(),
                    "v0.4.5-nightly.42.2",
                    "macos",
                    "aarch64"
                )
                .is_err(),
                "{field}"
            );
        }
        assert!(
            Catalog::parse(
                &vec![b' '; 65537],
                "v0.4.5-nightly.42.2",
                "macos",
                "aarch64"
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_paths_relabelled_bases_and_duplicate_bases() {
        for mutate in [0, 1, 2, 3] {
            let mut v: serde_json::Value = serde_json::from_slice(&fixture()).unwrap();
            match mutate {
                0 => v["target"]["name"] = json!("../payload"),
                1 => v["bases"][0]["tag"] = json!("v0.4.5-nightly.42.3"),
                2 => {
                    let b = v["bases"][0].clone();
                    v["bases"].as_array_mut().unwrap().push(b);
                }
                _ => v["bases"][0]["patch"]["sha256"] = json!("bad"),
            }
            assert!(
                Catalog::parse(
                    &serde_json::to_vec(&v).unwrap(),
                    "v0.4.5-nightly.42.2",
                    "macos",
                    "aarch64"
                )
                .is_err()
            );
        }
    }
    #[test]
    fn savings_are_against_compressed_fallback_including_metadata() {
        assert!(worthwhile(10_000_000, 7_000_000, 1000));
        assert!(!worthwhile(10_000_000, 7_500_000, 1));
        assert!(!worthwhile(2_000_000, 1_000_000, 1));
        assert!(!worthwhile(u64::MAX, u64::MAX, u64::MAX));
    }
}
