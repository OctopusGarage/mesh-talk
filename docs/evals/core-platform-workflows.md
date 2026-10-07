# Core workflow and platform evaluations

Product evaluations are separate from prompt/agent/model evaluations. A successful
build, coverage percentage, PNG file signature or mocked browser session alone is
not evidence that the shipped desktop workflow works.

## Layers and honest boundaries

The protected CI matrix runs Rust workspace unit/integration tests and frontend
Vitest unit tests on Linux, macOS and Windows. Linux records Rust `llvm-cov` and
frontend V8 coverage; the frontend report includes every production `.ts` and
`.tsx` file, including files with zero unit coverage. The `src/store` line gate
is 70%. Run `cd frontend && npm run test:coverage` locally to inspect the
frontend report in `frontend/coverage/`. Browser and native E2E assertions do
not contribute to this unit coverage percentage, so low component percentages
must be read alongside the scenario matrix below.

| Layer | Systems exercised | Platforms | Boundary |
| --- | --- | --- | --- |
| Backend core | Real CLI processes, signed discovery, encrypted TCP, DM/channel delivery, process restart, relay delivery, received file bytes | Linux, Windows, macOS | No renderer; discovery can use its signed unicast fallback, so a passed scenario does not prove multicast-only discovery |
| Native core | Actual Tauri builder/webview, embedded dev-only W3C driver, real CLI peer, persisted production stores, native keyboard/dialog input | Linux GTK/WebKit, Windows WebView2, macOS WKWebView/AppKit | No IPC replacement; test example uses existing cheap test KDF, separately built CLI and production application retain production KDF |
| Portable browser | Real React UI and visible interactions with a deterministic mocked IPC/event boundary | Chromium on Linux/Windows/macOS; WebKit on macOS | Guards UI wiring/layout, not native transport, OS chrome or Rust authentication |
| Model-backed AI | External model responses scored against prompt/agent cases | Configured provider | Not a product E2E; an unavailable provider means **not run**, never a real-model pass |

Hosted runners cannot establish behavior for every physical camera, microphone,
GPU, monitor scale, compositor or LAN firewall. Those remain explicit manual
acceptance checks; do not convert unavailable devices to passing observations.

## Backend execution and evidence

Run from the repository root:

```sh
EVAL_OUTPUT_DIR=/tmp/mesh-talk-backend-eval node scripts/evals/backend-runner.mjs
```

The runner selects the four integration test targets explicitly and runs ignored
tests serially, continuing through independent suites after a failure while
retaining the overall failing verdict. It requires all five named scenarios exactly once, a matching
Cargo test count and zero failures/ignored/filtered tests. An exit-zero invocation
which selects no tests is a failure. The JSON report binds source revision,
platform, timings and SHA256 log digests. Artifacts contain test observations and
redacted logs, never fixture keystores or account directories.

Normal CLI shutdown sends `/quit` and waits for the owned child to exit before
reusing its store. The test helper never removes a child's lock to manufacture a
successful restart. Forced termination remains a separate recovery boundary,
particularly on Windows where conservative stale-lock handling needs care.

The offline relay test must stop the sender before the receiver returns. A test
with the sender still online cannot independently establish post-office delivery;
ordinary direct synchronization could satisfy the same message assertion. Failure
of this stricter route-isolated scenario must be investigated, not hidden by
relaxing it or removing it from the required matrix.

## Native scenarios and evidence

The native runner retains the 13 contact-hiding/invisible-mode scenarios and adds
registration/sign-in, logout/invalid-password recovery, signed discovery,
bidirectional DM, Unicode/multiline/long content, channel delivery, attachment
rendering, history after real process restart, theme persistence, profile/avatar
geometry and settings geometry at default and minimum window sizes.

At native app readiness, the dev-only driver also requires the handled Mesh-Talk
tray icon to exist and Tauri's unhandled default `main` tray icon to be absent.
This checks actual Tauri registration on each runner; it does not simulate an OS
tray click or inspect stale icons retained by a Windows shell after a crash.

