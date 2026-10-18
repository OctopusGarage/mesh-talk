# Bot integrations: Feishu/Lark and MCP source research

Retrieved: 2026-10-19. Status: research evidence for a proposed feature, not a
statement of implemented Mesh-Talk behavior. The companion architecture proposal
owns design decisions; this note owns external facts and their limitations.

## Findings that affect architecture

Feishu exposes two distinct integration experiences: a group-bound incoming
webhook for notifications and an application bot for richer interactions. MCP
can expose local messaging through a standard tool interface. Neither example
eliminates the need for Mesh-Talk's own sender identity, conversation permissions,
durable submission semantics, and lifecycle ownership.

The current MCP revision is **2026-07-28**, according to the official
[versioning page](https://modelcontextprotocol.io/specification/versioning).
Older examples using `initialize` and HTTP session identifiers describe earlier
revisions. Implementations must choose their supported versions explicitly.

## Feishu and Lark

### Custom incoming webhook bot

The official [custom bot guide](https://open.feishu.cn/document/client-docs/bot-v3/add-custom-bot)
describes a bot restricted to its current group; the same bot cannot be installed
in another group. Adding one needs no tenant administrator approval. A caller
POSTs to its secret webhook URL to push notifications. The guide warns against
publishing that URL. These bots have no data access rights and cannot respond to
user messages or mentions. Security options are keywords, IP allowlisting, and
signatures. The documented limits are 100 requests/minute and 5 requests/second
per tenant per bot, with a 20 KB request body. Text, rich text, and cards are among
the supported message forms. The bot cannot withdraw its own messages; group
administrators can. These are Feishu's documented limits, not proposed Mesh-Talk
limits.

**Design inference:** a notification integration should be easy to install with
one destination and send-only authority. Its webhook is a credential rather than
a substitute for a stable sender identity. Keyword matching is content filtering,
not strong caller authentication.

### Application bot and messaging API

The official [Node SDK](https://github.com/larksuite/node-sdk) shows an application
client configured with an app ID and app secret, token management, and message
creation addressed by `chat_id`. It also shows registering
`im.message.receive_v1` and sending a response to the incoming chat. Feishu and
Lark are separate domain configurations in the SDK; a deployment must select the
appropriate region.

The official, explicitly historic [send-post API](https://open.larksuite.com/document/ukTMukTMukTM/uMDMxEjLzATMx4yMwETM?lang=en-US)
requires bot capability, a tenant access token, user visibility for direct
messages, and group membership for group messages. It establishes the older
membership model but should not be used as the current implementation API.

Feishu's official [AI messaging assistant article](https://www.feishu.cn/content/article/7602952057348902079)
currently describes enabling application bot capability, granting
`im:message:send_as_bot`, publishing the application, and configuring its available
user range. The article's additional bulk-send and chat-read scopes are specific
to that assistant; they are not evidence that every send-only bot needs them.

Current primary API references are [send message](https://open.feishu.cn/document/server-docs/im-v1/message/create)
and [receive message](https://open.feishu.cn/document/server-docs/im-v1/message/events/receive).
These pages returned JavaScript shells without readable bodies during this
investigation. Accordingly, this note does **not** certify current app endpoint
quotas, the precise outbound UUID deduplication window, or the complete current
permission table. Recheck those references in the platform console before
implementing a Feishu adapter.

**Design inference:** model installation, send permission, read permission, and
membership separately. Supporting alerts must not implicitly grant chat history
access or the power to impersonate the installing person.

### Events and long connections

The official [SDK long-connection documentation](https://github.com/larksuite/node-sdk#long-connection)
describes an SDK-managed outbound WebSocket connection to the platform, so event
handling does not require a public inbound server. The documented processing
deadline is three seconds, after which timeout redelivery occurs. Multiple clients
for the same application receive events in a cluster arrangement: one random
client gets an event, rather than all clients receiving it. The README says this
mode supports event subscriptions but not callback subscriptions; verify callback
support separately for the chosen SDK and current platform release.

**Design inference:** acknowledge after durable intake and perform expensive AI
work asynchronously. Application work distribution and bot execution ownership
must be explicit. A WebSocket connection by itself provides neither broadcast
semantics nor exactly-once processing.

### Official agent connector implementation evidence

Lark publishes an [OpenClaw connector](https://github.com/larksuite/openclaw-lark).
Its code provides useful evidence of operational concerns, not a general platform
contract:

- Its [deduplication module](https://github.com/larksuite/openclaw-lark/blob/main/src/messaging/inbound/dedup.ts)
  explicitly handles messages redelivered after WebSocket reconnect by tracking
  recently seen message IDs.
- Its [parser](https://github.com/larksuite/openclaw-lark/blob/main/src/messaging/inbound/parse.ts)
  recognizes bot senders separately from human senders and retains chat, message,
  parent, root, and thread identifiers.
- Its [cross-bot loop guard](https://github.com/larksuite/openclaw-lark/blob/main/src/messaging/inbound/bot-loop-guard.ts)
  limits consecutive bot turns per chat/thread and resets on human intervention.
  It documents that self-echo suppression alone does not stop two bots replying
  indefinitely to each other. Its counters are process-local and best-effort.

**Design inference:** provenance, explicit trigger policies, bounded automation
chains, and receiver deduplication belong in the integration design from the
beginning. The connector's numerical loop cap is an implementation choice to
study, not a value Mesh-Talk should copy automatically.

## MCP

### Version baseline and compatibility

The official [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
removes the `initialize`/`notifications/initialized` handshake and HTTP protocol
sessions. Requests carry their protocol version and client capabilities in
`_meta`. `server/discover` advertises supported versions and capabilities.
Applications can support both current and older versions, but must implement the
appropriate wire behavior for each.

The current [stdio specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)
continues to support a client-launched subprocess using newline-delimited
JSON-RPC over stdin/stdout. Stdout must contain only MCP messages; diagnostics go
to stderr. Modern requests carry inline metadata and use discovery rather than
initialization. The specification describes fallback to legacy initialization
when probing an older server. It also allows custom reliable byte-stream
transports to reuse the framing, while requiring transport-specific lifecycle
handling.

**Design inference:** a standard stdio adapter connected to a private local
runtime is compatible with the current specification. A Unix socket or Windows
named pipe between that adapter and Mesh-Talk is an internal API; external MCP
clients need only support ordinary stdio.

### Streamable HTTP

The current [Streamable HTTP specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
uses one POST endpoint and JSON or request-scoped SSE responses. It removes the
standalone GET stream and HTTP session IDs; subscriptions use
`subscriptions/listen`. It requires validation of present Origin headers, rejects
invalid origins, recommends localhost binding for local servers, and recommends
authentication. Current HTTP requests also require protocol/method/name metadata
headers, with validation against the body. Legacy HTTP+SSE is deprecated.

**Design inference:** adding HTTP later is feasible, but it adds authentication,
origin/host validation, network exposure, and compatibility work. It should be an
adapter over the same application service, not another node or messaging stack.
A localhost listener alone is not an authorization boundary.

### Authorization and local credentials

The current [authorization specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)
makes protocol authorization optional; when implemented, HTTP integrations should
follow the OAuth-based specification, while stdio integrations should obtain
credentials from their environment instead. Protected MCP servers act as OAuth
resource servers. Access tokens must target the server, are carried in
Authorization headers on every HTTP request, and must not appear in URI query
strings. Protected Resource Metadata supports authorization-server discovery.

The separate [authorization security requirements](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/security-considerations)
require intended-audience validation and prohibit token passthrough. Upstream API
access uses a separate credential. They require secure token storage and PKCE for
OAuth clients.

**Design inference:** the stdio bridge can use an installation-specific local
capability credential, bound to a bot and allowed conversations. It must not
expose a person's Mesh-Talk private keys to the agent. Such a credential scheme
is a Mesh-Talk design choice, not a claim that MCP defines bot permissions.

### Tool contracts and user control

The current [tools specification](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
defines tool listing/calling, JSON Schema input, optional output schemas, and
structured results. If an output schema is declared, results must conform to it.
Tool behavior annotations are untrusted unless provided by a trusted server.
The interaction guidance recommends human control and the ability to deny tool
invocations; it does not mandate one particular UI pattern.

**Design inference:** expose bounded send/status tools, return stable operation
and event identifiers, and enforce authorization on the runtime. An annotation
claiming a tool is idempotent does not implement deduplication. Application
confirmation policy and installation grants remain explicit product decisions.

### Rust SDK feasibility

The official [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) exposes
`rmcp`, async server/client support, stdio, and Streamable HTTP. Its README
explicitly supports the stable 2026-07-28 specification and older revisions.
The [rmcp 3.0.0 release](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.0.0)
shipped 2026-07-28 with modern discovery, stateless HTTP, subscriptions, and
multi-round-trip result types. Existing `serve()` initialization remains a legacy
path; `serve_with_lifecycle` with Discover or Auto selects modern behavior. The
release documents Rust 1.88 as its minimum supported version.

**Implementation gate:** pin an inspected published release, check Mesh-Talk's
Rust toolchain and dependency policies, and test actual target clients across the
chosen protocol versions. Upstream support evidence is not evidence that an
adapter has already been compiled or tested in this repository.

## Evidence limits and follow-up checks

1. No live Feishu tenant, bot, group, webhook, or agent integration was created or
   exercised. No messages were sent.
2. Feishu and Lark share an official SDK with regional configuration, but tenant
   policies, product editions, group types, and regional limits may differ.
3. Current send/receive API detail requires a browser/console follow-up because
   those primary pages were unreadable through the research fetch. Do not promote
   values from unofficial mirrors into a compatibility contract.
4. SDK source links on `main` can change. Before implementation, pin the exact SDK
   release/commit used for compatibility testing.
5. Neither MCP nor the reviewed Feishu sources define Mesh-Talk's encryption,
   distributed authorization, revocation ordering, crash recovery, or delivery
   receipts. Those require explicit local protocol and storage design.
