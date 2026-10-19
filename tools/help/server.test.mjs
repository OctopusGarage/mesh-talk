import test from "node:test";
import { fixture } from "./fixture.mjs";
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";
import { fileURLToPath } from "node:url";
test("real stdio MCP exposes only public read tools, version errors and resources", async (t) => {
  const root = await fixture(t);
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [fileURLToPath(new URL("./server.mjs", import.meta.url))],
    stderr: "pipe",
    env: { MESH_TALK_HELP_ROOT: root },
  });
  const client = new Client({ name: "help-test", version: "1.0.0" });
  try {
    await client.connect(transport);
    const tools = await client.listTools();
    assert.deepEqual(tools.tools.map((t) => t.name).sort(), [
      "get_help_topic",
      "search_help",
    ]);
    for (const t of tools.tools) assert.equal(t.annotations.readOnlyHint, true);
    const result = await client.callTool({
      name: "search_help",
      arguments: { query: "", locale: "zh-CN" },
    });
    assert.ok(!result.isError);
    const payload = JSON.parse(result.content[0].text);
    assert.equal(payload.language, "en");
    assert.ok(payload.localeFallback);
    assert.ok(payload.topics.length > 0);
    assert.equal(payload.topics[0].body, undefined);
    assert.equal(payload.topics[0].keywords, undefined);
    assert.equal(
      payload.topics[0].resourceUri,
      "mesh-talk-help://0.4.0/topics/connect",
    );
    assert.equal(
      payload.topics[0].url,
      "https://octopusgarage.github.io/mesh-talk/help/v/0.4.0/connect.html",
    );
    const source = await client.readResource({
      uri: payload.topics[0].resourceUri,
    });
    assert.match(source.contents[0].text, /^# Connect devices/);
    const id = payload.topics[0].id;
    const topic = await client.callTool({
      name: "get_help_topic",
      arguments: { id },
    });
    assert.match(JSON.parse(topic.content[0].text).topic.body, /^# /);
    const missing = await client.callTool({
      name: "get_help_topic",
      arguments: { id: "does-not-exist" },
    });
    assert.equal(missing.isError, true);
    const unsupported = await client.callTool({
      name: "search_help",
      arguments: { query: "", version: "999.0.0" },
    });
    assert.equal(unsupported.isError, true);
    const invalid = await client.callTool({
      name: "search_help",
      arguments: { query: "", platform: "android" },
    });
    assert.equal(invalid.isError, true);
    const resources = await client.listResources();
    assert.ok(resources.resources.length > 0);
    const resource = await client.readResource({
      uri: resources.resources[0].uri,
    });
    assert.ok(resource.contents[0].text);
    assert.ok((await client.listTools()).tools.length === 2);
  } finally {
    await client.close();
  }
});
