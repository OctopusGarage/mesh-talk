# Send messages from bots, scripts and local agents

Mesh-Talk provides a local integration service for scoped text/reply sending.
A dedicated headless participant can act as a bot; the desktop can delegate
sending through the signed-in user's account. Both use the existing encrypted
DM/group paths. AI execution stays in your agent application.

The service exposes private local IPC and four MCP tools. Desktop settings also
provide a loopback HTTP adapter over that same service. It does not expose chat
history, run models, or receive incoming HTTP webhooks. Interactive event
subscriptions, portable bot badges, cards, remote MCP/OAuth and incoming HTTP
webhooks remain outside the current implementation. Future adapters must reuse
the scoped integration service and core delivery journal; they must not own mesh
cryptography or introduce a second sending path.

## Build

```sh
cargo build -p mesh-talk-integrations -p mesh-talk-mcp
```

This produces `target/debug/mesh-talk-integrations` and
`target/debug/mesh-talk-mcp` (`.exe` on Windows). Production builds retain the
normal password KDFs. The reduced test KDF feature is for tests only.

## Run a dedicated bot

Use a new, dedicated, initially empty data directory. The host refuses an
unmarked directory containing existing desktop/node data and holds an OS-backed
exclusive lock for its bot profile. Keep bot accounts separate from human accounts.

The password input is a private JSON file, not a command-line password. For
macOS/Linux, create it with a terminal prompt:

```sh
python3 - <<'PY'
import getpass, json, os
path = os.path.expanduser('~/.mesh-talk-bot-password.json')
password = getpass.getpass('New bot profile password: ')
if not password:
    raise SystemExit('A nonempty password is required')
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as stream:
    json.dump({'password': password}, stream)
PY

./target/debug/mesh-talk-integrations serve \
  --data-dir "$HOME/.mesh-talk-bots" \
  --profile alerts \
  --name 'Release Watch [Bot]' \
  --password-file "$HOME/.mesh-talk-bot-password.json"
```

On Windows, private files must have the current user as owner and a protected
DACL granting that user access exclusively. Default inherited permissions are
rejected. Create a new empty file, set its owner and protected user-only ACL,
then write the JSON password through a trusted secret provisioning tool. The
same protected-file helper is used by the CLI for exported client credentials.
Windows transport code is cross-compiled; native Windows acceptance requires
its platform test gate before deployment.

`serve` prints a ready record with the bot's account fingerprint, device
fingerprint and private administrator configuration path. Save the path as
`ADMIN_CONFIG` in the examples below. Each host startup generates a fresh admin
credential/configuration. It is removed on clean shutdown. Its endpoint points
to a stable protected `integration-endpoint.json` manifest beside the bot profile;
ordinary scoped client files follow that manifest after restarts without
changing their credentials. Keep the data directory at a stable absolute path.

The default runtime socket lives in a private temporary directory. An optional
`--runtime-dir` must be private and short enough for Unix socket path limits.
`--discovery-port` must match participating peers (default 47474). Add
`--use-post-office` to permit sending/receiving through discovered eligible post
offices. This does not turn the bot into a relay. Normal route availability,
contact/privacy rules, bounded storage and group recovery still apply.

The host handles Ctrl-C and, on Unix, SIGTERM for clean shutdown. Use an OS service supervisor for restart and
startup scheduling; the service must have access to the same protected password
file and profile. A running MCP host is not a substitute for supervising an
interactive AI worker. Do not start a second active instance with the same
profile/device keys as a failover strategy.

## Add the bot to a group

1. Keep the bot and owner reachable through the configured mesh. Verify the
   fingerprint printed by the bot host.
2. The group owner invites that device using the existing group member controls.
   A bot does not automatically join arbitrary invitations.
3. Inspect pending groups through the admin configuration, then explicitly accept
   the intended full channel ID:

```sh
./target/debug/mesh-talk-integrations admin-context --config "$ADMIN_CONFIG"
./target/debug/mesh-talk-integrations accept-invite \
  --config "$ADMIN_CONFIG" --channel-id FULL_CHANNEL_ID
```

Group IDs are 64 lowercase hexadecimal characters. Account/device fingerprints
are different identifiers; an account destination uses a full 32-character
account ID. Display-name prefixes are not accepted for mutations.

The bot appears as an ordinary authenticated participant in existing clients.
`[Bot]` in the name is a presentation hint, not a verified badge. A portable,
account-signed bot classification is a separate protocol stage.

