# Updating Mesh-Talk

Open **More → About** and choose an **Update channel**, then **Check for updates**.
Checks happen only when requested. Eligible installations offer **Update and restart**:
Mesh-Talk downloads and verifies the signed update, installs it and restarts the application.
Other installations offer **Download update** with platform-specific manual instructions.
Changing channels never starts a download or installation. Updates preserve the local data directory;
storage compatibility remains a separate requirement.

## Choose a channel

| Channel              | Intended use                                              | Publication                                                                                                                    |
| -------------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| **Stable** (default) | Everyday use                                              | A maintainer publishes a verified, tagged official release.                                                                    |
| **Nightly**          | Trying recent features and fixes; builds may contain bugs | CI builds trusted `main` daily when it changes, or on an explicit manual request, then publishes a verified GitHub prerelease. |

The choice is saved on this device, independently of accounts. Selecting a channel does not
start a network request or installation. Nightly also accepts a newer Stable release when
appropriate. Other prerelease labels, drafts and arbitrary branch builds are not update candidates.
Nightly is a development channel, not a claim that a feature is ready for general use.

Both channels select the desktop package for the installed OS and CPU architecture. A missing
matching package is reported rather than substituting an older build. Desktop releases use one
package per target, without optional bundled packs or an edition suffix. Installed packs survive
upgrades. Older Lite clients still look for suffixed assets: install the new unsuffixed package
manually to migrate. Review the release notes and package name before choosing **Download update**.

Each Nightly has a unique build identity, for example `0.4.4-nightly.123456789.1`. About and
diagnostics show the logical build identity. The final two numbers identify the workflow run
and attempt; a new build of the same target can therefore be recognized without publishing
another official Stable version. Source builds use `0.4.4-dev`; an official tagged build uses
`0.4.4`. Native installer metadata retains the numeric workspace target (`0.4.4` in this
example). Update candidates follow standard SemVer ordering:

```text
0.4.3 < 0.4.4-dev < 0.4.4-nightly.123456789.1 < 0.4.4
```

Nightly publication requires a development target greater than the latest published Stable.
A published version is immutable: development after `0.4.3` must target a later version,
rather than changing the meaning of `0.4.3` or making its prereleases sort after it.

Changing back to Stable changes future checks. It does not automatically downgrade an installed
Nightly. If Stable has not caught up, use **Download stable release** to review its notes and
install manually. Version ordering is not proof of stored-data backwards compatibility: keep an
encrypted backup and recovery key before trying Nightly or returning to an older application.
There is one installation and one existing `~/.mesh-talk` data directory; the channels do not
create independent profiles or support concurrent Stable and Nightly instances.

## One-click installation

Automatic installation requires a packaged release with an embedded updater public key and a
matching, version-bound signed payload in the selected release. macOS supports installed `.app`
bundles outside mounted `/Volumes` images and App Translocation; Windows supports NSIS installations; Linux supports
AppImage installations. Source/debug builds, MSI, DEB/RPM and releases without updater artifacts
retain the manual path. Install the first updater-enabled release manually before using this feature.

**Update and restart** downloads into a private temporary folder, validates the exact release,
channel, OS and architecture, checks size/SHA-256 and verifies the updater signature against the
public key embedded in the running application. The signed version must equal the full logical
release identity, including Nightly run and attempt. A renderer cannot provide URLs, paths, keys,
signatures or replacement bytes. Downloads are cancellable; a canceled download never starts an
installation. Signature errors leave the running application untouched.

Before installation the UI flushes conversation drafts. An unfinished call, file transfer or draft
save failure keeps the verified update ready for **Restart to update** after the work finishes.
Installation and restart disable competing update actions. The native installer handles application
replacement and any system authorization; canceled authorization or installation errors offer retry
and **Download manually**. Prepared updates are session-local and temporary: restarting before
installation requires downloading again. Restart requests use Tauri's lifecycle handling and macOS
LaunchServices rather than launching a child that inherits the login agent's process group.

No paid Apple Developer ID or Windows publisher certificate is required for updater signatures.
These signatures authenticate update bytes; they do not grant administrator rights or remove
Gatekeeper/SmartScreen warnings for unsigned distribution. The system may request a password or
another permitted authorization method. Touch ID is not promised and the application does not add
an artificial biometric prompt. Restart resumes a session only according to the existing **Stay
signed in** and app-lock settings; updating never silently enables persistent login. Native
replacement recovery does not imply rollback of an incompatible storage migration.

### Free updater signing setup

