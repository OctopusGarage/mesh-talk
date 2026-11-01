# Updating Mesh-Talk

Open **More → About** and choose an **Update channel**, then **Check for updates**.
Checks happen only when requested. Eligible installations offer **Update and restart**:
Mesh-Talk downloads and verifies the signed update, installs it and restarts the application.
Other installations offer **Download update** with platform-specific manual instructions.
Changing channels never starts a download or installation. Updates preserve the local data directory;
storage compatibility remains a separate requirement.

## Choose a channel

| Channel | Intended use | Publication |
|---|---|---|
| **Stable** (default) | Everyday use | A maintainer publishes a verified, tagged official release. |
| **Nightly** | Trying recent features and fixes; builds may contain bugs | CI builds trusted `main` daily when it changes, or on an explicit manual request, then publishes a verified GitHub prerelease. |

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
The SDK enables TLS and system proxy support explicitly, with optional ZIP extraction disabled:
Windows receives the raw NSIS executable, Linux the raw AppImage, and macOS its `.app.tar.gz`
payload through the SDK's unconditional macOS tar support. Release verification enforces these formats.

**Update and restart** downloads into a private temporary folder, validates the exact release,
channel, OS and architecture, checks size/SHA-256 and verifies the updater signature against the
public key embedded in the running application. The signed version must equal the full logical
release identity, including Nightly run and attempt. A renderer cannot provide URLs, paths, keys,
signatures or replacement bytes. Downloads are cancellable; a canceled download never starts an
installation. Signature errors leave the running application untouched.

Before installation the UI flushes conversation drafts. An unfinished call, file transfer or draft
save failure or an edit made during persistence keeps the verified update ready for **Restart to update** after the work finishes.
Installation and restart disable competing update actions. The native installer handles application
replacement and any system authorization. Immediate installer-launch or authorization failures
keep the prepared update retryable and offer **Download manually**. On Windows, once the NSIS
installer has successfully started, the SDK exits the old application. If installation is then
canceled or fails, reopen Mesh-Talk and check again, or use the manual installer; the exited
application cannot display an in-app retry. Window state is saved before this handoff, while
native resources stay live until Windows accepts the installer launch. Prepared updates are session-local and temporary: restarting before
installation requires downloading again. Restart requests use Tauri's lifecycle handling and macOS
LaunchServices rather than launching a child that inherits the login agent's process group.

No paid Apple Developer ID or Windows publisher certificate is required for updater signatures.
These signatures authenticate update bytes; they do not grant administrator rights or remove
Gatekeeper/SmartScreen warnings for unsigned distribution. The system may request a password or
another permitted authorization method. Touch ID is not promised and the application does not add
an artificial biometric prompt. Restart resumes a session only according to the existing **Stay
signed in** and app-lock settings; updating never silently enables persistent login. Native
replacement recovery does not imply rollback of an incompatible storage migration.

### Incremental download preparation

On macOS and Windows, patch-capable clients first look for an optional signed delta catalog
in the selected release. Linux continues to download its full AppImage. Older clients keep
using the unchanged compressed native updater names. The first patch-capable client arrives
through the ordinary full update; subsequent updates can use its installed executable immediately.

The catalog is authenticated with the existing updater public key and full logical target
version before its declarations are parsed. It binds the release kind, OS/architecture,
canonical compressed fallback, optional native target and at most two older executable bases.
Release tooling reads at most 100 releases in 16 MiB of metadata and examines at most eight
matching older candidates to find two authenticated bases. Prior payload downloads are bounded
to 256 MiB and signatures to 16 KiB. Only a cryptographically valid legacy signature lacking
a signed logical version is skipped as ineligible; other authentication failures stop the build.
The host snapshots its own executable once; the renderer cannot select a base or a URL.
A zstd prefix patch reconstructs exact published native installer bytes in private staging.
The patch and reconstructed target must match their signed sizes and SHA-256 values, and the
target must additionally pass the unchanged SDK signed-version check before installation.

The optional macOS target stores the original tar bytes in a gzip level-0 stream. The optional
Windows target uses an uncompressed NSIS installer containing the same application files.
Clients never recreate archives, recompress installers or modify the running executable.
Release tooling verifies representation equivalence and round-trips every emitted patch
through the production decoder. Patch plus required catalog/signatures must save at least
25% and 1 MiB against the ordinary **compressed** full download; otherwise no patch is offered.
Windows bundle snapshots stay beside the build output so restoration uses the same filesystem,
even when the runner workspace and system temporary directory are on different drives.
Updater preparation failures report the underlying exception and captured child diagnostics,
with configured signing secrets redacted; authentication and equivalence failures remain fatal.

