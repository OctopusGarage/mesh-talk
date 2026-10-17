# Documentation guidelines

Mesh-Talk documentation should help a reader make a decision or complete a task.
Keep one coherent product story across surfaces, with details in the document that
owns them. A feature does not need a new paragraph everywhere it is mentioned.

## Audience and ownership

| Surface                                                      | Reader's question                                                     | Owns                                                                                                                                               |
| ------------------------------------------------------------ | --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Project site](../site/index.html)                           | Is Mesh-Talk for me?                                                  | Product model, trust boundaries in plain language, representative UI, download and documentation paths.                                            |
| [Root README](../README.md)                                  | What is it, how do I start, and where is the source?                  | Concise orientation, supported communication paths, first run, repository map.                                                                     |
| [Documentation hub](README.md)                               | Which guide answers my question?                                      | Task-based routes and a complete catalog of maintained guides and evidence.                                                                        |
| [Architecture](ARCHITECTURE.md)                              | How does the system work?                                             | Protocol, modules, data flow, crypto, wire compatibility, and technical limits.                                                                    |
| [Network architecture baseline](network-design-reference.md) | Which communication model and contracts must future changes preserve? | Network design principles, module ownership, topology scenarios, policy interactions, resource budgets and feature-specific acceptance boundaries. |
| Feature and operations guides                                | How do I configure or use this capability?                            | Exact steps, options, examples, constraints, and troubleshooting for their topic.                                                                  |
| [Evaluations](evals/)                                        | What was verified at the time?                                        | Dated test evidence and its limits; never a substitute for current behavior.                                                                       |

The site and README may summarize a capability, but link to its owning guide
for setup, flags, support matrices, and changing limits. Do not copy a CLI option
table, release procedure, or full feature inventory into either overview.

## Shared project narrative

1. **Local first:** a local identity, automatic signed LAN discovery, direct
   authenticated connections, and no vendor-operated account service.
2. **Optional reach beyond a LAN:** an existing routed path or a configured
   post-office bridge introduces participating peers. Mesh-Talk does not provide
   a VPN or merge IP subnets.
3. **Encrypted content with visible metadata:** messages are end-to-end encrypted;
   peers and relays can observe some network or routing metadata.
4. **Offline delivery is bounded:** a peer-operated post office stores encrypted
   events temporarily, subject to capacity and a 30-day maximum age.

Use this order when explaining the product to newcomers. Describe a capability
and its relevant boundary together. For example, bridge discovery is opt-in;
federated relays can replicate opaque mailboxes, while calls and direct device
synchronization still require their own reachable paths.

## Writing and maintenance rules

- Start with the reader's task. Use concrete headings and short paragraphs;
  put commands near the step that needs them.
- Name the actor and the condition: “an opted-in client registers with a pinned
  relay” is clearer than “the network becomes available.”
- Distinguish implemented behavior, required configuration, tested scenarios,
  and intended future work. Do not promote a test result into a universal claim.
- Prefer stable links to the owning guide over duplicated instructions. Use
  relative links within the repository; the static site links to GitHub's
  published guides.
- Keep screenshots representative, accessible, and labeled as demo data when
  appropriate. See the [screenshot guide](screenshots/README.md).
- Write durable copy in English. Avoid release-number-dependent claims on the
  site; the release page owns current binaries and verification commands.
- When implementation changes, update its owning guide and any overview claim
  that materially changes. Recheck links, commands, platform scope, and
  negative-space claims such as “no server.”

## Review checklist

1. Identify the owning guide and confirm each changed claim against the current
   code or a current release artifact.
2. Update the site or README only if the change affects a newcomer's decision
   or first-run path. Remove stale copy instead of appending another feature row.
3. Keep this hub complete and route the reader to the guide from the relevant
   task path.
4. Run formatting and link checks on touched documents. For site changes,
   inspect desktop and narrow layouts, keyboard focus, reduced motion, and
   image loading. Reserve full E2E and full-suite runs for PR/CI or an explicit
   validation request; see [testing feedback loop](testing-feedback-loop.md).