Generate a dedicated updater key pair once, outside the repository, using the installed Tauri CLI:

```bash
node frontend/node_modules/@tauri-apps/cli/tauri.js signer generate --write-keys /secure/path/mesh-talk-updater.key
```

Store the private key securely with a backup. Configure GitHub Actions repository variable
`MESH_TALK_UPDATER_PUBLIC_KEY` with the complete generated `.pub` file contents, and secret
`TAURI_SIGNING_PRIVATE_KEY` with the matching private key contents. If encrypted, also configure
secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Neither key requires a paid service. Never commit
the private key, put it in frontend resources, print it to logs or generate a fresh release key per
build. Changing keys requires a separate compatibility/migration design for installed clients.

The release workflow exposes this pair only to its existing trusted main Nightly and verified
Stable tag build paths. `scripts/release/updater.mjs prepare` creates an ignored build configuration;
`collect` copies native updater payloads and signs their complete logical version. Both keys absent
keeps manual-only publication; configuring only one fails the build. Local updater builds must
provide the same public-key environment value to compile the trust root into the host.

Each platform publishes `mesh-talk-update_<tag>_<platform>.app.tar.gz`, `.nsis.exe` or `.AppImage`
and its `.sig`. The same bytes are included in the platform ZIP and its checksum manifest. The
existing cosign/provenance gate verifies that ZIP; the aggregate verifier requires a complete
updater platform matrix and byte-for-byte equality between embedded and standalone assets.
Existing draft/source checks, immutability and explicit Stable publication remain required.
This is a full-package update mechanism; no delta/patch delivery is implemented.

## Download verification and installation

The app saves the compatible ZIP to Downloads. The ZIP contains platform installers and
`SHA256SUMS` (and updater payloads when enabled). The app streams it to a temporary file, checks the published size and SHA-256
digest from GitHub's Releases API, and only then shows **Show in Downloads**. Canceling or
failing a download removes the temporary file. An existing ZIP is reused only if its size and
digest still match. Download requests re-fetch the exact selected release and enforce its channel,
build identity and device package; a newly published Nightly does not invalidate an older checked
immutable build. The renderer cannot provide an arbitrary URL or destination path.

After downloading, unzip, quit Mesh-Talk, and install it yourself:

| Platform | Install                                                                                                                                                                                   |
| -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS    | Open the DMG and drag Mesh-Talk to Applications, replacing the existing app.                                                                                                              |
| Windows  | Run the EXE installer, or use MSI for a managed installation. Nightly rebuilds can share the native base version; these are manual replacements, not MSI-version-based automatic updates. |
| Linux    | Install the DEB or RPM for your distribution, or use the AppImage. A distribution package manager may require an explicit reinstall for a same-base Nightly.                              |

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

## Public website downloads

The project site requests GitHub's latest Stable release metadata and presents
its desktop ZIP packages directly. It accepts only published full releases,
exact unsuffixed desktop archive names and matching GitHub asset URLs. Relays,
legacy Lite packages, signature bundles and prereleases are not desktop download
choices. There is no separately maintained website version number or binary URL.

The visitor's operating system selects the initial platform tab. Browser client
hints can also identify a supported 64-bit CPU. When hints are unavailable,
denied or ambiguous, the page offers explicit architecture choices; in
particular, Safari's `MacIntel` platform string does not distinguish Intel from
Apple silicon. Explicit platform choices update the primary download button; architecture choices
are remembered for the browser session when session storage is available. Visitors
can always change platform and architecture. Mobile
visitors choose a desktop package explicitly. A missing package is reported,
never silently replaced with another architecture or an older release.

A download click uses GitHub's ordinary asset link and shows platform-specific
unzip, install and first-run guidance. The Mac guide includes the release-owned
app-specific quarantine command with copy feedback; it does not run commands or
request system privileges. Opening the guide separately does not start a download. The guide offers the
exact selected asset as **Download again**, and keeps focus on a visible control
after the architecture chooser closes.
The page cannot observe completion of the browser's download and does not claim
the archive has been checked. Follow the release notes for signature and
provenance verification. This differs from the desktop updater's verified
transfer described above.

Release lookup has a bounded timeout and a retry action. If GitHub is unavailable
or rate-limited, or JavaScript is disabled, ordinary release-page links remain
available. Downloading and installing remain explicit visitor actions. Browser
coverage is owned by `frontend/e2e/site-downloads.spec.ts`; these fixtures prove
page selection and interaction, not native installation or release provenance.

## Publish Nightly builds

