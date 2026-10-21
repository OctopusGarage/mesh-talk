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

## Inspect relationship evidence

The contact card separates signed public claims from your private observations.
A creation date is self-declared. A witness receipt is another account’s signed
statement using its own clock. First-observed time and device-change history
belong to this installation and are never included in shared card exports.
Legacy records without an observation time remain unknown. Re-verifying a device
preserves the original observation history.

Importing a signed profile or proof never marks a contact verified. See
[peer trust cards](../../peer-trust.md) before relying on introductions or age.

## Further reference

[Account and device identity](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/ARCHITECTURE.md)