Local limits are 64 KiB for the catalog, 128 MiB for the executable base and zstd window,
256 MiB for the patch and reconstructed target, and 60 seconds for reconstruction. One normal
zstd frame is accepted; trailing frames/data are rejected. Preparation checks private staging
space and runs reconstruction off the GUI thread with cancellation checks between chunks.
Missing optional artifacts, an unsupported codec, an unrecognized/oversized base or target,
insufficient savings, or an ordinary optional-metadata/patch network failure or reconstruction failure select the original
independently signed compressed full update once. Temporary delta files are discarded first.
Cancellation stops preparation, and catalog authentication, identity contradictions or final
native signature failures stop the update. They never install failed bytes or silently downgrade.

The UI distinguishes preparing the update from downloading a full fallback. Installation,
authorization, draft/session guards, retries and restart remain the existing native flow.
A successful full fallback is not evidence that an incremental update ran: native acceptance
must record the actual patch route and transferred bytes, then verify the exact installed
application, changed process and ready login state. Local fixtures and AI evaluation do not
replace isolated native or trusted published-release acceptance. CI collects workspace coverage
in each operating system's existing test pass; Linux's full-only branch cannot represent the
macOS/Windows delta preparation paths.

### Free updater signing setup

Desktop and post-office release ZIPs use `scripts/release/sign-blob.mjs` for keyless Cosign
signing. Transient Rekor conflicts, HTTP rate limits/server errors and recognized transport
timeouts retry in fresh signing processes at most three times, waiting 15 then 45 seconds.
Each attempt has a three-minute process timeout and a separate temporary bundle; only a
successful process with a nonempty bundle promotes it to the final path. Permanent errors
and exhausted retries fail the build. A transparency-log conflict alone never counts as success.
The aggregate verifier still checks each final bundle's certificate identity, issuer and
transparency evidence before publication; retries do not bypass that gate.

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
The same signing key authenticates canonical full packages and optional incremental preparation; both reach the existing native installer.

## Download verification and installation