The authoritative implementation is [the release workflow](../.github/workflows/release.yml),
with policy in `scripts/release/preflight.mjs`, `release-state.mjs`, and the existing artifact
verifiers. Stable and Nightly reuse the same platform matrix and verification owners.

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
- All desktop packages and post-office archives are built, signed and source-attested.
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

Prepare and review the intended next version using the [version lifecycle](#version-ownership-and-lifecycle),
test the chosen main-branch source, and use the existing `vX.Y.Z` draft build and explicit
**publish_release** process.
A Nightly release is not promoted by changing its prerelease flag or renaming its assets. Stable
publication retains its source-check, artifact-verification and container gates. Nightly tags
must never be made the repository's latest Stable release.

Local validation selects the updater, frontend update store/browser flow and release-script
regressions through their module `validation.json` contracts. Native one-click installation, authorization cancellation, restart from normal/login-agent launches,
window/session/draft restoration and same-base Nightly replacement (macOS, Windows NSIS and Linux
AppImage), manual MSI/DEB/RPM replacements, exact-source hosted signing/attestation and
first real Nightly publication require their native/hosted evidence; mocked browser tests do not
establish those properties. No claim of cross-version data compatibility follows from a green build.

## Version ownership and lifecycle

The root `Cargo.toml` field `[workspace.package].version` is the single authoritative numeric
product target. All four Rust packages inherit it with `version.workspace = true`; Tauri
omits its own version and resolves the inherited Cargo version. Root `package.json` is a
private tooling package whose version is a generated projection for release automation.
Its lockfile's root records and the four local-package records in `Cargo.lock` must agree
with Cargo. The private `frontend/package.json` and its lockfile have no product version.
The frontend obtains the running app's logical version from the host.

Use the version owner rather than editing those projections by hand:

```bash
node scripts/release/version.mjs check
node scripts/release/version.mjs sync
node scripts/release/version.mjs prepare 0.4.4
```

`check` reports contract drift without changing files; `sync` repairs derived metadata from
the canonical target. `prepare VERSION` applies an explicit numeric target and synchronizes
the same projections. The version command also supports `--root PATH` for isolated fixtures.
The example target is illustrative, not a release instruction: inspect the current target
before preparing a version. Review the diff and run the affected validation plan before
committing. Never use repository-wide search/replace: dependency versions, compatibility
fixtures, historical release references and protocol/schema versions have separate owners.

Version ownership is enforced by structural standards and CI, not only by this guide.
Changes to this lifecycle must update the version helper, its regression tests, release
workflow checks and owning `validation.json` contracts together. A new product-version
consumer must derive the existing target or declare a checked projection in that owner;
it must not introduce another manually maintained version constant.

### Automated release preparation

The **Release Maintenance** workflow (`release-maintenance.yml`) runs on pushes to `main`
and on manual dispatch. A pinned release-please action treats the repository as one product,
using its root private Node manifest to include Rust and frontend commits in the same release
proposal. Scoped updates change Cargo's canonical field; `scripts/release/maintenance.mjs`
uses the trusted main-branch version owner to synchronize the validated same-repository bot
PR. It never executes code from the bot's proposed branch. Release tracking records the last
released version; it is not another authority for the current development target.
An absent PR output is expected on idle runs and after a release is created. The
synchronization environment must tolerate that absence even when its step is skipped,
so it cannot prevent the subsequent exact tagged draft-build dispatch.

The fixed release-history migration floor handles the existing `v0.4.3` publication. Its
published source commit (`9b9f91b`) has exactly the same Git tree as the corresponding
`main` squash commit (`335a001`), but the tag's commit is not an ancestor of `main`.
Without a reachable boundary, release-please would count already released historical
commits again. The top-level `last-release-sha` therefore pins that equivalent main commit
as a history floor. Before proposing releases, maintenance verifies the immutable legacy
tag's exact source, tree equivalence and the floor's ancestry on main; a mismatch fails
closed. Later reachable release commits stop the scan before this floor automatically.
This is historical provenance, not a current version field: never advance it during
ordinary version preparation, retag `v0.4.3`, or omit its acceptance checks. Changes require
a reviewed history migration and regressions for both initial and subsequent releases.

The maintained release PR collects Conventional Commits and proposes a version and changelog.
Release-please owns the generated root `CHANGELOG.md`; review it in release PRs. Its history
records released changes and does not define the current product target. The release-publication
validation contract explicitly owns this generated file.
While the product is `0.x`, ordinary features and fixes propose patch releases and breaking
changes propose a minor release; entering `1.0` requires an explicit maintainer decision.
Review compatibility and the intended target rather than accepting a generated number blindly.

Merging a release PR permits automation to create an immutable `vX.Y.Z` tag and **unpublished
draft only**, then explicitly dispatches **Tauri Release** on that tag with both publication
flags disabled. The existing workflow builds and verifies the complete asset set into the draft.
Before dispatch, maintenance fills the draft from `.github/release-notes.md` with
download, installation and verification instructions bound to the exact tag/source.
The verified build refreshes those notes and adds the changelog; useful draft notes
must not depend on a successful build or an external attestation request.
It cannot publish Stable merely because a version PR merged. A maintainer must select the
version tag in **Tauri Release → Run workflow** and enable **publish_release** for the existing
exact-source, signatures, provenance, downloaded-draft and container gates to publish it.
Do not publish an empty draft, move a published tag or replace its assets.

After successful Stable publication, the release workflow explicitly dispatches maintenance
with `stable_tag=vX.Y.Z`; the published-release event is also supported. Maintenance verifies
the published Stable release and its source's relationship to `main`, then opens an
`automation/next-development` PR for at least the next patch target. The target never moves
backwards if main has already advanced. This PR requires the normal checks and review; it
does not push directly to `main` or merge itself. Merge the next-target PR before requesting
a new Nightly, because Nightly rejects a target equal to or below published Stable.

### Permissions and recovery

Maintenance uses scoped repository `GITHUB_TOKEN` permissions and pinned actions. In organization
**Settings → Actions → General → Workflow permissions**, allow GitHub Actions to create and
approve pull requests, then enable the corresponding repository setting. GitHub couples these
permissions in one setting; automation still uses reviewed PRs and never approves or merges them.
If the organization disallows it, the repository-level API returns HTTP 409 and bot proposals
remain blocked until an organization administrator enables the setting. Inspect readiness with
`gh api repos/OctopusGarage/mesh-talk/actions/permissions/workflow`; the
`can_approve_pull_request_reviews` field must be true for this built-in-token setup.

Repository
settings must allow bot PR/branch creation and the trusted release workflow's tag/draft and
Actions dispatch operations. It requires no embedded app token, external version service or
personal access token. Built-in-token push/tag events do not cascade into another workflow,
so release builds and post-publication maintenance use explicit dispatch. Bot PR checks may
require a write-access maintainer to approve workflow runs; inspect the PR's check state and
repository rules before merging. A deliberately configured GitHub App can support unattended
event chaining if the project later needs it.

For an interrupted release proposal, manually run **Release Maintenance** with `stable_tag`
empty to reconcile the proposal. If a draft/tag already exists but its build dispatch failed,
inspect its exact source SHA and manually run **Tauri Release** on that tag with both
publication flags disabled; an existing draft does not imply the dispatch will repeat.
If maintenance dispatch fails after Stable publication, the publisher reports a warning and
recovery instructions while preserving the successful publication result. Do not retry or undo
publication to repair a follow-up dispatch.
For a missing next-target PR after publication, run maintenance with the exact published
`stable_tag`. Inspect Actions logs, release/tag source SHA and the resulting PR; retry the
owning workflow rather than bypassing version or source checks.
An incomplete or failed draft stays unpublished. Hosted automation acceptance must establish
bot-PR permissions/check execution, draft-only behavior, explicit build dispatch and next-target
PR creation; local mocked tests do not prove GitHub settings or real signing/publication.
Release-state API reads request cache revalidation: a stale empty listing must not
hide an existing draft or replace the fresh asset identity/digest checks.

The numeric target and channel build identity have separate responsibilities. The target
supports native package formats; `BASE-dev`, `BASE-nightly.RUN.ATTEMPT`, and tagged `BASE`
identify source, experimental and Stable builds. Native same-target Nightly replacement
remains a manual operation; adding a Git SHA or MSI fourth component is not an automatic
update mechanism. Product version equality does not establish wire or stored-data compatibility.

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

[Cargo workspace inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-package-table)
and [Tauri's Cargo-version fallback](https://v2.tauri.app/reference/config/#version) provide the
existing single-source mechanisms. [release-please](https://github.com/googleapis/release-please)
provides reviewed release proposals and [scoped manifest updates](https://github.com/googleapis/release-please/blob/main/docs/customizing.md);
Mesh-Talk keeps artifact verification and final publication in its own release workflow.
[GitHub's workflow-trigger rules](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)
explain the explicit dispatch and bot-PR check requirements.
