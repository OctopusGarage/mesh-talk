# Change language and startup behavior

Choose an interface language and decide how the app behaves at login or when closed.

## Change the display language

1. Open **More → Settings → General**.
2. Use **Language** to choose a supported interface language.

The interface supports English, Spanish, Portuguese, Japanese, Korean, Simplified
Chinese, Traditional Chinese and Cantonese. Language names appear in their
own language, including **Português** and **한국어**. A change takes effect
immediately and is saved on this device; it does not require restarting the app.
Conversation day separators, message times and diagnostic elapsed-time labels
follow the chosen interface language.
On first use, the app chooses the first supported system/browser language, then
falls back to English. Portuguese regional tags select the single **Português** option; Korean regional
tags select **한국어**.

The desktop app, website, recovery page and current/versioned online help use the
same fixed selector order: English, Español, Português, 日本語, 한국어, 简体中文,
繁體中文, 粵語. English comes first, followed by Latin-script languages, Japanese,
Korean and the Chinese choices.
Their preference is separate from the desktop app: a shared `lang` URL takes
precedence over a saved website choice and browser preferences. Automatic browser
detection does not save a fixed website preference; choosing a language or opening
an explicit language URL does. Native language names carry their own language tags
so assistive technology can pronounce them appropriately.

The user manual is currently authored in English. The help interface follows the display language and common Chinese search aliases are available, but articles remain English and state that fallback explicitly.

## Startup and closing

General also contains launch-at-OS-login and close-to-tray choices. Operating-system startup integration is a computer-wide choice. Closing to the tray keeps the app running; use **Quit Mesh-Talk** in the tray menu to exit.

On macOS, the native window controls differ from the Windows and Linux window chrome. Help describes named controls instead of relying on screen coordinates.

## Further reference

[Settings and ownership](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/settings.md)