## Issue a scoped client credential

An administrator creates a grant bound to one stable client ID, exact
conversation destinations and expiry:

```sh
./target/debug/mesh-talk-integrations grant-create \
  --config "$ADMIN_CONFIG" \
  --client-id build-monitor \
  --destination "channel:FULL_CHANNEL_ID" \
  --expires-in-seconds 86400 \
  --out "$HOME/.mesh-talk-build-monitor.json"
```

`--destination` is repeatable, up to 16 destinations. Use `account:FULL_ACCOUNT_ID`
for account DMs. Add `--describe` only if the client should list the names of its
configured conversations. Sending never implies access to message history.
Basic context discloses the configured destinations even without description
permission, allowing a fixed-target agent to address its intended conversation.

The exported file contains a bearer credential. It is created exclusively with
private permissions; existing files are not overwritten. CLI output contains
grant metadata, not the credential. Do not place client/admin/password files in
source control, prompts, shared folders or diagnostic attachments. Processes
running as the same OS user are within the host's local trust boundary; the
capability system does not sandbox malicious same-user software.

At most 256 live grants are retained, additionally constrained by a 2 MiB
serialized store cap. Grants are encrypted under the profile password and bound
to the cryptographic account. Administrator credentials rotate on every host
startup. Scoped grants survive normal restarts until expiration/revocation;
recreating the same client ID preserves its retry-key namespace.

## Send, retry and inspect acceptance

```sh
./target/debug/mesh-talk-integrations send \
  --config "$HOME/.mesh-talk-build-monitor.json" \
  --destination "channel:FULL_CHANNEL_ID" \
  --text 'Build 482 failed: integration checks' \
  --idempotency-key build:482:integration-failed

./target/debug/mesh-talk-integrations status \
  --config "$HOME/.mesh-talk-build-monitor.json" \
  --idempotency-key build:482:integration-failed
```

A successful send returns `operation_id`, the stable message reference, local
`acceptance: accepted`, and `delivery: pending_or_unknown`. It returns before
network distribution completes. Acceptance is not recipient confirmation,
relay custody or proof everyone in a group received the message. The first
service release conservatively reports delivery as pending/unknown.

Reuse the exact key and payload after a timeout or lost response. Within the
seven-day retained retry window, the same request returns the original message;
changed text/destination/reply/expiry with the same key returns
`IDEMPOTENCY_CONFLICT`. Account/device copies and group events retain their
existing delivery semantics. The ledger refuses new work at capacity rather
than silently evicting unexpired retry records; it is bounded to 100,000 records
and the existing outbox's byte/record budgets. After seven days, do not blindly
retry an old key: it may create a new message.

`--reply-to FULL_EVENT_ID` accepts retained targets associated with the exact destination
within the most recent 500 records of a destination's sent, received or file
sidecar. Older references are rejected rather than scanning unlimited history. This bounded association check does not grant content access or promise that a referenced record is still displayed as readable text; retained own-sidecar slots may include recall/expiry placeholders.
`--expires-at-ms` sets the existing absolute UTC expiry. Maximum integration
text is 8 KiB UTF-8, additionally constrained by final encrypted frame limits.
Whitespace-only text is rejected. The send budget is one request/second with
burst five per grant; retrying also consumes this budget.

Unknown/unlearned account destinations fail explicitly. A destination string
cannot create a recipient key, reachable route or membership. Contact/privacy
policy, signed group acceptance, removal and dissolution remain authoritative.

## Connect an MCP host

Launch the MCP binary with a scoped client file, never the admin configuration:

```sh
./target/debug/mesh-talk-mcp --config "$HOME/.mesh-talk-build-monitor.json"
```

A typical host configuration has this shape; adapt the surrounding configuration
format to your MCP application:

```json
{
  "mcpServers": {
    "mesh-talk": {
      "command": "/absolute/path/to/mesh-talk-mcp",
      "args": ["--config", "/absolute/private/client.json"]
    }
  }
}
```

The SDK owns stdout; diagnostic errors use stderr. The adapter supports current
MCP `2026-07-28` discovery and legacy `2025-11-25` initialization through the
official Rust SDK. Actual SDK lifecycle tests verify both negotiated versions.

