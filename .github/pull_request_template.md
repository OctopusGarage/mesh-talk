## Summary

<!-- One sentence describing what this changes and why. -->

## Checklist

- [ ] `make test` passes (or `cargo test -- --test-threads=2`)
- [ ] `./scripts/check-health.sh` passes before requesting review
- [ ] Strict workspace Clippy passes (`cargo clippy --workspace --all-targets -- -D warnings`)
- [ ] `cargo fmt` applied (no formatting diff)
- [ ] New behaviour is covered by assertions that fail for the old/incorrect behaviour
- [ ] Applicable restart, revocation, persistence failure, boundary and account-switch races are covered (or gaps explained below)
- [ ] Platform-specific changes have native evidence on affected platforms; browser mocks alone do not establish native correctness
- [ ] Networking changes ship an integration assertion (discovery/relay)
- [ ] AI eval / smoke gate run when code, prompts, model config, agent rules, or core flows changed (`make eval-smoke`; `make ai-eval` for prompt/model/agent changes; `make smoke-full` for release-critical flow changes)
- [ ] Specs under `specifications/` updated if protocol/flows changed

## Validation

<!-- Paste the commands you ran and their result. -->

<!-- See docs/evals/quality-gates.md. Record the exact tested commit, platform,
test names/actions links and failing-baseline evidence. Separate PASS, FAIL,
SKIPPED and INCONCLUSIVE. Coverage percentages alone are not proof of correctness. -->

## Risks and evidence gaps

<!-- List applicable scenarios not exercised, surviving mutations and their
triage, runner limitations, compatibility risks, and rollback. "Not applicable"
is acceptable with a reason; do not claim an unobserved platform is verified. -->
