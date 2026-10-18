# Find people on your network

Check discovery, visibility and connectivity when someone is missing from the sidebar.

## Start with the local network

1. Confirm both people are signed in and running Mesh-Talk.
2. Confirm the computers share a reachable network. Guest Wi-Fi, client isolation and separate VLANs can prevent peer communication.
3. Open **More → Settings → Connection & delivery**. Check **Device discovery scope** and whether **Invisible mode** is enabled.
4. Open **More → Connection** for the connection diagnostics. Inspect local interfaces and known peers, and rescan if needed.
5. If needed, open **Add device address…** in Connection & delivery and enter the other device's reachable address.

Local discovery uses signed UDP multicast on port 47474; messaging uses an authenticated TCP connection on the app's advertised listening port. Firewalls must allow the relevant traffic. The diagnostics troubleshooting page gives platform-specific guidance; do not guess a fixed TCP port.

## Discovery is not a delivery guarantee

A known or recently observed device is not proof of a working message path. An offline contact can remain in your list. Invisible mode and reply permissions can affect public discovery and admission independently of IP reachability.

For different networks, read [Connect across networks](routed-networks.md). For a known person whose messages are pending, read [Understand delivery](delivery.md).

## Further reference

[Network troubleshooting](https://github.com/OctopusGarage/mesh-talk/blob/main/specifications/troubleshooting_guide.md)
