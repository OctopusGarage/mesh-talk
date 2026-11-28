# Temporary LAN review

The desktop owner can preview and temporarily share one ordinary text message.
After confirming the text, the owner receives a link that a browser on the
same physical LAN can open without Mesh Talk. HTTP sends both the URL and text
in plaintext. Anyone who obtains the URL and can reach the LAN listener can
read the item while the link is active. A visitor can retain a copy.

## Authority and lifetime

The desktop uses its current unlocked session for preview, publication and
disclosure. The core resolves the exact retained message and supplies only
the eligible UTF-8 text and its expiry. Protected, recalled, unavailable,
attachment and sticker entries are ineligible. The approved bytes must match
at publication and at every read. Publication and disclosure validate the
committed storage version under the source connection guard. Notifications
wake revalidation, while the guarded version also catches commits whose
notification is still pending. The handoff performs no storage query or
socket I/O. A response already authorized may finish within its write
deadline when the share ends.

The first slice allows one active link, scoped to one physical IPv4 interface.
It admits TCP peers in that interface's subnet and validates the numeric HTTP
Host. Use this on a trusted LAN. The subnet check does not identify a visitor
or detect a gateway that rewrites the source address into the subnet.

The desktop stops admitting reads on revocation, lock, logout, source
invalidation, expiry, interface loss or runtime retirement. Links expire after
15 minutes or at the earlier source expiry. Restart requires creating a new
link. A listener failure ends the grant.

## Resource limits and verification

One active share owns one listener and one expiry deadline. The HTTP adapter
limits accepted requests to eight, reads headers into at most 8 KiB and writes
at most 16 KiB of text. Connections close after one response. Excess sockets
are closed before authorization. Request and write deadlines are two seconds
each. One synchronous history read can outlive its HTTP waiter. The participant
work permit and share slot remain occupied until the actual read completes.
Desktop session and lock guards are reacquired only to approve disclosure.
Withdrawal closes sockets without holding those guards during the read.
Fresh issuance waits for the slot to be returned. A transient busy read
denies the current HTTP request and retains the pending source recheck.
Content and link credentials are omitted
from application logs, and responses set no-store.

Keep exact-source eligibility, desktop session and runtime admission, bounded
HTTP serving, and two-device native acceptance in their respective checks.
Verify unsupported input, publication-time invalidation, withdrawal during a
read, timeout and return to idle. A transport fixture shows reachability and
literal text rendering. Native desktop issuance, production authority and
lifecycle need their own evidence.

Run the desktop selection with `cargo test -p mesh-talk --lib lan_review`.
On a machine with one eligible physical IPv4 interface, also run
`cargo test -p mesh-talk --lib lan_review -- --ignored` to exercise the
registered lifecycle races and the held interface observer.