Each passing scenario supplies JSON observations, a screenshot and a redacted
process log. Schema 2 requires the tested source revision and SHA256 digests for
all three files. Validation requires typed scenario observations and fully decodes
bounded RGB/RGBA PNG pixels, rejecting corrupt, truncated, blank or transparent
screenshots. These checks prevent incomplete/stale evidence from passing; they
do not replace the scenario's interaction and layout assertions.

Definitive revision evidence comes from a committed clean checkout. A local
uncommitted run validates the current working tree; its HEAD SHA identifies the
base revision, not proof that the unmodified base commit has the new behavior.

Native attachment evidence intentionally proves incoming manifest persistence and
generic-file rendering **only**. The OS save picker is outside the embedded DOM
driver and its default destination may be outside the isolated fixture. The
report explicitly marks download as unverified; actual transferred file bytes
are asserted separately by the real CLI integration scenario.

On macOS, avatar clearance is compared with actual AppKit standard-button bounds
converted to the content view. Other platforms report this macOS-specific chrome
observation as unavailable, not as successful native traffic-light coverage.

Native input requires an unlocked, active desktop session. A locked local macOS
session can still render WKWebView snapshots while refusing application/key-window
activation. That is an environment failure, not a successful interaction test;
retain focus diagnostics and rerun on an unlocked desktop or hosted runner.

## Portable browser execution

```sh
cd frontend
EVAL_BROWSER=chromium EVAL_TIER=core npx playwright test --config playwright.portable.config.ts
EVAL_BROWSER=webkit EVAL_TIER=extended npx playwright test --config playwright.portable.config.ts
```

Core scenarios cover both 760×520 and 1040×720 viewports without sharing
Linux-specific pixel baselines across operating systems. They check registration
and invalid-password responses, logout, bidirectional DM/channel rendering,
Unicode/multiline/wide content, interactive avatar/profile geometry, repeated
dialog Tab/Shift+Tab containment, light/Chinese interactions, and theme restoration
after reload. These remain mocked IPC tests, not Rust authentication assertions.

Every scenario attaches a screenshot, trace and source/platform/browser-bound
JSON. Page errors fail the test. A screenshot failure still leaves failed JSON;
a crashed page must not erase the failure record. Screenshot presence alone is
not the oracle: visible controls, content, hit targets, bounds and focus are asserted.

Extended runs exercise all existing palettes, locale changes, resize cycles and
repeated dialog opening. Their DOM-count assertion is a **DOM retention smoke**,
not native RSS, heap collection or a proof of absence of memory leaks. Profile
focus restoration to the avatar after dismissal is currently not established;
modal focus containment is tested separately and must not be presented as that
stronger accessibility guarantee.

UI CI keeps the full Linux regression suite and adds Chromium on three platforms
and macOS WebKit. Nightly/manual extended runs additionally require Linux Firefox.
The protected `Playwright UI E2E` aggregate requires every selected matrix; Firefox
is deliberately not selected for bounded PR core runs and cannot be skipped during
an extended run.

## CI gating and diagnostics

`Backend E2E` runs the real backend three-platform matrix and calls the reusable
native workflow. Its aggregate preserves the protected `Multi-process E2E` check
name and fails unless every backend and native platform succeeds. Failed, skipped
or missing child jobs cannot make this aggregate green. Artifacts are uploaded
on both success and failure with bounded retention.

Hosted macOS configures only the fixture multicast group as a local route and
requires a receive-capability probe before the real tests. This is runner setup,
not a product fallback or proof of multicast capability on an arbitrary LAN.

Inspect evidence before classifying a failure: environment startup/capability,
driver/input, assertion, protocol/delivery, persistence, and cleanup are distinct
failure categories. Publish which platforms/scenarios actually ran, which failed
and which were not run. A declared matrix is not an executed verification result.