Linux release builds use Ubuntu 22.04 for both x64 and ARM64 to retain that glibc
baseline. Keep these build runners versioned: moving to a newer distribution can
produce an AppImage whose bundled libraries cannot load on older systems, even
when packaging and signature verification succeed. Compiled Cargo caches include
the runner identity so native objects and build scripts cannot cross baselines.
Public-package acceptance must launch the actual AppImage on a supported older
runtime before the complete upgrade chain is considered verified. See Tauri's
[AppImage compatibility guidance](https://v2.tauri.app/distribute/appimage/).

The app saves the compatible ZIP to Downloads. The ZIP contains platform installers and
`SHA256SUMS` (and updater payloads when enabled). The app streams it to a temporary file, checks the published size and SHA-256
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

## Native release inspection

[`scripts/release/sevenzip.py`](../scripts/release/sevenzip.py) owns the supported
7-Zip version and official asset digests for native payload inspection. Native
macOS/Windows producers and both Ubuntu acceptance jobs install that tool into a
job-local directory. Downloads have size and time limits; checksum verification
precedes extraction, and the installed version must match before its absolute
path is exported as `MESH_TALK_SEVENZIP`. Windows uses its existing 7-Zip only to
unpack the verified tool archive. It never installs the tool globally.

The Python metadata and delta verifiers share this selection. An explicitly
configured missing tool fails rather than falling back to another version.
Local callers may use an available 7-Zip, but an NSIS decoder reporting `BadCmd`
fails before payload acceptance. Unknown application/resource sizes remain an
error; decoded payload bytes, plugin equivalence, signatures and provenance
remain mandatory. Update the version and official digests together, then verify
the native consumers before accepting a tool upgrade.

The existing Linux portable health gate installs NSIS and runs
`python scripts/release/test_native_nsis.py -v`. It compiles real Unicode
installers with a plugin call, compressed and raw payloads, and an uninstaller;
it proves complete payload equivalence and rejects altered application/resources
without executing any installer. Bootstrap regressions also cover checksum,
version, platform and explicit-path failures. These checks catch decoder drift
before publication builds. All three portable health platforms also execute the
bootstrap before merge, while actual signed six-platform artifacts and
downloaded draft verification remain required release evidence.

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
  Nightly builds. Missing or pending required checks are polled once per minute for up to four
  hours on the selected source SHA, so CI finishing after the scheduled start can proceed
  automatically. Completed unsuccessful checks, open scanning alerts and API errors fail
  immediately; a timeout reports the checks still pending. Retry after resolving those failures.
  Final publication rechecks immediately and never accepts pending checks. Required checks and
  bounded waiting are owned by `scripts/release/check-source-checks.mjs`.
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
publication retains its source-check, artifact-verification and container gates.
Current-source acceptance includes ASan and mutation-runner contracts. Stable
rechecks that same policy immediately before public container upload and final
release publication, after all intervening verification/build preparation; a newly
failed or pending source check prevents the corresponding public mutation. Nightly tags
must never be made the repository's latest Stable release.

Local validation selects the updater, frontend update store/browser flow and release-script
regressions through their module `validation.json` contracts. Native one-click installation, authorization cancellation, restart from normal/login-agent launches,
window/session/draft restoration and same-base Nightly replacement (macOS, Windows NSIS and Linux
AppImage), manual MSI/DEB/RPM replacements, exact-source hosted signing/attestation and
first real Nightly publication require their native/hosted evidence; mocked browser tests do not
establish those properties. No claim of cross-version data compatibility follows from a green build.

### Native acceptance fixtures

The standalone `native-update-eval`, `native-update-windows-eval` and
`native-update-linux-eval` examples exercise the installed Tauri SDK on their
respective platforms. They use alternate application identifiers, host-owned
signed fixture files and disposable profiles guarded by sentinel files. Build
with `cargo build -p mesh-talk --example NAME --locked`; numeric fixture versions
derive from Cargo, while synthetic Nightly suffixes identify the two snapshots.
Never point these fixtures at the production installation or account directory.

Record the exact committed source, package digests, old/new process identities,
rejected signature/version cases, replacement, relaunch and preserved data marker.
An SDK fixture does not exercise production GitHub discovery, real account
session/draft restoration or interactive system authorization; those need separate
native and hosted acceptance. The Windows launch-failure unit regression also
checks that application resources and renderer IPC remain usable after the SDK
cannot launch an installer.

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

### Manual release preparation

Ordinary pushes to `main` do not create release proposals, infer versions from commit
prefixes or generate changelog entries. Prepare a Stable release only when its scope is
ready. Maintainers own the release decision and product-facing notes:

1. In a normal development branch, use `node scripts/release/version.mjs prepare VERSION`
   if the intended version differs from Cargo's current target. Do not change version
   projections by hand. Add the corresponding `## VERSION` section to root `CHANGELOG.md`,
   describing user-visible changes, compatibility or migration requirements and known
   limitations. Existing linked version headings remain readable. Keep historical sections.
2. Review and merge that PR through the normal checks. A Stable draft requires a nonempty,
   unique changelog section for its exact numeric Cargo version. A different version's
   section never substitutes for missing notes. Changelog contents are maintained prose,
   not a generated commit list or a second authority for the product version.
3. On `main`, run **Prepare Stable Draft** with `tag=vVERSION`, for example `v0.4.5`.
   The workflow checks out trusted `main`, validates canonical version projections,
   curated notes, exact-source hosted CI/security and existing tag/release state before
   changing remote state. The target must exceed the highest published numeric Stable.
   It creates an immutable exact-source tag and an **unpublished draft**, then explicitly
   dispatches **Tauri Release** on that tag with both publication flags disabled.
4. Wait for the complete signed, source-attested and verified asset set. Then select the
   version tag in **Tauri Release → Run workflow** and enable **publish_release** to use
   the existing exact-source, downloaded-draft, signature, provenance and container gates.
   Preparing a draft does not publish Stable. Do not publish an empty draft or move a tag.

The manual preparation command can resume a same-source unpublished draft or tag. It
refuses a tag bound to another commit, a published release, a prerelease or a conflicting
version. It never pushes commits to `main`, changes a tag target or merges a PR.

`CHANGELOG.md` is the sole product-note source. `.github/release-notes.md` provides only the
shared product, download, installation and verification template; it contains no fixed
release highlights. Draft preparation and build reruns render the exact tag's curated
section together with that template. Edit notes in the reviewed source before preparing
its tag; edits made only in the GitHub draft body are not the maintained input and may be
replaced by a build rerun. GitHub's additional automatic release-note generation is disabled.
Nightlies include a manually maintained section only when their current numeric target has
one; otherwise they omit release highlights, retaining the experimental/source identity
and download instructions. They never borrow a previous Stable's product notes.

The legacy `v0.4.3` tag remains immutable. Its source commit (`9b9f91b`) has exactly the same
Git tree as the corresponding reachable `main` squash commit (`335a001`). Manual draft
preparation still verifies that fixed historical source/tree equivalence and ancestry.
Removing the commit-scanning proposal generator removes its config/manifest, not this
provenance check. Never advance the fixed historical boundary during version preparation.

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

If manual draft preparation is interrupted, rerun **Prepare Stable Draft** on `main`
with the same tag only while main still identifies that exact source. It verifies an
existing tag/draft rather than replacing it. If main has advanced, inspect the existing
unpublished tag's exact SHA and manually run **Tauri Release** on that tag with both
publication flags disabled to resume its build. An existing draft does not imply a
successful or repeated build dispatch. Correct missing curated notes before tagging.
If maintenance dispatch fails after Stable publication, the publisher reports a warning and
recovery instructions while preserving the successful publication result. Do not retry or undo
publication to repair a follow-up dispatch.
For a missing next-target PR after publication, run maintenance with the exact published
`stable_tag`. Inspect Actions logs, release/tag source SHA and the resulting PR; retry the
owning workflow rather than bypassing version or source checks.
An incomplete or failed draft stays unpublished. Hosted automation acceptance must establish
manual intent/exact-source checks, draft-only behavior, explicit build dispatch and next-target
PR creation; local mocked tests do not prove GitHub settings or real signing/publication.
Release-state API reads request cache revalidation: a stale empty listing must not
hide an existing draft or replace the fresh asset identity/digest checks.

The numeric target and channel build identity have separate responsibilities. The target
supports native package formats; `BASE-dev`, `BASE-nightly.RUN.ATTEMPT`, and tagged `BASE`
identify source, experimental and Stable builds. Native same-target Nightly replacement
remains a manual operation; adding a Git SHA or MSI fourth component is not an automatic
update mechanism. Product version equality does not establish wire or stored-data compatibility.

## Why this model

The established tagged build, verified draft and explicit publication stages remain the
release engine. Manual preparation supplies reviewed intent and orchestrates that engine;
it does not duplicate the platform matrix or publisher. The earlier releases established
that separation, while the current preparation action removes hand-run tag/API coordination.

[Release Please](https://github.com/googleapis/release-please) derives proposals from
Conventional Commits. Mesh-Talk uses one versioned product and can squash several features
into one integration commit, so that input does not reliably describe user-facing scope.
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) instead advocates curated notable
changes, grouped by their effects on users. Maintainers review those notes in the same
source change as the intended version; CI validates and renders them deterministically.
No additional package-version planner or automatic commit-list body is required.
[GitHub's release guidance](https://docs.github.com/en/repositories/releasing-projects-on-github/automatically-generated-release-notes)
supports reviewing draft content and assembling assets before publication. The existing
artifact/source checks and immutable publication path continue to own that transition.


[VS Code Insiders](https://code.visualstudio.com/insiders/) demonstrates an explicit daily
experimental channel; [Zed's release policy](https://zed.dev/faq) separates early access from
stability, and [RustDesk's Nightly workflow](https://github.com/rustdesk/rustdesk/blob/master/.github/workflows/flutter-nightly.yml)
demonstrates scheduled and manual builds. Mesh-Talk uses two channels initially because it does
not yet have a separate Preview stabilization process.

GitHub Releases [are based on Git tags](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases):
the explicit preparation action creates the chosen tag without changing Git's source identity. GitHub's
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
existing single-source mechanisms. Mesh-Talk uses reviewed version/changelog changes and an
explicit draft-preparation action, keeping artifact verification and final publication in its
existing release workflow.
[GitHub's workflow-trigger rules](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)
explain the explicit dispatch and bot-PR check requirements.