| Tool                           | What it permits                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------------ |
| `mesh_talk_get_context`        | Principal, grant destinations, availability and limits                               |
| `mesh_talk_list_conversations` | Only granted destination descriptions, if `--describe` was issued                    |
| `mesh_talk_send_message`       | Text/reply send to an exact granted destination with a retry key                     |
| `mesh_talk_get_send_result`    | Acceptance lookup in the same client namespace, subject to current destination scope |

There are no history, shell, filesystem, arbitrary command, membership-edit or
grant-administration tools. A model cannot expand its grant through a tool
argument. A notification script can use the CLI directly without any MCP host.
Receiving a chat message does not automatically wake an idle AI application;
interactive event processing remains a later feature stage.

## Desktop local agent access

Open **Settings → General → Local agent access**, select up to 16 existing
contacts or joined groups, then enable the switch. Access is off by default and
belongs to the signed-in account on this computer. No separate bot, CLI install,
MCP host or HTTP server process is required. Only the desktop Rust backend runs
the local adapters; it reuses the same integration service, grants and encrypted
message acceptance path.

New account scopes follow the current user's hidden-contact presentation policy;
joined groups remain available. Existing grants remain listed for explicit
revocation, including grants for subsequently hidden contacts. Hiding a contact
does not change an existing authorization.

The switch starts a numeric-loopback HTTP listener on an available port.
**View agent instructions** expands a short English bootstrap containing its
complete `http://127.0.0.1:PORT/v1/help` URL. **Copy instructions** copies exactly
that bootstrap for another agent. It includes no credentials or conversation IDs.

Enabling also upserts one marked block in the Codex user-level `AGENTS.md` under
`CODEX_HOME`, or `~/.codex/AGENTS.md` when that variable is unset. The settings
panel shows the actual path. Existing unrelated instructions and Unix file
permissions are preserved; repeated toggles do not append duplicate blocks.
Disabling removes only Mesh-Talk's block and stops local admission. Malformed or
externally duplicated markers produce an error rather than guessing which user
content to delete. Other agents may use different instruction locations; paste
the copied bootstrap into their instructions. An already-running agent may need
to reload instructions to discover a refreshed endpoint.

### API discovery and authentication

`GET /v1/help` is public local discovery. It returns a structured versioned
catalog: fixed HTTP routes, request schemas derived from the actual Rust request
types, examples, authentication instructions, response envelopes, resource limits,
errors and retry semantics. It exposes no token, account principal, chat content
or granted destinations. Agents learn the private credential-file location from
this catalog; the prompt does not need to embed it or maintain a second API manual.

The managed credential file is `agent-access.json` under the desktop's configured
data root (`~/.mesh-talk` by default). It contains an HTTP `endpoint` and scoped
`token`; read it privately and supply `Authorization: Bearer <token>` on every
operation request. Never paste, print or log the token. A normal first operation
is `POST /v1/context` with `{}` and `Content-Type: application/json`, which
identifies the current granted destinations and limits. The help catalog describes
`POST /v1/conversations`, `/v1/messages` and `/v1/send-result`; there are no HTTP
administration or arbitrary RPC routes.

The listener binds only `127.0.0.1`. Requests must use its exact numeric Host;
browser Origin and Sec-Fetch metadata are rejected, and no CORS access is granted.
This is local agent automation, not a remotely reachable API. Processes running
as the same OS user remain within the documented local trust boundary. Existing
private UDS transport on macOS/Linux and current-user named pipes on Windows
retain their original framing and CLI/MCP compatibility.

### Lifecycle and authority

