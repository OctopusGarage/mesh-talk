# Local test feedback loop

During feature development, run the smallest tests that cross the changed
boundary. The pre-commit hook checks staged whitespace and source formatting;
it does not launch test suites. Record the focused commands and results in the
PR. Before opening or updating a PR, commit the candidate and run
`./scripts/prepare-pr.sh` with affected Rust selectors and Playwright specs.
It runs affected platform preflight, followed by full health (including
`make test`'s workspace test stage); `create-pr.sh` invokes it automatically before publication. CI also runs the comprehensive suites. Run backend or full UI E2E sooner
when explicitly requested or when the changed behavior needs process/browser
evidence.

| Changed boundary            | Fast local command                                                                                          | Broader trigger                                             |
| --------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| Core unit module            | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync`                              | `make test` before PR                                       |
| Host settings/session       | `make test-focused TEST_PACKAGE=mesh-talk TEST_FILTER=owner_scoped_node_handle`                             | `make test` before PR                                       |
| One Rust integration target | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_TARGET=decoder_smoke`                                   | `make e2e` when requested or for release validation         |
| Frontend state/controller   | `cd frontend && npm test -- src/features/chat/settingsDialogSession.test.ts`                                | `npm run test:coverage` in PR preflight                     |
| Frontend rendering          | Run the affected component test, for example `cd frontend && npm test -- src/features/chat/Sidebar.test.ts` | Pass relevant Playwright specs to `scripts/pr-preflight.sh` |

Use `cargo test -p <package> --lib <module_or_test_name>` directly when a
filter is easier to spell than a Make variable. `make test-focused` requires a
package plus either a filter or an integration target, so an omitted selector
cannot silently start the whole workspace. Ignored process tests require an
explicit `cargo test ... -- --ignored --test-threads=1`; the focused target does
not enable them automatically.

## Change-aware development plans

Use the affected-test planner during development:

```bash
make test-plan
make test-changed
# Include committed changes since a comparison branch, plus current edits:
node scripts/validation/changed.mjs --base origin/main --json
```

The default comparison is `HEAD`, so staged, unstaged and untracked edits are
included. An explicit base uses its merge base with HEAD and includes committed
changes since then. Deleted files remain selection inputs; renames are treated
as deletion plus addition. Preview reads source/contracts only: it does not
compile, start browsers or run tests. It exits with status 2 when selection is
blocked. `--json` emits the same plan as structured data for agents.

`validation.json` files beside each Rust crate, `src-tauri/`, `frontend/` and the
validation tooling own focused selectors, explicit downstream consumers and
escalation notes. Paths are repository-relative exact names or prefixes ending
in `/`; IDs are unique across contracts. Consumer links expand transitively and
selected Rust filters are combined per package. Add shared fixture consumers at
the owner's contract rather than copying selection rules into callers. Contract
changes themselves select the planner's regression tests.

Frontend selection also follows transitive source imports, type imports,
re-exports, literal dynamic imports and literal Vitest mocks. It includes all
importing unit tests and retains former edges for changed/deleted source. The
registered unit-file convention is `src/**/*.test.ts`, matching Vite. Tests that
share an imported fixture therefore run together. Runtime IPC/events, computed
imports, glob discovery and configuration dependencies cannot be established
completely through static imports. Computed discovery produces an escalation
note; review runtime wiring and select browser/native evidence where needed.

Automatic Rust contracts initially cover account-profile ownership, desktop
session/admission and integration/MCP boundaries. Integration policy expands
through its service, host, IPC, client and desktop/MCP consumers. Most other core
modules have **explicit-selection-only** IDs such as `core-transport`: their
public Node/host consumers need a contextual decision. Shared configuration,
lockfiles, CLI entry points, unassigned fixtures and other unmapped changes block
execution. Extend the owning contract or explicitly select all affected owners
and explain the decision:

```bash
node scripts/validation/changed.mjs \
  --owner core-transport --owner desktop-session \
  --reason "Reviewed transport consumers; this change affects session retirement" \
  --run
```

An explicit selection acknowledges every otherwise unmapped path, so inspect the
preview first. The plan preserves those paths and the reason for review. This is
a developer decision, not evidence that all dependencies have been inferred.
Contracts with no executable tests (including after consumer expansion), invalid
references and missing registered test files fail rather than producing a pass.

Execution uses existing cross-worktree stage locks and keeps child processes
within its cancellation group, with a 30-minute overall queue/run deadline. Rust
compilation uses the shared heavy lock; focused frontend and Node checks use
separate locks so they do not queue behind a full browser gate. If edits change
the required resource class while queued, execution fails and requires a new plan.
It compiles only selected Rust library targets, checks every filter against actual nonignored test names before running
any tests, and invokes each test binary once with the combined filters. It never
automatically enables ignored tests or launches workspace/E2E suites. Each Node
diagnostic must run passing tests; every selected Vitest file must report at
least one passing assertion. Output separates Cargo compilation/inventory from
test execution time; existing stage logs retain queue time and overall duration.

For parallel agents, assign one module owner and its focused plan to each agent.
After combining their changes, one integration owner reruns the combined plan
and the applicable mandatory gates. Reuse remains limited to the existing clean,
revision-bound evidence policy; dirty edits never create reusable success.

This command selects development tests, not static checks or release evidence.
Run affected formatting/lint/type checks separately. Expand validation for public
interface, persistence, crypto, authorization, browser and native behavior changes,
using the boundary table above. Existing PR preflight, full-health, protected CI
and release obligations remain unchanged. Compare median/p95 feedback latency,
compile time, resource queue time, first-attempt failures and defects caught only
by broader tests before claiming a speedup.

## Local evidence reuse and resource ownership

Local gates use `scripts/validation/stage.mjs` for diagnostics, frontend builds,
lint/typechecking, unit coverage, workspace tests, and browser runs. An identical
successful stage on a clean committed HEAD can be reused for up to 24 hours.
Receipts bind the revision, working directory, platform, tool versions, command
arguments, relevant environment and required output digests. Base-sensitive local
preflight also binds the resolved comparison ref. Changed or missing artifacts,
failed/interrupted stages, dirty sources, and changed inputs require execution.
`CI=1` and `MESH_VALIDATION_FRESH=1` always execute checks. Advisory and secret
scans continue to run directly rather than reusing time-sensitive scan verdicts.

`make test` and full health share the same workspace-test stage. Preflight and
health share frontend static/unit stages. The PR wrapper still invokes preflight;
its local work can reuse intact evidence, while both required remote platforms
always execute. An unavailable environment therefore still blocks publication.
No local receipt replaces a protected GitHub check. Local receipts are trusted
workspace acceleration, not signed attestations against a hostile local user.

Heavy local stages share a resource lock across Git worktrees, preventing browser,
coverage and Rust compilation workloads from competing. Node unit tests default
to two workers and Rust tests to one thread; explicit `VITEST_MAX_WORKERS` and
`RUST_TEST_THREADS` values override those defaults and invalidate matching evidence.
Each browser stage owns an available port and private results/report directory.
The full browser process has a two-hour ceiling; selected/portable processes have
30 minutes. `MESH_BROWSER_TIMEOUT_SECONDS` can set an explicit 1–7200 second
ceiling and participates in evidence identity. Individual scenario timeouts remain
unchanged.
Coverage cleanup and merging occur inside that stage's exclusive resource window.

Before browser coverage or remote execution, preflight compiles the workspace
library test executables once and checks selectors against libtest's actual test
names without executing tests. Invalid filters fail locally with the crate and
selector. Choose module filters present on the preparing platform for tests whose
individual names are platform-specific. Remote execution still rejects selectors
that run zero tests on either guest.

Per-stage logs and atomic success receipts live under `target/validation/`.
Console output reports queue time, execution time, reuse, and running-stage
heartbeats. Platform preflight reports each platform's completion immediately and
prints progress while the other continues. Missing/corrupted evidence is a cache
miss; it is never a successful verification result.

## CI ownership and browser sharding

The standalone `scripts/check-health.sh` still runs the full local gate before
requesting review. Hosted workflows divide the same responsibilities so that
independent checks run concurrently and heavy suites are not repeated by health.

| Owner                                 | Checks                                                                                                                                                                                                                 |
| ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `check-health.yml` platform jobs      | Frontend formatting, ESLint, source/E2E TypeScript, npm advisory policy, dependency audits on Linux/macOS/Windows                                                                                                      |
| `check-health.yml` source job         | Rust formatting, typos, cargo-deny, cargo-machete, shellcheck and actionlint on Linux                                                                                                                                  |
| `ci.yml`                              | Frontend builds, built-JS compatibility, unit coverage, diagnostic/Python regressions, Clippy `--all-targets`, explicit workspace builds and Rust tests on all three platforms; Linux Rust/E2E coverage and eval smoke |
| `e2e-ui.yml`                          | Full Chromium regression on Linux/macOS/Windows, Linux browser coverage, lite checks, portable browser matrices and scheduled extended scenarios                                                                       |
| Independent security/native workflows | Full-history Gitleaks, dependency review, CodeQL, mutation/sanitizer checks, backend process rigs and native desktop evaluation                                                                                        |

Every existing protected health name now requires both the source scanner job
and the complete platform static matrix. A failed, cancelled or skipped dependency
fails the gate. Health runs for every PR base so moving source checks out of CI
does not leave non-main PRs without them. CI retains the weekly Rust run formerly
provided by scheduled full health; UI E2E runs the full browser matrices daily.
A manual health workflow dispatch runs static checks; dispatch CI and UI E2E too
when complete hosted verification is needed. The local full-health command is
unchanged. Required CI, health, UI E2E and backend check names remain stable.
The protected `Full Linux UI regression` name now belongs to the coverage merge
job, which explicitly fails when its browser shard matrix is unsuccessful.

Full Chromium tests use four Playwright shards per OS, with one worker and zero
retries per runner. This keeps the existing per-machine rendering budget while
running all scenarios. Portable/native scenarios and snapshot assertions remain
in place. Sharding trades more concurrent runner slots and setup work for shorter
elapsed time; account concurrency limits can reduce the gain.

Each Linux shard uploads raw browser coverage plus its source SHA and shard
identity. The coverage job generates the unit line map once, then requires all
four distinct current-source shards before collecting their parts and producing
one browser LCOV upload. It prefixes filenames so reports from different runners
cannot overwrite each other and clears old output only after validating every
input. The final UI gate requires successful browser, coverage, merged-report,
lite and portable jobs, plus Firefox when selected. Blob reports and failure
traces are retained per OS/shard/attempt, and an HTML report is merged per OS even
when browser execution fails.

The Rust build cache maps the workspace root to `target/`, matching
`cargo metadata --no-deps --format-version 1`. The CI cache has its own build
configuration key; native release, mutation and sanitizer caches stay separate.
Cached build output never replaces executing checks on the current revision.

To assess performance after deployment, compare at least several unchanged-scope
runs with warm caches and a cold-cache run. Record time from PR update to all
required checks passing, queue delay, slowest browser shard, native build/test
steps, total runner minutes, and first-attempt failures. The pre-change October 17
sample had 27–38 minute health jobs and a 31.5 minute full Linux UI job. These are
baseline observations, not promised speedups; hosted runs must establish the new
latency and reliability.

## Automatic platform checks before PR creation

Commit the candidate and use the repository's PR entry point:

```bash
./scripts/create-pr.sh \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts \
  -- --draft --fill
```

Arguments before `--` select tests; arguments after it go to `gh pr create`.
Use filters and browser specs relevant to your change. Core, host, integration and MCP filters can be repeated. Use
`--integration-filter policy::` and `--mcp-filter tests::` for the two integration
crates; a changed Rust crate requires its corresponding filter. Shared Cargo,
toolchain or Clippy configuration requires selections for all four crates.
Examples, benchmarks, package configuration and Tauri build inputs also select
their owning package. Each selected guest package runs Clippy with
`--all-targets -- -D warnings` before its focused tests. Use `--core-test decoder_smoke` to select a nonignored core integration
target instead of a library filter. A selector matching zero tests fails rather than producing misleading
evidence. This selects unit tests and explicitly named browser scenarios; it
does not automatically launch the full workspace, ignored process rigs, or
the full browser suite.

Changes to the platform runner, PR entry point or source-evidence helpers also
select four fixed Node workflow regression files on both guests. This catches
Git, shell and path compatibility independently of application tests; a
workflow-only diagnostic plan does not install frontend dependencies.
Verification archives suppress export substitutions and text/filter conversions
in private Git metadata so the archived bytes match the committed source.

Frontend changes require affected browser specs. Owning `prScenarios` rules in
`validation.json` add required visual specs automatically, so an unrelated
explicit spec cannot replace shell/layout/theme evidence. The plan preview
shows the selected scenario owners and required native platforms. Renames
include both old and new paths. Frontend unit coverage is a shared local stage; the two platform guests do not repeat
the whole frontend unit suite. To target another PR base, pass
`--base origin/release` before `--`; the wrapper uses that same base for
change selection, coverage and `gh pr create --base release`. A local revision
such as `HEAD` cannot stand in for a PR target branch.

`scripts/pr-preflight.sh` automatically sends the same test plan to Linux
Docker and Windows over SSH after its existing local static, coverage and
portable browser checks. Cheap local failures therefore block remote builds
before they consume time. Both platforms test an archive of committed HEAD. Workspaces are
isolated from installed applications and post-office services. Both results
are collected even if one fails. Logs and revision-bound reports remain
untracked under `target/pr-platform/`; dependency and build caches can be
reused, but successful verdicts are never reused for another revision. Required
remote execution is never replaced by local receipts.
On macOS the preflight holds a temporary `caffeinate -is` assertion until it
exits, preventing idle/system sleep on AC without changing power settings or
keeping the display lit. This avoids Docker timer failures during maintenance
sleep. Only one local preflight should run at a time. Avoid overlapping Docker
compilation and browser workloads with the timing-sensitive network fixtures.
Windows reuses an installed Cargo home only when its resolved physical
directory is on drive D, including a C: junction that points to D. This
preserves that directory's registry and network configuration without logging
its contents. Otherwise, Cargo uses the isolated D: cache. Source, build
output, browser downloads and temporary directories remain isolated.

For an existing PR, run `scripts/prepare-pr.sh` before pushing the candidate.
Use `scripts/pr-preflight.sh` alone for focused platform investigation; it does
not certify the complete publication gate. After a broader run has produced evidence, a focused follow-up can
use `--base <previous-candidate>` with the tests affected by the delta. Retain
the earlier reports and record both revisions, the comparison base, remaining
failures and exact test scope in the PR. Shared fixture changes require all
cases that use that fixture. An incremental pass is not whole-branch
certification; final required CI still runs on the complete current PR head.
The new-PR wrapper always uses the actual `origin/<target>` base.

Preview the plan without starting Docker or SSH:

```bash
node scripts/pr-platform/run.mjs --plan \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts
```

| Setting                            | Default / purpose                                                                                                           |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `MESH_PR_WINDOWS_HOST`             | `windows10`; an SSH host with noninteractive access, Rust, Node and Git tools available                                     |
| `MESH_PR_WINDOWS_ROOT`             | `D:/MeshTalk/PRPreflight`; Windows source, caches, browser downloads and temporary data must stay on drive D                |
| `MESH_PR_LINUX_HOST`               | Optional SSH host, such as `mac2015`, to run Docker remotely; unset keeps local Docker.                                     |
| `MESH_PR_LINUX_PLATFORM`           | Optional `linux/amd64` or `linux/arm64`; remote Docker defaults to `linux/amd64` and rejects mismatched guest architecture. |
| `MESH_PR_LINUX_DOCKER`             | Remote Docker executable; defaults to `/Applications/Docker.app/Contents/Resources/bin/docker`.                             |
| `MESH_PR_LINUX_ROOT`               | Remote temporary source/evidence root; defaults to `/tmp/mesh-talk-pr-preflight`.                                           |
| `MESH_PR_DOCKER_IMAGE`             | Optional prepared image override; the default image is built from `scripts/pr-platform/Dockerfile`                          |
| `MESH_PR_PLATFORM_TIMEOUT_SECONDS` | `1800`; platform execution limit, not a protocol timeout                                                                    |
| `PATCH_COVERAGE_BASE` / `--base`   | `origin/main`; the comparison base for changed-file planning                                                                |

To run native Linux amd64 containers on the Intel Mac while preparing a PR:

```bash
MESH_PR_LINUX_HOST=mac2015 MESH_PR_LINUX_PLATFORM=linux/amd64 \
  ./scripts/create-pr.sh --core-filter discovery:: --host-filter discovery \
  e2e/settings-recovery.spec.ts -- --title "Your PR title" --body-file /tmp/pr-body.md
```

The remote adapter uploads the same committed source archive, plan and Dockerfile
used by local Docker. Mounts and named caches belong to the remote Docker host;
local filesystem paths are never passed as remote bind mounts. Selected test
containers enable IPv6 loopback so routed discovery tests exercise `::1` even
when Docker's bridge network has no IPv6 subnet. This is a container setting;
the Docker daemon's network configuration stays unchanged. Test
reports and logs are copied back before the source revision and archive digest
are validated. Remote run directories remain under the configured temporary root
for diagnosis; named caches are reusable. Docker's executable directory is added
to the remote command's PATH so GUI-installed credential helpers work over SSH.
SSH uses configured host keys and noninteractive authentication, as for Windows.

Local PR browser traces, screenshots and the HTML report use a unique directory
under `target/pr-preflight-playwright/<commit>-<process>/`. They do not share
Playwright's default `frontend/test-results` or `frontend/playwright-report`
directories with the full health gate, so starting a preflight cannot delete
another active run's traces.

Required environments being unavailable blocks preparation; it is not a pass.
Fix the environment or report the blocker. Documentation-only changes without
explicit selectors report platform tests as not applicable. Container reports state the actual
architecture: Linux ARM64 on an Apple Silicon Mac does not replace Linux
x86-64 sanitizer coverage. Browser checks exercise the real frontend with its
test IPC fixture. Native webview, desktop input, multicast across real hosts,
and ASan remain separate CI or explicitly requested acceptance gates.

The PR creation wrapper also checks that HEAD and the working tree did not
change during verification. Alternate head/repository options and `GH_REPO`
overrides are rejected. Use it instead of invoking `gh pr create` directly
so the platform checks are part of the normal workflow. GitHub branch
protection remains the final enforcement for CI; a local script cannot
intercept arbitrary GitHub CLI or website actions.

The current four-crate Rust workspace separates protocol logic from
the Tauri host. Within the core, `node/node.rs` still coordinates many domain
modules; keep moving bounded parsing and state transitions behind narrow
module interfaces so `--lib <module>` reaches them. In the host, ownership
validation belongs to `NodeState`; commands should use its owner-scoped handle
instead of repeating runtime locks and session comparisons. In the frontend,
the Settings dialog's session controller and store tests provide narrow targets;
continue extracting field-specific state transitions from the presentation
component as those fields change. Prefer tests at those interfaces over extra
process/browser cases for pure state rules.

For local speed, keep the existing Cargo build cache and the test-only cheap
KDF parameters. Avoid invoking Clippy, full Rust tests, coverage, and browser
tests after every small edit; run the focused test during the loop, then the
appropriate static check and final gates at the triggers above. Do not change
the CI commands or the explicit `make test`, `make e2e`, and health-check
entries: they provide the comprehensive evidence for review and release.

## Native requirements and publication evidence

The owning core and desktop sidecars require native acceptance for discovery,
process/session lifecycle and OS-specific UI boundaries. `host` resolves to the
preparing OS. Additional `darwin`, `win32` or `linux` entries require evidence
from each named platform; one platform cannot stand in for another. These rules
are maintained acceptance boundaries, not exhaustive dependency inference.
Review escalation notes and add owning rules when new runtime boundaries appear.

Run the existing native runner from an unchanged, clean committed checkout with
an unlocked desktop. Build its current binaries first:

```bash
cd frontend && npm ci && cd ..
node scripts/validation/native-build.mjs
node scripts/diagnostics/hidden-contacts.mjs
MESH_PR_NATIVE_REPORTS='["target/hidden-contacts-native/report.json"]' \
  ./scripts/prepare-pr.sh --host-filter owner_scoped_node_handle
```

Linux needs the display/session prerequisites documented in
[platform validation](platform-validation.md) and the native CI workflow. The
current native harness runs its complete acceptance scenario set; there is no
claim that it supports per-scenario filtering. It runs only when the selected
boundary requires it. Keep reports and all referenced JSON, PNG and log files
together; paths are supplied as a JSON array to preserve spaces. Preparation
revalidates real native observations, artifact hashes, platform, source SHA and
the runner's clean-source observation and verified build provenance.
The build helper pins the committed source and hashes the frontend assets and
actual binaries. A clean checkout alone cannot certify previously built binaries. Cargo executable
records must match the hashed paths; redirected `build.target` output is rejected
rather than allowing old default-path binaries to be stamped as current. Old reports lacking that observation
must be regenerated. Missing environments or evidence block preparation before
heavy health/platform work; they never become a pass. Direct platform execution
also verifies native requirements; `--plan` only previews them.

Preparation runs the outer affected platform preflight before the comprehensive
health gate, catching selected platform failures sooner. It forces full health
mode even when `FAST=1` is inherited. Cheap formatting, ESLint and TypeScript checks run
before diagnostic suites and native compilation. Existing successful stages can
be reused under their source/tool/configuration and artifact checks; full health
and platform evidence remain distinct. HEAD, working tree and comparison ref
must remain stable. Hosted CI still verifies its complete matrices and external
security policy; local success does not certify every hosted environment.

For parallel agent work, one integration owner collects changes, reviews shared
interfaces, prepares the combined candidate and pushes it. Individual agents
commit locally and report focused tests. After a CI failure, reproduce the
failure and run affected neighboring checks before publishing a coherent fix.
Do not push each intermediate attempt. Keep failure evidence in untracked output
or PR artifacts, not historical reports in the repository.

Measure first-attempt PR success, corrective pushes per PR, time to the first
actionable failure and runner minutes per merged PR. Classify failures as code,
scenario/fixture, platform/environment or external service before deciding
whether an earlier check or a harness correction is appropriate. Retain the
existing cancellation, sharding and comprehensive CI gates.

## Explicit physical network acceptance

These opt-in checks use disposable profiles and test-only `fast-test-kdf`
binaries. Do not deploy those binaries as permanent services. They require the
four authorized SSH hosts and their actual Tailscale routes; failure to reach a
host is not a protocol pass. Windows fixtures, working directories, profiles and
temporary files belong on drive D. No installed relay or network service needs
to be stopped.

- `scripts/e2e-routed-sibling.py`: build `network-acceptance` for arm64 and Intel
  macOS, then exercise real SDK pairing, restart, history sharing off/on and
  continued catch-up across mac2015 and mac-mini.
- `scripts/e2e-federation-recovery.py`: build `mesh-talk-node` for arm64 macOS,
  Intel macOS and Windows GNU, plus the arm64 `network-acceptance` example.
  Exercise preferred-relay outage, a surviving three-relay route, consented file
  delivery with sender restart and SHA-256, and bridge partition/recovery.
- `frontend/e2e/call-media-physical.spec.ts`: build the `network-acceptance`
  example for native macOS and Windows GNU, then run
  `MESH_TALK_PHYSICAL_MEDIA=1 npx playwright test e2e/call-media-physical.spec.ts`
  from `frontend/`. Override `MESH_TALK_MEDIA_LOCAL_IP` and
  `MESH_TALK_MEDIA_REMOTE_IP` with the actual directly reachable Tailscale IPs.
  It uses shared browser media
  fixtures, authenticated backend signaling and an independently checked
  Tailscale ICE pair between Chromium and Windows Edge. Synthetic capture
  verifies transport/decode and cleanup, not native WKWebView or camera quality.

Set `MESH_TALK_E2E_MAC2015_SSH=ys-aquria@192.168.3.72` when the `.local` alias
is unavailable; `MESH_TALK_E2E_BASE_PORT` isolates disposable port ranges. Keep
logs and machine-specific evidence outside git. Four hosts at two physical
sites cannot establish a three-independent-LAN acceptance claim.
