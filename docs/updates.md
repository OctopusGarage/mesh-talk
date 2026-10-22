# Updating Mesh-Talk

Open **More → About** and choose an **Update channel**, then **Check for updates**.
Checks happen only when requested. Downloads and installation remain separate manual actions;
the app does not install software, restart itself, or reset your local account store.

## Choose a channel

| Channel | Intended use | Publication |
|---|---|---|
| **Stable** (default) | Everyday use | A maintainer publishes a verified, tagged official release. |
| **Nightly** | Trying recent features and fixes; builds may contain bugs | CI builds trusted `main` daily when it changes, or on an explicit manual request, then publishes a verified GitHub prerelease. |

The choice is saved on this device, independently of accounts. Selecting a channel does not
start a network request or installation. Nightly also accepts a newer Stable release when
appropriate. Other prerelease labels, drafts and arbitrary branch builds are not update candidates.
Nightly is a development channel, not a claim that a feature is ready for general use.

Both channels preserve the installed **default/lite edition**, OS and CPU architecture. A missing
matching package is reported rather than substituting an edition or an older build. Review the
release notes and package name before choosing **Download update**.

Each Nightly has a unique build identity, for example `0.4.3-nightly.123456789.1`. About shows
that identity after checking for updates. The final two numbers identify the workflow run and
attempt; a new build of the same base version can therefore be recognized without publishing
another official stable version. Native installer metadata retains the workspace base version
(`0.4.3` in this example). The update policy compares base versions, then the numeric Nightly
run and attempt. For a given base version, Nightly builds follow the stable build; a higher
base version follows all builds of the previous base. This is a deliberate build-order policy,
not generic SemVer prerelease precedence.

Changing back to Stable changes future checks. It does not automatically downgrade an installed
Nightly. If Stable has not caught up, use **Download stable release** to review its notes and
install manually. Version ordering is not proof of stored-data backwards compatibility: keep an
encrypted backup and recovery key before trying Nightly or returning to an older application.
There is one installation and one existing `~/.mesh-talk` data directory; the channels do not
create independent profiles or support concurrent Stable and Nightly instances.

## Download verification and installation

The app saves the compatible ZIP to Downloads. The ZIP contains platform installers and
`SHA256SUMS`. The app streams it to a temporary file, checks the published size and SHA-256
digest from GitHub's Releases API, and only then shows **Show in Downloads**. Canceling or
failing a download removes the temporary file. An existing ZIP is reused only if its size and
digest still match. Download requests re-fetch the exact selected release and enforce its channel,
build identity and device package; a newly published Nightly does not invalidate an older checked
immutable build. The renderer cannot provide an arbitrary URL or destination path.

After downloading, unzip, quit Mesh-Talk, and install it yourself:

| Platform | Install |
|---|---|
| macOS | Open the DMG and drag Mesh-Talk to Applications, replacing the existing app. |
| Windows | Run the EXE installer, or use MSI for a managed installation. Nightly rebuilds can share the native base version; these are manual replacements, not MSI-version-based automatic updates. |
| Linux | Install the DEB or RPM for your distribution, or use the AppImage. A distribution package manager may require an explicit reinstall for a same-base Nightly. |

The ZIP's adjacent `.bundle` signature and build attestation support independent provenance
verification; follow the release's **Verifying your download** instructions. The in-app SHA-256
check protects the transfer using metadata returned by GitHub, but does not replace signature
verification. Current installers do not have commercial OS code-signing certificates.

