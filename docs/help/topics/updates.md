# Check for and install updates

Check GitHub Releases on demand, then install the downloaded package yourself.

## Check and download

1. Open **More → About**.
2. Choose an **Update channel**: **Stable** is the default; **Nightly** lets you try recent changes that may contain bugs. The choice is saved on this device.
3. Choose **Check for updates**. Selecting a channel alone does not check or download anything.
4. If a compatible newer package is offered, review its release notes and choose **Download update**.
5. Wait for verification, then choose **Show in Downloads**.

The app checks the downloaded archive against published size and SHA-256 metadata. This transfer check does not replace independent release provenance verification.

Switching back to Stable changes future checks; it does not automatically downgrade a Nightly installation. **Download stable release** opens the official Stable download if you want to return manually. Keep a backup before doing so. Stable and Nightly use the same local data directory.

## Install manually

Unzip the download, quit the running app, and install the package for your platform: DMG on macOS, EXE or MSI on Windows, or the supported Linux package. Updating does not install automatically or reset the local account store.

Keep an [encrypted backup](backup.md) and recovery key before a major upgrade. If GitHub is unreachable or has no matching package, use the retry guidance or release-page link shown in About.

## Further reference

[Install and update guide](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/updates.md)
