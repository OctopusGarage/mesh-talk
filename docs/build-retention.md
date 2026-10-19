# Local build retention

Managed Rust builds and tests share a machine-local retention policy. Successful
`make build` commands archive release binaries; `make tauri-build` archives the
newly generated native bundles. Keep the latest **five successful build sets**
across all registered checkouts and worktrees. Each set includes its commit,
branch, dirty-worktree status, source path, and timestamp.

After a successful managed command, cleanup measures the combined Cargo targets
and evicts compiler caches from the least recently used checkout until usage is
at most **40 GiB**, where possible. A checkout's `deps`, `.fingerprint`, `build`,
`incremental`, and `examples` directories are evicted together within each idle
debug, release, or cross-compilation profile. Coverage profiles are also eligible.
Removing these caches makes the next affected build recompile; runnable binaries,
native bundles, documentation, source, and application data remain available.
The budget is soft when preserved files alone exceed it. Archived packages are
separate from the compiler-cache budget and have a count limit, not a byte limit.
Automatic cache scans run at most once every ten minutes; package retention is
checked after every successful managed command. Post-command cleanup also keeps
profiles whose cache files or verified managed-use marker were active within the
last **ten minutes**. This avoids repeatedly deleting a fresh cache when active
or preserved outputs already keep total usage above the soft budget. Older idle
profiles and worktrees remain eligible. Coordination lock timestamps do not count
as cache use, so cleanup itself cannot renew this window.

Manual and scheduled `clean` commands always scan and retain their immediate
budget policy; the automatic idle window does not apply to them. Active profiles
are skipped while cleanup continues through idle profiles and other worktrees.
An incomplete cleanup reports excess usage and does not advance the scan interval,
so the next successful command can retry immediately. The window reduces cache
churn during development; it does not increase the configured budget or guarantee
that scheduled cleanup will preserve a warm profile.

## Commands

Requires Python 3.11 or later and Git.

```sh
make clean-builds-dry-run
make clean-builds

# Register another clone; its worktrees are discovered automatically too.
python3 scripts/build-retention.py clean --checkout /path/to/another/mesh-talk

# Protect custom commands from concurrent cleanup.
python3 scripts/build-retention.py run -- cargo test -p mesh-talk-core --lib account_sync

# Opt out of the automatic idle window for this command.
python3 scripts/build-retention.py run --min-idle-seconds 0 -- cargo check -p mesh-talk-core
```

An explicit `--checkout` is remembered after a real cleanup run, including runs
below the budget. A dry-run reports the proposed deletions without registering
checkouts or changing caches, packages, or the deletion log. It may create the
state directory and Cargo coordination lock files. The registry lives outside Git, so every worktree
using the mechanism shares it. Old branches need these script/Makefile changes
before their builds acquire the managed lock.

Managed Make targets include `build`, `tauri-build`, `install-deps`, `test`,
`test-focused`, `e2e`, `lint`, `tauri-dev`, `clean`, and `check`. Direct Cargo
commands are still supported, but do not automatically trigger this policy.
`run` executes commands from the repository root. Failed commands preserve their
exit status and do not archive or trigger cleanup. Archive/cleanup failures print
a warning and preserve a successful build's exit status.

## Configuration

| Setting                                                      | Default                              | Meaning                                                                                                                              |
| ------------------------------------------------------------ | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `MT_BUILD_KEEP` / `--keep`                                   | `5`                                  | Successful build sets to retain; at least one                                                                                        |
| `MT_BUILD_CACHE_GB` / `--budget-gb`                          | `40`                                 | Combined Cargo target budget in GiB                                                                                                  |
| `MT_BUILD_STATE_DIR`                                         | `~/.cache/mesh-talk/build-retention` | Shared archive, registry, locks, and log directory                                                                                   |
| `MT_BUILD_CHECKOUTS`                                         | empty                                | Additional clones, separated by the platform path separator                                                                          |
| `MT_BUILD_SCAN_INTERVAL_SECONDS` / `--interval-seconds`      | `600`                                | Minimum interval between automatic cache scans; `0` scans after every command                                                        |
| `MT_BUILD_CACHE_MIN_IDLE_SECONDS` / `run --min-idle-seconds` | `600`                                | Minimum idle time before post-command cache eviction; nonnegative; `0` disables the window; explicit/scheduled `clean` is unaffected |
| `MT_TARGET_CLEAN`                                            | `1`                                  | Set to `0` to disable eviction; successful builds can still be archived                                                              |

