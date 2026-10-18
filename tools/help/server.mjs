import { McpServer } from "@modelcontextprotocol/server";
import { StdioServerTransport } from "@modelcontextprotocol/server/stdio";
import * as z from "zod/v4";
import { loadHelp } from "./load.mjs";
import { searchHelp } from "./search.mjs";
const data = await loadHelp({ root: process.env.MESH_TALK_HELP_ROOT });
const server = new McpServer({
  name: "mesh-talk-public-help",
  version: data.version,
});
const text = (value) => ({
  content: [{ type: "text", text: JSON.stringify(value) }],
});
const error = (message) => ({
  ...text({ error: message, availableVersions: [data.version] }),
  isError: true,
});
const check = (version) =>
  version && version !== data.version
    ? `Unsupported help version: ${version}`
    : null;
const annotations = {
  readOnlyHint: true,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
};
server.registerTool(
  "search_help",
  {
    description:
      "Search lightweight ranked public help results, then use get_help_topic or resourceUri for the full article. English fallback is explicit.",
    annotations,
    inputSchema: z.object({
      query: z.string().max(1000),
      version: z.string().optional(),
      platform: z.enum(["macos", "windows", "linux"]).optional(),
      locale: z.string().optional(),
    }),
  },
  async ({ query, version, platform, locale }) => {
    if (check(version)) return error(check(version));
    return text({
      version: data.version,
      language: "en",
      requestedLocale: locale ?? "en",
      localeFallback: !!locale && !/^en(?:-|$)/i.test(locale),
      topics: searchHelp(data.topics, query, { platform, limit: 20 }).map(
        ({ id, title, summary, platforms }) => ({
          id,
          title,
          summary,
          platforms,
          resourceUri: `mesh-talk-help://${data.version}/topics/${id}`,
          url: `https://octopusgarage.github.io/mesh-talk/help/v/${data.version}/${id}.html`,
        }),
      ),
    });
  },
);
server.registerTool(
  "get_help_topic",
  {
    description:
      "Read one bundled public help topic by id for the specified version.",
    annotations,
    inputSchema: z.object({
      id: z.string().max(100),
      version: z.string().optional(),
    }),
  },
  async ({ id, version }) => {
    if (check(version)) return error(check(version));
    const topic = data.topics.find((t) => t.id === id);
    return topic
      ? text({ version: data.version, language: "en", topic })
      : error("Unknown help topic: " + id);
  },
);
server.registerResource(
  "help-index",
  `mesh-talk-help://${data.version}/index`,
  { description: "Public English help catalog", mimeType: "application/json" },
  async (uri) => ({
    contents: [
      {
        uri: uri.href,
        mimeType: "application/json",
        text: JSON.stringify(data),
      },
    ],
  }),
);
for (const topic of data.topics)
  server.registerResource(
    topic.id,
    `mesh-talk-help://${data.version}/topics/${topic.id}`,
    { description: topic.summary, mimeType: "text/markdown" },
    async (uri) => ({
      contents: [
        { uri: uri.href, mimeType: "text/markdown", text: topic.body },
      ],
    }),
  );
await server.connect(new StdioServerTransport());
