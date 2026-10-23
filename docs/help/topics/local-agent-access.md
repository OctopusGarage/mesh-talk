# Let a local agent send messages

Use your signed-in Mesh-Talk account for updates requested through an agent on
this computer. You do not need a bot account, MCP setup or a CLI download.

1. Open **More → Settings → General**.
2. Under **Local agent access**, select the contacts or joined groups the agent
   may send to. You can select up to 16 conversations.
3. Turn on **Local agent access** and check that its status is running.
4. Expand **View agent instructions**, then choose **Copy instructions** to give
   the short help URL hint to another agent.

Mesh-Talk also writes this hint to the Codex user-level `AGENTS.md` at the path
shown in settings. Repeated toggles update one section. Other agents may use a
different instructions file; paste the copied hint where that agent reads it.
An existing agent session may need to reload its instructions after an app restart.

The help URL explains the available operations and authentication. It contains no
credentials or chat content. Agents may send text and replies only to your chosen
conversations; this feature does not give access to chat history.

Keep Mesh-Talk running and unlocked while using the agent. Turning the switch off
stops new local requests and removes only Mesh-Talk's instructions section. It does
not recall messages already accepted for sending. Unavailable status or a file
update error offers a retry; a saved switch alone does not prove the service is ready.

## Further reference

[Desktop local agent access](https://github.com/OctopusGarage/mesh-talk/blob/main/docs/bot-integrations.md#desktop-local-agent-access)
