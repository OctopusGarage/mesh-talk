---
description: Cut a Mesh-Talk release — bump version, tag, and let CI build/sign/publish the 3-platform bundles
argument-hint: "[patch|minor|major|X.Y.Z]"
allowed-tools: Bash, Read, Edit
---

Releases are **tag-driven**: pushing a `vX.Y.Z` tag triggers
`.github/workflows/release.yml`, which builds **macOS (arm64 + x64), Windows, and Linux**
bundles, writes `SHA256SUMS`, signs each zip with **cosign**, attaches a **SLSA
build-provenance** attestation and a **CycloneDX SBOM**, and publishes them to a GitHub
Release whose body is auto-filled from `.github/release-notes.md` (the `__TAG__` placeholder is
substituted at release time — it documents verification + install + usage). For a local dev run
use `/dev`; `release.yml` also has a `workflow_dispatch` that builds all platforms as workflow
artifacts **without** publishing (a safe dry run).

Argument: `$ARGUMENTS` — a bump kind (`patch`/`minor`/`major`) or an explicit `X.Y.Z`.
Default `patch`.

## Steps

1. **Pre-flight** — refuse to release otherwise:
   - on `main`, working tree clean, in sync with `origin/main`;
   - latest `main` CI is green (`gh run list --branch main`).
2. **Compute the new version** from the current `version` under `[workspace.package]` in the
   **root `Cargo.toml`** and the requested bump.
3. **Bump in both places so they stay in sync** (the app version is the workspace version):
   - root `Cargo.toml` → `[workspace.package]` `version = "X.Y.Z"`
   - `src-tauri/tauri.conf.json` → `"version": "X.Y.Z"`
   Refresh the lockfile (`nice -n 10 cargo check` or `cargo update -p mesh-talk --precise X.Y.Z`).
4. **Commit + push**: `chore(release): vX.Y.Z` to `main`.
5. **(Recommended) dry run first** — validate packaging on all 4 targets without publishing:
   ```bash
   gh workflow run release.yml --ref main
   ```
   Wait for all 4 jobs green before tagging.
6. **Tag + push** (annotated; a lightweight `git tag -f` errors asking for a message here):
   ```bash
   git tag -a vX.Y.Z -m "Mesh-Talk vX.Y.Z — <one-line summary>"
   git push origin vX.Y.Z
   ```
7. **Watch** the `Tauri Release` run; confirm all 4 platform jobs succeed and the GitHub
   Release published with: 4 `*.zip` + 4 `*.zip.bundle` (cosign) + `mesh-talk.cdx.json` (SBOM).
8. **Verify a published asset** (proves the chain works, as a user would):
   ```bash
   gh release download vX.Y.Z -p 'mesh-talk_vX.Y.Z_macos_arm64.zip' -p '*.bundle'
   gh attestation verify mesh-talk_vX.Y.Z_macos_arm64.zip --repo Kingson4Wu/mesh-talk
   cosign verify-blob --bundle <zip>.bundle \
     --certificate-identity-regexp 'https://github.com/Kingson4Wu/mesh-talk/.*' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com <zip>
   ```

## Notes

- **Branch protection blocks force-push** to `main`, and the release flow never needs one.
- The released zips are **unsigned by a commercial CA** (free distribution) — first-run bypass is
  documented in the release body + README.
- To change what the release page says, edit `.github/release-notes.md` (not the published body).

## Report

State old → new version, the files bumped, the tag pushed, links to the release run + the
published Release, and the asset/verification results. If pre-flight failed, stop and say why —
never tag over a dirty tree or red CI.
