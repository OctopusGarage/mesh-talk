# Change language and startup behavior

Choose an interface language and decide how the app behaves at login or when closed.

## Change the display language

1. Open **More → Settings → General**.
2. Use **Language** to choose a supported interface language.

The user manual is currently authored in English. The help interface follows the display language and common Chinese search aliases are available, but articles remain English and state that fallback explicitly.

## Choose an incoming-message language

In **More → Settings → General → Incoming message language**, choose the main language for text sent to your account, or keep **Unrestricted**. This is separate from the interface language and does not translate messages. Detection runs locally.

Restricted messages need at least 80% content in the chosen language, subject to approximate detection and input limits. Updated senders keep blocked text in their draft; the receiving device also filters new incoming text under its known rule. Groups, attachments, stickers and calls are unaffected. Changing the preference does not reveal previously hidden messages. See the [incoming DM language guide](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/dm-language-restrictions.md) for exceptions, linked-device propagation and compatibility.

## Startup and closing

General also contains launch-at-OS-login and close-to-tray choices. Operating-system startup integration is a computer-wide choice. Closing to the tray keeps the app running; use **Quit Mesh-Talk** in the tray menu to exit.

On macOS, the native window controls differ from the Windows and Linux window chrome. Help describes named controls instead of relying on screen coordinates.

## Further reference

[Settings and ownership](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md)
