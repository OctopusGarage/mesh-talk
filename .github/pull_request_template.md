## Summary

<!-- One sentence describing what this changes and why. -->

## Checklist

- [ ] `make test` passes (or `cargo test -- --test-threads=2`)
- [ ] `make lint` passes (`cargo clippy -- -D warnings`)
- [ ] `cargo fmt` applied (no formatting diff)
- [ ] New behaviour is covered by tests
- [ ] Networking changes ship an integration assertion (discovery/relay)
- [ ] AI eval / smoke gate run when code, prompts, model config, agent rules, or core flows changed (`make eval-smoke`; `make ai-eval` for prompt/model/agent changes; `make smoke-full` for release-critical flow changes)
- [ ] Specs under `specifications/` updated if protocol/flows changed

## Validation

<!-- Paste the commands you ran and their result. -->
