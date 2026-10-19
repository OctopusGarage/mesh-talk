# Domain language and DDD conventions

Use [CONTEXT.md](../CONTEXT.md) as the single domain glossary. This guide owns DDD
vocabulary, identifier translations and modeling conventions; it does not replace
protocol specifications or require a directory/class for every DDD pattern.

## Reference index

| Question                                               | Authoritative reference                                                 |
| ------------------------------------------------------ | ----------------------------------------------------------------------- |
| What does a product/domain term mean?                  | [Canonical glossary](../CONTEXT.md)                                     |
| How do we model and name responsibilities?             | This guide                                                              |
| Which implementation owns a decision?                  | [Module ownership](module-boundaries.md)                                |
| What are the protocol and security invariants?         | [Architecture](ARCHITECTURE.md)                                         |
| What are the network, presence and delivery contracts? | [Network reference](network-design-reference.md)                        |
| Which syntax/style applies to a language?              | [Development conventions](../specifications/development_conventions.md) |
| Which checks apply to a change?                        | [Testing feedback loop](testing-feedback-loop.md)                       |
| Where should documentation live?                       | [Documentation guidelines](DOCUMENTATION_GUIDELINES.md)                 |

Definitions live once in the glossary; implementation details live with their
owning reference. Link to a definition rather than creating a second glossary.
For ambiguous existing symbols, use the translation table below.

## Modeling vocabulary

| Term                      | Repository convention                                                                                                                                                                                                 |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Ubiquitous language       | The shared domain terms in `CONTEXT.md`, used in code, tests, docs and review discussions. Existing wire identifiers may retain historical spellings.                                                                 |
| Domain                    | The problem of authenticated private conversations, authorized history and optional encrypted custody. A cryptographic domain separator is a different use of “domain.”                                               |
| Bounded context           | A scope in which a model and its terms have one consistent meaning. A crate, directory, deployment process or React feature is not automatically a bounded context.                                                   |
| Entity                    | A domain concept tracked by identity through change, such as an account, device or channel. A struct alone does not establish entity identity.                                                                        |
| Value object              | A value defined by its contents and validated rules, such as `EventId` or `ConversationId`. A string containing an identifier is a representation, not proof it has been validated.                                   |
| Aggregate                 | A consistency boundary whose owner enforces named invariants for a state transition. Do not infer an aggregate from a large struct, whole node, store or event log.                                                   |
| Aggregate root            | The entry point responsible for those aggregate invariants. Declare the invariant and actual commit/locking boundary before applying this label.                                                                      |
| Domain service            | Domain behavior that does not fit one entity/value object. Keep the authoritative policy with the core owner; an arbitrary class named `Service` is not automatically a domain service.                               |
| Application workflow      | Coordination of admission, domain operations, persistence and effects for a use case. It delegates domain rules and does not create a second policy implementation.                                                   |
| Domain event              | A fact recognized by a domain model. Mesh-Talk protocol `Event` records are a specific signed representation; UI/native notifications and callbacks are not automatically persisted domain events.                    |
| Projection                | A derived view for a reader, such as the roster, person presence or conversation history. A projection does not become membership or delivery authority.                                                              |
| Repository interface      | A persistence seam serving a real model need. This is distinct from the Git repository; do not add generic CRUD interfaces or wrappers without an actual use case.                                                    |
| Adapter                   | An implementation translating an interface at a seam, such as native IPC or MCP. Translation must preserve identity, authority, ordering and error meaning.                                                           |
| Module / interface / seam | A module hides implementation behind a caller contract; its interface includes errors, ordering and validity requirements. A seam is where implementations can vary; it is not synonymous with a DDD bounded context. |

These are modeling conventions, not a claim that all current modules implement
DDD patterns. The repository currently shares one central glossary. Keep local
access/session management, mesh messaging and integration grants explicit as
responsibility areas, using the module ownership map. Introduce separate bounded
contexts and a context map only when distinct models, translations and ownership
are demonstrated; do not relabel architectural layers as contexts.

## Existing symbols and overloaded names

Paths below are repository-relative. The table describes current representations;
it does not authorize interchangeable identifiers or silent wire migrations.

