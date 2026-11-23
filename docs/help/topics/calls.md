# Enable experimental voice and video calls

Experimental calling is off by default. Enable it in settings to use direct voice and video calls or group voice meetings.

## Enable calling

1. Open **More → Settings → Notifications & calls**.
2. Enable the experimental calling option.
3. Open a direct conversation and use its call control.
4. Grant microphone or camera access when the operating system asks.

## Group voice meetings

Open an existing group with reachable devices on the same LAN. Start a voice
meeting from the group, or choose to join an available meeting. Each participant
needs experimental calling enabled and must choose to join before the app
requests microphone access. Invitations go to up to 64 known reachable group
devices when the meeting starts and expire after two minutes.

Group voice currently supports up to four participating devices. Each device
sends its microphone audio directly to the other participants and plays their
audio locally. Group messages and post offices carry no meeting audio.
Existing group membership is required. A working group chat can coexist with
a blocked direct audio connection, for example on a Wi-Fi network that isolates
clients.

Leaving releases your microphone. If the initiator leaves, existing
connections can continue and new joins pause.

## Check microphone and camera

With calling enabled, stay in **Settings → Notifications & calls** and use
**Microphone & camera**. Choose **Test microphone** or **Test camera** separately.
Opening Settings does not request device access. Microphone checking shows the
input level without playing your voice through the speakers. Camera checking
shows a muted preview; a camera failure does not prevent a microphone check.

Use **Stop check** when finished. Leaving the category, closing Settings, locking,
signing out, disabling calling or starting a call releases the check's devices.
Checks are unavailable during an active or incoming/outgoing call.

**Advanced diagnostics** is collapsed by default. With a device check running,
**Check local media connection** verifies advancing received audio samples or
decoded video frames through a same-app WebRTC connection. A track event alone
is not a passing result. This checks this computer's local media reception; it
does not establish another device's reachability, speaker output or call quality.
Permission, device availability and timeout failures include retry guidance.

## Availability and retry

If every known device is reachable only through a post office, clicking a call
control explains that no direct connection is available before requesting camera
or microphone access. **Check connection** opens the connection troubleshooting
view. A routed VPN connection can still be attempted; being on different LANs
does not by itself make a call unavailable. Older hosts with unknown route
information also remain attemptable. A known route is not a guarantee that media
will connect.

Capture, connection and unanswered/connection-timeout failures remain visible
until dismissed. **Retry** is offered for an outgoing call in the same signed-in
session and uses the latest discovered devices and routes. Technical details
are available separately from the explanation. Incoming-call failures can open
connection troubleshooting without creating an outgoing call automatically.

## Media playback and cleanup

Voice calls play the remote audio even when neither side has a camera. Video calls
fall back to audio capture if the camera cannot be opened; a remote stream without
video still has an unmuted player. Local video previews stay muted to avoid echo.
Replacing a stream or closing its view pauses playback and detaches the old stream.
Hangup and media connection failure stop local capture and close the peer
connection. A failed media connection shows the call failure notice.

## Share a window

During an established one-to-one video call, choose **Share window** when the app offers it. The control requires a client with window capture and an outgoing video track negotiated for the call. Select one application window in the system picker. The shared window replaces your outgoing camera picture. Your microphone setting still applies, and sharing does not capture system audio.

**Sharing a window** appears while your window is being sent. Choose **Stop sharing**, close the shared window, or use the system stop control to end the share. The call restores the camera to its previous state. Hanging up, losing the call, or locking the app ends the capture. If the picker returns a display or browser tab, choose a window instead.

## Connection limits

Calling is experimental. A working message or post-office route does not guarantee a usable media path. Post offices are not a general media relay, and a cross-site introduction does not make the peers directly reachable.

Across LANs, both devices need a live direct signaling route and independently
reachable WebRTC host candidates, such as through a permitted routed VPN.
Calling currently configures no STUN/TURN servers and has no mailbox signaling
fallback, so ordinary NAT-separated networks are not guaranteed to work.

If a call fails, use the microphone/camera check and **More → Connection**. See [Connect across networks](routed-networks.md) for topology limits.

## Further reference

[Settings and calls](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md)

[Network model](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/network-design-reference.md)
