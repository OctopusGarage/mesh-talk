# Project decisions

Use this log to recover conclusions from discussions, implementation and
verification when they affect future work.

## Find or record a decision

- Before reopening a question, scan the topic headings or search this file for
  the feature, term or requirement ID. Follow links for current behavior and
  supporting evidence.
- Record a decision when it resolves a recurring question or changes what a
  later contributor should do. Use a descriptive heading and a short paragraph
  covering the decision and why it was made. Add a status when implementation
  or verification is pending. Include alternatives, evidence links and revisit
  conditions when they help the next reader make a decision.
- Update the existing entry when the conclusion or its status changes. If the
  old rationale still explains earlier work, mark it superseded and link to the
  current decision. Keep feature contracts and operating instructions in their
  owning guides.

## One shared decision log (REQ-022)

Use this file as the shared location for concise decisions from discussions,
implementation and verification. Link it from the agent instructions and
[documentation hub](README.md) so a later session can find it from either entry
point. A shared file keeps lookup and updates simple when the reader has little
prior context. Revisit the organization if topic headings and search no longer
make relevant decisions easy to find.

### Bound production group recovery turns

Production callers use `request_sync_turn_paged`, which negotiates indexed turns
and reports explicit completion, continuation or policy limits. The original
`request_round_paged` remains an explicit legacy drain API for compatibility.
Its production fallback, `request_legacy_turn_paged`, caps transfers at 200 events,
1 MiB encoded bodies and eight protocol steps. It bounds preparation and inventory
admission; oversized old-peer responses are rejected before apply. Interrupted
legacy exchanges retire their channel and do not establish convergence. No
seven-day retention or control/key-event filter is introduced.

### Negotiate indexed group turns instead of truncating Have

New group peers negotiate a domain-separated fingerprint capability probe before
using dedicated range-summary/body frames. An old peer's ordinary unmatched
response opts out. Range limits, invalid snapshots or allocation pressure never
silently downgrade to a full Have exchange. The existing compatibility requester
can compare a qualified cached fingerprint, but mismatched inventory admission is
limited to 2,000 IDs and eight frames; large unsupported peers remain explicitly
limited rather than appearing converged.

The production source index contains only qualified outgoing SQL selection IDs,
not raw repository rows or pending-only Have contributors. Exact body requests
use the same selected outgoing rows, existing bounded dependency preparation,
current access checks and bounded body loader. Each connection pins a source view;
new probes requalify pooled snapshots and qualified ordinary appends extend the
prepared prefix. Cold derived preparation, receiver indexes and checkpoints are
retained under the existing peer/scope-qualified owner across bounded turns.

Range turns support both directions through explicit role reversal. Each direction
reserves at most two summary RPCs plus its open/body exchange, 100 events and
512 KiB encoded event bodies. The entire reconciliation turn is bounded by 200
events, 1 MiB and eight RPCs; explicit custody/receipt confirmation remains its
separate existing bounded lane. Individual replies also respect the transport
frame ceiling. Up to 32 canonical leaf IDs amortize summary round trips.

`More` carries the actual local commit report, recycles a balanced session and
requeues group work fairly without transport-failure backoff. `Limited` remains
incomplete, parks for source/policy changes or sparse repair and grants neither
quiet/equality evidence nor custody. Unavailable IDs are deferred behind other
frontier work; a completed ID response is validated as an exact unique set before
commit. Local applied counts never include reverse-direction remote commits.

The existing frontend history projection already defaults to 200 records in
`frontend/src/store/conversationHistory.ts`. No seven-day retention filter or
cryptographic/control-event truncation is introduced.

Legacy budget exhaustion can interrupt an old-peer exchange; that channel is
retired and follows the existing transport retry policy. It is not recycled as a
clean `Limited` range turn. New-profile `More`/`Limited` retain the explicit fair
continuation/sparse-repair behavior described above.