| Existing representation                                                                                        | Canonical meaning and usage                                                                                                                                                |
| -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`User.user_id`](../crates/mesh-talk-core/src/identity/user.rs)                                                | Local user ID: a UUID for local sign-in data. In new local variable names prefer `local_user_id`.                                                                          |
| [`PublicIdentity::user_id()` and `DeviceIdentity::user_id()`](../crates/mesh-talk-core/src/identity/device.rs) | Device ID: a self-certifying device-key fingerprint. In new local variable names prefer `device_id`; retain existing public/wire spelling where compatibility requires it. |
| [`account_id` in account identity/certificates](../crates/mesh-talk-core/src/identity/account.rs)              | Account ID: the cross-device cryptographic identity. It is neither the local UUID nor the device fingerprint.                                                              |
| [`EventId`](../crates/mesh-talk-core/src/eventlog/event.rs)                                                    | Immutable protocol event identity. A frontend optimistic `clientId` is a local outgoing-intent correlation key, not an Event ID.                                           |
| [`ConversationId`](../crates/mesh-talk-core/src/eventlog/event.rs)                                             | Opaque protocol conversation identifier. It is not interchangeable with a frontend selection ID.                                                                           |
| [`Conversation` UI model](../frontend/src/store/conversationModel.ts)                                          | UI selection with `kind: "account" \| "channel"` and a kind-dependent ID. Translate through the existing owner; do not treat its `id` as a raw protocol `ConversationId`.  |
| [`User.display_name`](../crates/mesh-talk-core/src/identity/user.rs)                                           | Editable display label; `username` remains the local login key. Neither grants authority.                                                                                  |
| [`SessionLease`](../src-tauri/src/desktop_session.rs)                                                          | Exact desktop-session owner and generation. Matching an owner string after re-login does not restore an old lease.                                                         |
| [`transport::Session`](../crates/mesh-talk-core/src/transport/session.rs)                                      | Noise transport session for a peer connection. Qualify it as a transport session in prose and imports where ambiguous.                                                     |
| Ratchet sessions                                                                                               | Cryptographic messaging state; separate from transport connections and local login sessions.                                                                               |
| `owner`                                                                                                        | Qualify the role: local user/session owner, channel owner, implementation owner or resource owner. There is no universal `owner` identifier type.                          |
| Channel / group                                                                                                | Prefer `channel` in implementation names; `group` is valid product language. Qualify Noise channels and multicast groups when discussing networking.                       |
| `Node` / participant runtime                                                                                   | A node coordinates core behavior; its runtime owns active producers. Neither is the device's durable identity or a physical machine.                                       |

The links point to the current declarations. The mappings explain meanings;
they do not make look-alike string representations safely interchangeable.

## Naming and translation rules

- Use a canonical term for each new domain concept. Qualify overloaded terms:
  `local_user_id`, `device_id`, `account_id`, `desktop_session`, `transport_session`
  in Rust, with equivalent camelCase names in TypeScript.
- Keep Rust snake_case/PascalCase and TypeScript camelCase/PascalCase conventions.
  Canonical domain vocabulary does not require identical syntax across languages.
- Avoid new generic `userId`, `owner`, `session`, `context` or `status` names when
  several meanings meet. Name the identity kind, lifetime or state being observed.
- At IPC, SDK and persistence seams, validate and translate identifier kinds;
  renaming or casting a string is not validation. Reuse existing admitted paths
  and typed projections instead of reconstructing policy from names or IDs.
- Treat public Rust exports, IPC fields, persisted records, enum ordering and
  signed/hash inputs as compatibility contracts. Do not mass-rename them for
  vocabulary alone. An actual format change needs versioning and migration tests.
- Name commands by requested action and facts by observed outcome. A send request,
  accepted event, relay-held copy, recipient confirmation and local read action
  must retain different names and evidence.
- Use `projection` for derived data and `policy` for authoritative decisions.
  A UI component renders/interacts; an application workflow coordinates; a core
  owner enforces domain invariants. Do not force every module into a DDD class.

## Examples that must remain unambiguous

- Two logins by the same local user have different desktop-session lifetimes.
  An old continuation cannot regain authority just because the username matches.
- Two devices on one account are distinct delivery targets and channel members.
  One account row or “person” count cannot erase device-scoped authority.
- A post office discovered on the same machine is infrastructure, not another
  human contact or an automatically admitted channel member.
- A visible file card establishes metadata presence, not byte availability.
  A relay-held message establishes custody, not recipient delivery or reading.
- Restoring an archive preserves account identity and creates a new device.
  It neither clones the former device nor revokes its remaining authority.

## Maintaining the shared language

Before introducing or changing a term, locate its glossary definition, owner and
actual code representation. Resolve disagreement against implemented behavior;
label proposed behavior explicitly rather than redefining an existing guarantee.
Update the glossary, this guide's mappings if needed, and affected owning docs in
the same change. Add a new glossary term only when it expresses a distinct domain
concept; do not collect general programming vocabulary there.

In review, check identifier kind, authority, lifetime, outcome and compatibility.
Use focused tests when behavior changes. For terminology-only edits, check links,
formatting and affected source comments; do not run exhaustive suites merely for
wording. Record consequential trade-offs in a maintained design/decision document
only when they justify one; do not generate dated audit or implementation reports.