GitHub failures are retryable; no download is advertised as verified before validation completes.
Stable uses GitHub's latest full-release endpoint. Nightly also reads a bounded recent release
catalog (up to three pages of 10 entries, stopping at a page containing recognized Nightlies).
An exhausted discovery bound is reported explicitly. Public download links are available from the
[project download page](https://octopusgarage.github.io/mesh-talk/#download), without Actions
artifact access or a GitHub login.

## Publish Nightly builds

The authoritative implementation is [the release workflow](../.github/workflows/release.yml),
with policy in `scripts/release/preflight.mjs`, `release-state.mjs`, and the existing artifact
verifiers. Stable and Nightly reuse the same platform/edition matrix and verification owners.

- The daily schedule runs at **03:23 UTC** on `main`. It skips a source SHA already represented
  by a published Nightly. A manual **Tauri Release → Run workflow** on `main` with
  **publish_nightly** enabled requests a Nightly immediately. Leave **publish_release** disabled.
- Ordinary branch dispatches without either publication flag still produce private workflow
  artifacts only. Nightly publication rejects tag refs, other branches and simultaneous Stable
  publication requests. Only trusted main-branch code may use publication credentials.
- Preflight requires the existing exact-source CI/health/E2E/security checks before expensive
  Nightly builds. A failed or pending required check blocks publication; wait for those checks
  and retry. Required checks are owned by `scripts/release/check-source-checks.mjs`.
- CI generates `v<workspace-version>-nightly.<run-id>.<attempt>` and binds that technical tag to
  the exact source SHA. There is no manual version bump or official Stable tag. Never reuse or
  move a published Nightly tag, and never replace its uploaded assets.
- All default/lite desktop packages and post-office archives are built, signed and source-attested.
  The workflow verifies the downloaded workflow assets, creates a prerelease draft, uploads the
  complete set, re-downloads and verifies the draft, records asset IDs/digests, and publishes
  only that verified snapshot with `prerelease=true` and `make_latest=false`.
- Nightly publication is serialized. Failed matrix jobs leave no public build; failures after draft
  creation leave an unpublished draft for inspection. Retry the workflow after fixing the source
  or environment; a rerun has a new attempt identity. Do not bypass verification to publish a
  partial draft. Inspect source SHA, run/attempt and recorded digests in Actions evidence.
- Nightly produces downloadable post-office archives; GHCR container publication remains part of
  the existing explicit Stable publication path. Production relay operators should pin Stable tags.

The workflow uses the existing scoped `GITHUB_TOKEN` and GitHub OIDC signing/attestation paths;
no user-supplied download token is embedded in the app. Repository permissions and branch/tag
rules must allow the workflow to create its generated Nightly refs and drafts. Enabling GitHub
immutable releases is recommended; the workflow uploads all assets before publication so it
works with that protection. Published Nightlies are retained as immutable, traceable builds;
there is no automatic deletion policy in this workflow. Review storage use before adding one.

## Stable promotion and acceptance

Test a chosen main-branch source, assign the intended official base version across the existing
version owners, and use the existing `vX.Y.Z` draft build and explicit **publish_release** process.
A Nightly release is not promoted by changing its prerelease flag or renaming its assets. Stable
publication retains its source-check, artifact-verification and container gates. Nightly tags
must never be made the repository's latest Stable release.

Local validation selects the updater, frontend update store/browser flow and release-script
regressions through their module `validation.json` contracts. Native same-version replacement
(macOS, Windows EXE/MSI and Linux package formats), exact-source hosted signing/attestation and
first real Nightly publication require their native/hosted evidence; mocked browser tests do not
establish those properties. No claim of cross-version data compatibility follows from a green build.

## Why this model

[VS Code Insiders](https://code.visualstudio.com/insiders/) demonstrates an explicit daily
experimental channel; [Zed's release policy](https://zed.dev/faq) separates early access from
stability, and [RustDesk's Nightly workflow](https://github.com/rustdesk/rustdesk/blob/master/.github/workflows/flutter-nightly.yml)
demonstrates scheduled and manual builds. Mesh-Talk uses two channels initially because it does
not yet have a separate Preview stabilization process.

GitHub Releases [are based on Git tags](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases):
this mechanism removes manual official tagging, not Git's source identity. GitHub's
[release API](https://docs.github.com/en/rest/releases/releases) separates prereleases from latest
full releases. Unique per-build releases fit GitHub's
[immutable release model](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
and avoid downloads changing behind a permanent Nightly asset URL.

Native package versions and build identity are separate because
[Windows MSI compares only three product-version fields](https://learn.microsoft.com/en-us/windows/win32/msi/productversion),
while [SemVer ignores build metadata for precedence](https://semver.org/). Merely appending a Git
SHA or incrementing a fourth MSI component would not reliably identify the next update.
