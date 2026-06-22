---
description: Hunt for real bugs in the core via adversarial read-only audits, with verified findings
argument-hint: "[module | all]"
allowed-tools: Task, Bash
---

Find real, verified bugs in Mesh-Talk's core by dispatching the `bug-hunter` subagent(s).

Argument: `$ARGUMENTS` — a module/concern to focus (e.g. `ratchet`, `sync`, `parsers`,
`node`), or `all` / empty for a full sweep.

- **Scoped** (a module given): dispatch ONE `bug-hunter` focused on that module.
- **Full sweep** (`all` or empty): dispatch SEVERAL `bug-hunter` agents **in parallel**, each
  owning a distinct high-risk area, so they cover the surface without tripping over each other:
  1. untrusted-input **parsers** (every `decode`/`deserialize`/`open` on `&[u8]`)
  2. **crypto** (ratchet + channel sender-key + dm: key/nonce reuse, FS, signatures)
  3. **eventlog + sync** (convergence, ordering determinism, frame bounds)
  4. **node** multi-device/account routing, reactions, search, history
  5. **postoffice + discovery** (storage bounds, roster eviction, announce verification)

Each subagent is read-only and reports only VERIFIED findings (file:line + repro + severity).
Collect their results, de-duplicate, and present a single prioritised list (critical→low). Do
NOT fix anything here — this command finds and reports; fixing is a separate, reviewed step.
If nothing real is found, say so — that's a valid result.
