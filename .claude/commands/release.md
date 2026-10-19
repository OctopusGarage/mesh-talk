---
description: Prepare an authorized version release using the maintained draft-verification workflow
argument-hint: "[patch|minor|major|X.Y.Z]"
allowed-tools: Bash, Read, Edit
---

The source of truth is `.github/workflows/release.yml` and its
`scripts/release/` helpers. Read them before release work; do not duplicate the
platform/edition matrix, artifact counts, signing identities or publication
steps in this command.

## Preparation

1. Confirm the user authorized the release scope. Inspect branch state, clean
   tree, source checks and the requested version; release sources must be merged
   into main before tag-based verification/publication.
2. Keep workspace/desktop versions consistent (`Cargo.toml`,
   `src-tauri/tauri.conf.json`) and review affected lockfile/frontend metadata.
3. Use a new version tag. Published releases are immutable to the maintained
   preflight; never overwrite a published tag/release or bypass its state guard.
4. A branch dispatch builds verification artifacts without publishing. A version
   tag run builds a draft and verifies signed archives, provenance, editions and
   relay artifacts. Read `scripts/release/preflight.mjs` for exact ref rules.
5. Publication is a separate explicitly authorized version-tag workflow dispatch
   with `publish_release=true`, after the current verification/state checks pass.
   A tag push alone does not publish the verified draft.

The repository is `OctopusGarage/mesh-talk`. The workflow derives signing and
attestation identities from its repository/ref; do not reuse another owner's
verification commands. `.github/release-notes.md` owns release-body content;
`docs/updates.md` and `docs/post-office-release.md` own user verification/install.

Report the source commit/tag, exact workflow result, draft/verified/published
state and artifact evidence. Do not publish or send announcements without the
user's authorization.