Enabling desktop local-agent access or adding selected conversations requires
[fresh identity verification](privacy-controls.md#verify-sensitive-operations),
including additions to a saved disabled scope. Disabling and scope reduction do
not require a new prompt. Authentication is bound to the captured desktop
session and lock revision and checked again at the grant/configuration commit.

The saved preference, active listener and instructions state are distinct. The
settings panel reports actual readiness and errors. Permission changes and startup
reconciliation share one serialized desktop lifecycle owner; there is never a
second grant-store writer or participant using the same desktop stores. Changing
scope retires the previous listener and managed credential before replacement.
New desktop authorizations default to **One hour**. Choose **Until I turn it off**
explicitly for ongoing automation. Existing saved continuous authorizations keep
their behavior and are labeled accordingly. Temporary authorization stores an
absolute deadline: retrying service setup, editing conversations, locking and
unlocking, or restarting the app does not start a new hour. **Renew authorization**
is an explicit operation requiring fresh identity verification. Extending access
or changing it to continuous also requires verification; disabling and reducing
scope remain available without a new prompt.

The grant is checked again at actual message acceptance, so a request waiting
when authorization expires cannot commit afterward. Expiry does not recall
messages already accepted. The UI distinguishes expired authorization from a
service error. Restarting replaces credentials without extending the saved
deadline, and an expired authorization stays expired until explicitly renewed.
Across app launches, deadlines rely on the system clock. During a running
process, monotonic elapsed time prevents clock rollback from reviving authority.
CLI-issued grants retain their documented explicit expiry/restart behavior.

The desktop must be running and unlocked. Lock, logout, profile replacement and
runtime retirement reject captured requests at the actual message commit boundary.
Closing admission prevents pending requests from accepting new work, then the
runtime-owned supervisor shuts down both adapters and joins their producers.
Disabling does not recall already accepted messages. Normal restart restores the
unexpired saved scope, issues fresh managed credentials and refreshes the one instructions
block. Core privacy, membership, rate limits and retry/delivery semantics above
continue to apply. The service never unlocks the app or opens a second node.

The advanced `MESH_TALK_INTEGRATIONS=1` startup opt-in remains compatible until an
explicit settings preference is saved; it uses this same lifecycle owner. The
private administrator configuration and CLI grant commands above remain available
for manually managed clients. Settings takes precedence after an explicit choice.

## Revoke, inspect and stop

Desktop General settings also shows recent activity for the **Local agent
interface**. It includes recent accepted and retried sends, with time and target
conversation, without message bodies, credentials or raw retry keys. Acceptance
does not prove recipient delivery. The view contains at most 20 sends from the existing seven-day audit window
and may be incomplete;
an unavailable activity read is shown separately from an empty record.

The desktop authorization is shared by local clients holding its credential.
Activity identifies the local agent interface, not a verified AI application or
vendor. It does not expose chat history or add an activity-reading tool to the
producer API.

```sh
./target/debug/mesh-talk-integrations grant-list \
  --config "$ADMIN_CONFIG" --offset 0 --limit 20
./target/debug/mesh-talk-integrations audit-list \
  --config "$ADMIN_CONFIG" --offset 0 --limit 20
./target/debug/mesh-talk-integrations grant-revoke \
  --config "$ADMIN_CONFIG" --grant-id GRANT_ID
```

Administrative lists are paginated, at most 20 items per request; use
`next_offset` for the next page. Audit queries show recent grant changes and accepted/retried sends without text,
credentials or raw context, bounded to 256 records and the previous seven days.
Older metadata is compacted when the permission snapshot is next written. If an audit write fails after durable acceptance, the
accepted message is still accepted; the core retry mapping remains authoritative.
Future operations fail closed if the grant store is no longer trustworthy.

Revocation stops new client acceptance; it does not recall already accepted
messages. Removing a bot from the group also requires the owner-signed removal
of its enrolled devices, with normal epoch rotation. Token revocation cannot
neutralize a stolen bot signing key or erase plaintext already copied by a
member. A send-only client cannot read group history, but its trusted bot
hosting gateway is an ordinary member with decryption capability.

The service creates no remotely reachable network API. Unix sockets are restricted to their
owner; Windows pipes use current-user ACLs and reject remote clients. Requests
are bounded to 64 KiB, 16 concurrent IPC connections and a 30-second I/O budget.
Accepted messages survive a client timeout/cancellation through the encrypted
core journal. Clean host shutdown stops admission, cancels/joins request and
flush producers, and retires the participant before releasing profile ownership.

## Validation and remaining limits

Use focused tests for these module boundaries:

```sh
cargo test -p mesh-talk-integrations -p mesh-talk-mcp
cargo test -p mesh-talk-core --lib outgoing_ --features fast-test-kdf
cargo test -p mesh-talk --lib integrations_
```

The implementation includes persistent/concurrent retry, conflict, crash
recovery, bounded reply lookup, scope/revocation, private IPC, modern/legacy MCP,
profile retirement and loopback group/file checks. Cross-compiling Windows code
does not establish native Windows runtime behavior. This feature's focused
checks do not substitute for the required full workspace/preflight/health gates
before a PR or release. The [testing workflow](testing-feedback-loop.md) defines
those gates.
