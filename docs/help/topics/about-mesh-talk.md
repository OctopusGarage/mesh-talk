# About Mesh-Talk

Mesh-Talk is an open-source encrypted desktop messenger for local and connected networks, with local accounts and no required vendor-operated messaging server.

## Do I need an account or an internet connection?

You create a local account on your computer. No email address, phone number or cloud identity provider is required. Linked devices can share an account; an account and a device are different identities.

Messaging works without internet access when the participating computers share a reachable local network. Guest Wi-Fi isolation, firewalls and separate network segments can prevent communication. Offline operation does not mean every disconnected recipient can immediately receive a message.

Read [Start using Mesh-Talk](first-run.md), [Find people on your network](find-people.md) and [Linked devices](link-devices.md).

## Can Mesh-Talk connect across networks?

Yes, with a suitable existing route or a configured post-office bridge. Routed discovery can use existing Tailscale, EasyTier or manually supplied reachable addresses. Mesh-Talk does not automatically join separate IP networks or provide a vendor-operated global directory.

Discovery, transport reachability and recipient acceptance are different conditions. Seeing someone in the roster does not prove that a message was delivered. Read [Connect across networks](routed-networks.md) and [Understand message delivery](delivery.md).

## Are servers or relays required?

Reachable devices can communicate directly. Optional peer-operated post offices can temporarily hold encrypted events while a recipient is offline. They are actual running nodes, even though no central vendor service is required.

Offline delivery depends on eligible post offices, connectivity, capacity and retained events. It is not guaranteed permanent storage. Relay custody is not recipient delivery or a read receipt. Calls and direct device synchronization have separate reachability requirements. Read [Post offices](post-offices.md).

## What does encryption protect?

Messages and files are end-to-end encrypted. Direct messages use a Double Ratchet and channels use per-sender keys. Post offices cannot read message plaintext, but may observe routing metadata. Encryption does not hide all connection information or prevent a recipient from retaining content.

Verify contacts through a separate trusted channel and choose local screen and visibility controls for your situation. Read [Verify a contact](verify-contact.md), [Privacy controls](privacy.md) and [File availability](send-files.md).

## Which platforms and features are supported?

The desktop app supports macOS, Windows and Linux. It provides direct messages, channels, file sharing, linked devices and optional offline custody. Its protocol core is Rust; the desktop app uses Tauri and React. Check published release assets for available packages and platform requirements.

[Download and release notes](https://github.com/OctopusGarage/mesh-talk/releases)

## Does Mesh-Talk include an LLM?

Mesh-Talk does not embed a language model or send account data to an AI service for its help system. External assistants can read public documentation. The optional read-only documentation MCP serves only public help from a configured local checkout; it does not provide access to private conversations.

Optional local agent access is a separate, explicitly enabled messaging capability with scoped permissions. Public documentation access does not authorize sending messages. Read [Local agent access](local-agent-access.md) and [Documentation MCP setup](https://github.com/OctopusGarage/mesh-talk/blob/main/tools/help/README.md).

## Which version do these instructions describe?

The online reader displays its documentation source version. Source documentation is not proof that a matching Stable release has been published. Check your installed version and release notes before following instructions. A clean website build publishes the current version snapshot; older installed apps retain their bundled manual.

[Install and update](updates.md)

## Further reference

[Source repository and project overview](https://github.com/OctopusGarage/mesh-talk)

[Architecture and security boundaries](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)

[Network topology contracts](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/network-design-reference.md)
