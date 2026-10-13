# Updating Mesh-Talk

The **About → Check for updates** control checks GitHub's latest published stable release only when
you ask. It does not install software, restart the app, or change your account or message store.

If a newer version is published, About shows its version and release notes. **Download update**
selects the ZIP for the running app's OS, CPU architecture, and default/lite edition, then saves it
to Downloads. The ZIP contains the platform installers and `SHA256SUMS`. The app streams the ZIP
to a temporary file, checks the published size and SHA-256 digest from the GitHub Releases API,
and only then shows **Show in Downloads**. Canceling or failing a download removes the temporary
file. A ZIP already in Downloads is reused only if its size and SHA-256 digest still match the
published asset. An unavailable package or an invalid digest is never offered as ready.

After downloading, unzip and install it yourself:

| Platform | Install |
|---|---|
| macOS | Open the DMG and drag Mesh-Talk to Applications. Quit the running app before replacing it. |
| Windows | Run the EXE installer, or use MSI for a managed installation. Close Mesh-Talk first. |
| Linux | Install the DEB or RPM for your distribution, or use the AppImage. Close Mesh-Talk first. |

The ZIP's adjacent `.bundle` signature and build attestation on the release page support a stronger
independent provenance check; see the release's **Verifying your download** section. The in-app
SHA-256 check protects the transfer using metadata returned by GitHub, but does not replace that
independent signature check. Current installers are not signed by a commercial OS code-signing
certificate, so some systems may show their usual downloaded-app warning.

The app keeps local data under `~/.mesh-talk`; replacing the application does not remove it.
Keep an encrypted backup and recovery key before any major upgrade. If GitHub is unreachable,
rate-limits checks, or does not publish an asset for the current platform, About shows a retryable
message or a link to the release page. No background network request is made for updates.
