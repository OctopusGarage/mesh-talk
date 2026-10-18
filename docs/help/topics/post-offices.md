# Use post offices for offline delivery

A peer-operated post office can temporarily hold encrypted events while a recipient is offline.

## Configure participation

1. Open **More → Settings → Connection & delivery**.
2. Review **Use post offices**, the master switch for relay participation.
3. Review **Send through post offices** and **Receive through post offices** independently.
4. To opt into participant introductions, review **Discover peers through a post office**. Its availability depends on the parent settings and visibility policy.
5. Choose a **Preferred post office** from known eligible direct candidates if you have a preference.

The preferred choice is saved intent, not proof the relay is currently connected or handling a message. Disabling the master switch retains dependent saved choices; the app shows why they are inactive.

## Understand the limits

Post offices are operated by peers, not a vendor account service. They hold encrypted events subject to capacity and a maximum age of 30 days. Delivery depends on eligible relays and surviving routes; it is not guaranteed permanent storage.

Turning sending off also withholds return controls such as relay receipts and file decisions. Direct peer communication remains separate. In invisible mode, offline relay delivery requires a manually allowed trusted relay account.

Relay custody is not the same as recipient confirmation. Relays may observe routing metadata even when content is encrypted. Calls and direct device synchronization still need their own reachable paths.

## Further reference

[Post-office metadata privacy](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/post-office-metadata-privacy.md)

[Network settings](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md)
