# Quality gates and release evidence

The gate verifies outcomes, not just process exit codes. A successful compiler,
high line coverage, or a browser mock is not proof of a correct native release.
Keep implementation plans and transient verification logs outside the repository.

## Change review

Prefer independently reviewable PRs with one feature or mechanism. Record the exact
tested commit, executed commands, failing-baseline evidence, native runner links,
and limitations in the PR. Before requesting review run `make test` and
`CI=1 MT_TARGET_CLEAN=0 ./scripts/check-health.sh`. The full health gate builds
frontend assets before Tauri compilation, including on a fresh checkout.

For each applicable scenario, assert externally observable outcomes. A test should
fail with the old implementation or a deliberate incorrect mutation. Do not weaken
assertions to accommodate unexplained failures.

| Change | Required scenarios in addition to the happy path |
|--------|-------------------------------------------------|
| Persistent state | Reopen/restart, append after torn record, read/write failure, owner isolation, no memory-only success after failed persistence |
| Permissions/privacy | Allow and deny, revocation after existing session, restart recovery, unknown peer/device, group vs direct-message boundaries |
| Async UI/media | Account switch/logout while request is pending, late response rejection, failed save and retry, media cleanup |
| Multiple devices/files | Every destination, one logical history record, sender restart before recipient download, revoked access |
| Platform/native UI | Real Tauri IPC and window/process state on the affected OS, negative control, runner/desktop/GPU limitations |
| CI/release scripts | Real fixture files, missing/partial evidence, tool failures, changed source and stale artifacts |

Rust unit/integration, real multi-process backend E2E, browser UI tests, and native
desktop tests answer different questions. A VM/Xvfb/software-renderer pass does not
establish that a reported physical GPU or VMware compositor fault is fixed.

## Test quality and evidence

`node --test scripts/diagnostics/*.test.mjs` runs in three-platform CI and the health
gate. Python 3.11+ is required for archive regressions; CI pins Python 3.12. The
diagnostic suite includes actual release-script execution with deliberate verifier
failures, real ZIP corruption fixtures, and release-state changes after verification.
Full health requires cargo-deny, cargo-machete, typos, Gitleaks, ShellCheck and
actionlint; missing tools fail rather than being silently skipped. CI installs them
explicitly. `--fast` remains a clearly labeled partial local check.

Full macOS/Windows health checks also run on PRs, not only after merging to main.
Independent matrix results are retained even if another platform fails. Superseded
CI, health and mutation runs are cancelled automatically. Windows gates explicitly
use Bash: opening a `.sh` file through PowerShell file associations or passing a
literal test glob is not execution evidence. Require the native health check names
alongside `verify` in branch protection to make these PR checks merge-blocking;
the workflow alone does not change administrator branch-protection settings.

Mutation testing uses pinned cargo-mutants 27.1.0 and examines both Rust workspace
members. A hard unmutated workspace test runs first; the mutation tool also runs its
own baseline for nonempty selections. Selected counts, completed outcomes, baseline,
tool exit code and summary counts must agree. Zero selected mutations are recorded
explicitly and may have no report; a stale report is rejected.

