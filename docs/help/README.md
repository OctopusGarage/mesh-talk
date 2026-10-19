# Mesh-Talk user guide

Open **More → Help** inside Mesh-Talk, or **Help** on the sign-in screen. Articles
and search work offline. The desktop tray menu also has **Mesh-Talk Help…**.
The [online manual](https://octopusgarage.github.io/mesh-talk/help/) presents the
same task guides. Its deployment follows the repository's Pages workflow.

The manual is maintained in English. The existing app interface remains localized;
common Chinese search aliases help find English articles. Translated manuals are
not maintained yet. This is product help, separate from contributor instructions
and the technical [documentation map](../README.md).

| Your task                               | Guide                                                |
| --------------------------------------- | ---------------------------------------------------- |
| Get started                             | [Start using Mesh-Talk](topics/first-run.md)         |
| Find another person                     | [Find people on your network](topics/find-people.md) |
| Connect another site                    | [Connect across networks](topics/routed-networks.md) |
| Set up offline delivery                 | [Use post offices](topics/post-offices.md)           |
| Understand pending or failed messages   | [Message delivery](topics/delivery.md)               |
| Share attachments                       | [Send and receive files](topics/send-files.md)       |
| Choose where files are saved            | [Download folder](topics/download-location.md)       |
| Start a group or catch up on messages   | [Channels](topics/channels.md)                       |
| Try calling                             | [Experimental calls](topics/calls.md)                |
| Compare a contact's identity            | [Verify a contact](topics/verify-contact.md)         |
| Add another computer                    | [Link devices](topics/link-devices.md)               |
| Protect the screen or change visibility | [Privacy controls](topics/privacy.md)                |
| Change alerts and previews              | [Notifications](topics/notifications.md)             |
| Search or retain local messages         | [History](topics/history.md)                         |
| Keep a recoverable copy                 | [Backup and restore](topics/backup.md)               |
| Change theme or wallpaper               | [Appearance](topics/appearance.md)                   |
| Change language or startup behavior     | [General settings](topics/language.md)               |
| Install a newer release                 | [Updates](topics/updates.md)                         |
| Investigate connectivity                | [Diagnostics](topics/diagnostics.md)                 |

Additional tasks: [change your name and avatar](topics/profile.md),
[capture a screenshot](topics/screenshots.md), and
[install customization packs](topics/customization-packs.md).

## Ask an external AI

Give a web-capable assistant the [help index](https://octopusgarage.github.io/mesh-talk/help/llms.txt)
and your Mesh-Talk version, operating system and question. The index points to
clean Markdown articles and a machine-readable catalog. An assistant must actually
retrieve the documents; publishing an index does not guarantee automatic discovery.

For an MCP-capable client, use the local [documentation MCP setup](../../tools/help/README.md#mcp-client-configuration).
It exposes only public help. Mesh-Talk needs no model API key or running desktop
session for this integration; your AI client's own model configuration is separate.
See [help system maintenance](../help-system.md) for version and publication boundaries.
