# Verify a contact identity

Compare the contact verification information through a separate trusted channel.

## Compare identities

1. Open the direct conversation with the person.
2. Open **Verify contact** from the conversation header.
3. Compare the displayed verification information with the other person using an independent trusted channel, such as in person.
4. Mark the contact verified only after the comparison succeeds.

A display name or avatar can be copied. Verification concerns the account and its device identity bindings, not merely the person's name.

## When an identity changes

Treat an unexpected identity change as a reason to verify again. A restored account can retain its account fingerprint while creating a new device fingerprint. Starting with a new identity changes the trust relationship; tell contacts through a trusted channel.

## Inspect profile and relationship history

The contact profile shows their About text and a read-only creation date when
available. That date uses the originating device's clock and does not establish
identity or trustworthiness. Legacy dates remain unknown.

First-observed time and device-change history belong to this installation and
remain private. Legacy records without an observation time remain unknown.
Re-verifying a device preserves the original observation history. Profile updates
never mark a contact verified. See [profiles and verification](../../peer-trust.md)
for the distinction between presentation and your verification decision.

## Further reference

[Account and device identity](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