Archives are under `packages/`; `cleanup.jsonl` records deletion attempts with
paths, timestamps, and estimated bytes. Packages appear atomically after copying;
an interrupted copy remains hidden until a later cleanup removes it. The state
directory must remain outside all registered checkouts. Use one state directory
for all worktrees so their locks and retention counts are shared.

Archiving supports the native default `target/release` layout used by the Make
targets. Custom output locations, target triples, build directories, and profile
overrides skip automatic archiving and that command's cleanup with a warning.
Standalone cleanup only considers verified checkout-local `target/` directories;
external/custom target directories are not deletion candidates.

Managed commands refresh `.mesh-talk-build-use` in their checkout-local target.
An explicit `CARGO_TARGET_DIR` (or `CARGO_BUILD_TARGET_DIR` when it is unset) also
refreshes that target's marker when it is a verified Mesh Talk checkout's real
`target/` directory. This preserves a shared cache even when a Cargo cache hit
updates no artifacts. Unverified/external output directories and symlinked
markers are not written. The wrapper does not resolve command-line or Cargo
configuration-only output overrides, or separate build directories, into managed
markers; those inputs retain their existing output/archiving limitations. Recent
cache-file activity still receives the automatic window where the existing
collector owns that profile.

## Concurrency and scheduling

Managed builds hold one OS lock per checkout, protecting the command and package
capture. Cleanup skips that checkout if the lock is busy and continues through
other checkouts. A separate cleanup lock prevents overlapping collectors; a
package lock protects archive copies and retention. No machine-wide build lock
or process-name guard blocks unrelated idle worktrees.

Within an idle managed checkout, cleanup takes nonblocking exclusive locks on
each profile's `.cargo-lock`, `.cargo-build-lock`, and `.cargo-artifact-lock`.
These coordinate with raw Cargo invocations using both older and newer Cargo
locking layouts. A busy profile is preserved; idle sibling profiles can still
be cleaned. A Cargo build that starts during deletion waits on the profile lock.
Profile directories and lock files remain in place throughout cleanup.
The lock paths follow Cargo's
[1.95 layout](https://github.com/rust-lang/cargo/blob/rust-1.95.0/src/cargo/core/compiler/layout.rs)
and [1.96 layout](https://github.com/rust-lang/cargo/blob/rust-1.96.0/src/cargo/core/compiler/layout.rs).
The fixture suite verifies mutual exclusion with a real installed Cargo process.

This mechanism targets local filesystems with working OS file locks. Unsupported
or failed locks preserve the affected profile. OS locks release automatically
when their owner exits. Symlinked targets, profiles, and lock files are excluded,
and cleanup does not follow symlinks inside compiler caches. Use managed Make
commands to protect the entire Tauri bundling lifecycle as well as compilation.

`make dev` installs a macOS launchd fallback that runs every ten minutes and at
03:00. Busy profiles are retried at the next interval,
including for older worktrees without post-build hooks. To update it directly:

```sh
./scripts/install-auto-clean-macos.sh
```

The installer copies the runner outside the checkout so switching branches does
not remove it, and captures the current configuration and PATH. Reinstall after
changing these settings or updating the runner. It operates on the same shared
registry as post-build cleanup. Logs go to
`~/Library/Logs/mesh-talk-target-gc.log`.
