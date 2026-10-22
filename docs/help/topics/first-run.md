# Start using Mesh-Talk

Create a local identity, find someone nearby, and send your first message.

## Create your identity

1. Install Mesh-Talk from the project's GitHub Releases page on each computer.
2. Open the app and choose **Create identity**. Enter a local username and password, review **Stay signed in**, then choose **Create account and enter**.
3. Mesh-Talk signs you in after creating the account; you do not need to enter your password again. Your credentials unlock this installation; there is no vendor account or password-reset service.
4. Ask the other person to open Mesh-Talk on the same reachable local network. Discovered people appear in the sidebar.
5. Select a person, write a message, and send it.

## Find the app's tools

The **More** button beside your account and connection status opens **Link a device**, **Connection**, **Settings**, **Help**, and **About**. On desktop systems, right-click the tray icon to open its menu. If the icon is hidden, use the app's More menu.

Settings has seven categories and saves ordinary changes automatically after the backend acknowledges them. Read any confirmation before changing a privacy preset or shortening history retention.

## Keep access to your identity

Create an [encrypted backup](backup.md) before relying on the app for important history. To use one identity on another computer, [link your devices](link-devices.md); creating a second identity does not link it to the first.

## If nobody appears

See [Find people on your network](find-people.md). Mesh-Talk needs a reachable network path; it does not provide a VPN.

## Further reference

[Installation and product overview](https://github.com/OctopusGarage/mesh-talk/blob/main/README.md)

## Keep access to your account

An empty password field says **Enter your account password**; dots appear only
when a password is present. Use **Show password** to check your entry. Password
requirements remain visible below the creation field.

**Stay signed in** is available when creating an account and when signing in.
It saves the login credential in the operating system credential store (macOS
Keychain on a Mac). It does not create a passkey or promise iCloud/Passwords app
sync. Closing and reopening the app can resume a saved login; explicit sign-out
removes it. Locking the running app is separate: on macOS, use Touch ID or the
Mac login password to unlock it.

After creating an account, the account-protection notice reports whether login
credentials were saved. Saving must succeed in both the credential store and
local resume metadata before the app reports success. A save failure does not
block entry: keep your account password safe and choose **Retry saving login**
once the node is ready. Retry uses the existing unlocked native session and
never sends its password back to the interface. It cannot save for a replaced
session or while the app is locked. A disabled stay-signed-in preference stays
disabled.

If creation succeeds but sign-in fails, the form explains that the account
already exists and lets you retry sign-in with the entry you just supplied.
It does not create the account again. Passwords are not retained in browser
storage, and switching between sign-in and creation clears the field.

Saved login is a convenience on this device, not a recovery method. The
optional **Create encrypted backup** action opens the existing
[backup flow](backup.md); keep the recovery key separately. **I’ll do this later**
dismisses this session's reminder. Backup remains available under
**Settings → History & backup**.
