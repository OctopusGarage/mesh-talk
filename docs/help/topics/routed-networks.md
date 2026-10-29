# Connect across networks

Use an existing routed network or a configured post-office bridge to reach another site.

## Use an existing reachable path

1. Set up the routed network outside Mesh-Talk, such as Tailscale or EasyTier.
2. Open **More → Settings → Network & delivery**.
3. Choose **Device discovery scope → Local and routed networks**. Mesh-Talk can use supported local Tailscale and EasyTier inventories when available.
4. If discovery is unavailable, choose **Add device address…** and supply the reachable remote endpoint.
5. Check **More → Connection** to distinguish discovery from an established connection.

Mesh-Talk does not install a VPN, join IP subnets, or make an unreachable address reachable. **Local network only** limits public discovery sources; it is not a connection firewall and does not revoke an already authorized contact.

## Use a post-office bridge

A configured, trusted post-office bridge can introduce opted-in participants across sites. **Use post offices**, relay send/receive permissions, and **Discover peers through a post office** are separate choices. Invisible mode suppresses public disclosure.

Trusted relays need an existing reachable path between sites; enabling discovery does not create one. Encrypted messages and eligible file transfers can use configured relay paths, while calls and full linked-device synchronization need their own live paths. See [Use post offices](post-offices.md) for participation and delivery limits.

## Further reference

[Routed discovery guide](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/routed-network-discovery.md)

[Relay operator federation setup](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/post-office-federation.md)

[Network topology and communication boundaries](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/network-design-reference.md)
