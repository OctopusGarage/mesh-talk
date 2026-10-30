# Use post offices for offline delivery

A peer-operated post office can temporarily hold encrypted events while a recipient is offline.

## Configure participation

1. Open **More → Settings → Network & delivery**.
2. Review **Use post offices**, the master switch for relay participation.
3. Review **Send through post offices** and **Receive through post offices** independently.
4. To opt into participant introductions, review **Discover peers through a post office**. Its availability depends on the parent settings and visibility policy.
5. Choose a **Preferred post office** from known eligible direct candidates if you have a preference.

The preferred choice is saved intent, not proof the relay is currently connected or handling a message. Disabling the master switch retains dependent saved choices; the app shows why they are inactive.

## Choose whether to share your registration office

**Share my originating post office**, under **Network & delivery**, is off by
default. Enable it only if you want people discovering this device to see the
name and identity of its registration office, including through forwarding
offices. This choice belongs to the person sharing the information; viewing
someone's profile cannot enable it for them.

Office names may reveal a network or site. Participating post offices can read
this optional metadata through the existing encrypted connections. Turning the
setting off stops new sharing; already forwarded information expires within
its original 15-second lease. It cannot erase information someone already read.
Ordinary discovery and messaging continue. This control does not hide connection
IP addresses or the office introducing a participant directly to you.

A member profile separates **Registration office** from **Discovery source**.
No shared registration information can mean sharing is off, evidence has expired
or a node does not support it. Neither field establishes physical location.

## Understand the limits

Post offices are operated by peers, not a vendor account service. They hold encrypted events subject to capacity and a maximum age of 30 days. Delivery depends on eligible relays and surviving routes; it is not guaranteed permanent storage.

Turning sending off also withholds return controls such as relay receipts and file decisions. Direct peer communication remains separate. In invisible mode, offline relay delivery requires a manually allowed trusted relay account.

A post office does not become a channel member. Authorized members can also recover retained group history from one another when **Share a copy** is enabled; see [Channels](channels.md). A retained encrypted event or file card is not proof that all attachment bytes are available.

Relay custody is not the same as recipient confirmation. Relays may observe routing metadata even when content is encrypted. Calls and direct device synchronization still need their own reachable paths.

## Further reference

[Post-office metadata privacy](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/post-office-metadata-privacy.md)

[Network settings](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md)