Survivors remain advisory and must be triaged in review. Unviable mutants are not
caught mutants. Timeouts, usage/diff/internal errors, absent reports and incomplete
sweeps fail the job: they are inconclusive, not successful validation. The full sweep
still has a runtime budget; a timed-out sweep is not represented as complete.
Report formats and exit codes follow [cargo-mutants documentation](https://mutants.rs/mutants-out.html)
and [the pinned exit-code definitions](https://github.com/sourcefrog/cargo-mutants/blob/v27.1.0/src/exit_code.rs).

Coverage remains a signal, not a substitute for assertions or mutation review. Do
not count test-module lines as production-only coverage, unviable mutants as killed,
or skipped/inconclusive native scenarios as passed.

## Release workflow

The normal sequence is:

`preflight → four native builds → aggregate verification → draft upload → downloaded-draft verification → optional publication`

Preflight rejects version/tag mismatches, tags not already merged into main and
published releases. Workflow runs for the same ref are serialized. A branch dry run
uses `v<version>-ci.<run-id>` archive names and never writes a GitHub release.

Each platform signs its ZIP and records build provenance. Builds only upload workflow
artifacts; they cannot independently publish or update a release. The aggregate job
downloads all four outputs and requires exactly four ZIPs, four signature bundles
and one nonempty CycloneDX SBOM.

The gate verifies signatures against the exact repository, workflow and source ref,
and provenance against the exact source commit on GitHub-hosted runners. It inspects
ZIPs without extracting them: duplicate/unsafe members, installer symlinks, unexpected
formats, blank/partial/duplicate/wrong checksums and empty installers are rejected.
Legitimate relative symlinks inside the macOS app must stay within that app.

Metadata inspection never executes or installs application binaries:

- macOS: inspect actual DMG UDIF structure and embedded app with 7-Zip; check version,
  architecture and equality of its executable to the separately packaged `.app`.
- Windows: inspect PE version and bootstrapper architecture, MSI ProductVersion and
  x64 Template. An x86 NSIS bootstrapper installing an x64 app is valid.
- Linux: inspect Debian/RPM version and architecture, AppImage format and embedded ELF.
  Use `unsquashfs -cat` at the runtime's validated ELF-derived filesystem offset,
  without executing AppImage; 7-Zip codec support differs across operating systems.
  The offset calculation follows the [official Type 2 runtime](https://github.com/AppImage/type2-runtime/blob/main/src/runtime/runtime.c).
  AppImage has no application-version metadata; bind its `.text` and `.rodata` to the
  versioned Debian executable, normalizing only Tauri's fixed-width bundle-type markers.
  Any other code/constants difference fails.

This is artifact correctness verification, not proof that installation, OS trust
prompts, every desktop/GPU, or every application feature works. Native functional
tests remain necessary. The SBOM is checked for inventory and byte equality between
workflow and draft; it does not have a separate signature bundle.

Only after all workflow assets pass are they centrally uploaded to a draft. Draft
status is rechecked immediately before upload. The actual draft is re-downloaded and
re-verified; its bytes must equal workflow output and GitHub asset digests. Evidence
binds the repository, source SHA, release ID and each asset ID/size/digest.

Tag pushes keep releases as drafts. To explicitly publish a verified stable tag:

```sh
gh workflow run release.yml --ref vX.Y.Z -f publish_release=true
```

This rebuilds and verifies the draft, then requires current-source CI, both full health
jobs, UI/backend E2E, Gitleaks, Scorecard, all three CodeQL languages and no unresolved
main code-scanning alerts. Missing, running, skipped or failed checks stop publication.
The latest check run is used; an older success cannot mask a newer failure.

Immediately before publication, the gate rechecks draft state and all bound asset
identities/digests, then publishes by the verified release ID, not just a mutable tag
lookup. If checks are pending, wait and rerun after they complete. Do not manually
publish, edit assets or move tags while a release workflow is active: GitHub does not
provide one atomic transaction covering asset writes and publication, and workflow
concurrency cannot lock external administrator actions. Published versions are never
rebuilt or overwritten by this workflow; use a new version for corrections.

The `verified-release-evidence` workflow artifact and job summary retain source and
asset hashes. A failed build, verifier or metadata tool leaves the release unpublished.

To run the gate locally (requires cosign, gh, Python 3.11+, exiftool, msiinfo, dpkg-deb,
rpm and 7zz/7z; the directory must contain only the nine release assets):

```sh
bash scripts/release/verify-release.sh /path/to/assets vX.Y.Z X.Y.Z \
  OctopusGarage/mesh-talk refs/tags/vX.Y.Z <exact-source-commit>
```
