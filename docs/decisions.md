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

## Share the Mesh Foundation libraries through a pinned source snapshot

Keep the reusable Rust libraries together in Mesh Foundation. Mesh Talk
consumes a pinned source snapshot so its builds can run without credentials
for the private SDK repository. Update the snapshot from a verified SDK
revision and keep its provenance alongside the source. Keep Mesh Talk's
product authorization and lifecycle in its adapters. Replication defines its
own transport interface so a consumer can compose it with Mesh Connect or
another suitable channel. See the [architecture reference](ARCHITECTURE.md#mesh-foundation-libraries).

## Use introduction distance as a relay ranking hint

A targeted exchange uses the known correspondent's fresh directory distance
alongside authenticated timing and retry state. A first timeout supplies routing
evidence even before a successful sample. Origin distance estimates forwarding
work, while timing observes the local leg and control exchange. Retain the
configured healthy preference and rotate fallback relays because either direction
can have durable copies on other paths. See the [relay ordering contract](network-design-reference.md#c-relay-preference-failover-and-partitions).
