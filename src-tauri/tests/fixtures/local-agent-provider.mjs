// Deterministic HTTP driver for the shared provider harness; this is not an AI eval.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import http from "node:http";

async function request(url, method = "GET", body, token) {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const headers = token ? { Authorization: `Bearer ${token}` } : {};
    if (payload !== undefined) headers["Content-Type"] = "application/json";
    const req = http.request(url, { method, headers, agent: false }, (res) => {
      let text = "";
      res.setEncoding("utf8");
      res.on("data", (chunk) => {
        text += chunk;
      });
      res.on("error", reject);
      res.on("end", () => {
        try {
          assert.equal(res.statusCode, 200);
          const envelope = JSON.parse(text);
          assert.ok(!envelope.error);
          resolve(envelope.result);
        } catch (error) {
          reject(error);
        }
      });
    });
    req.on("error", reject);
    req.setTimeout(10_000, () =>
      req.destroy(new Error("HTTP fixture timed out")),
    );
    req.end(payload);
  });
}

async function run() {
  let prompt = "";
  for await (const chunk of process.stdin) prompt += chunk;
  const encoded = prompt.match(/^Mesh Talk local API: (".+")\.$/m)?.[1];
  assert.ok(encoded);
  const helpUrl = new URL(JSON.parse(encoded));
  assert.equal(helpUrl.hostname, "127.0.0.1");
  assert.equal(helpUrl.protocol, "http:");
  if (prompt.includes("live-eval:disabled")) {
    await assert.rejects(() => request(helpUrl), { code: "ECONNREFUSED" });
    return { sent: false, unavailable: true };
  }
  const help = await request(helpUrl);
  const config = JSON.parse(
    await readFile(help.authentication.credential_file, "utf8"),
  );
  assert.equal(config.endpoint, helpUrl.origin);
  assert.equal(typeof config.token, "string");
  const invoke = (name, body) => {
    const operation = help.operations.find(
      (candidate) => candidate.name === name,
    );
    assert.ok(operation);
    return request(
      new URL(operation.path, helpUrl),
      operation.method,
      body,
      config.token,
    );
  };
  const context = await invoke("context", {});
  const { conversations } = await invoke("list_conversations", {});
  const authorized = conversations.find(
    (item) => item.name === "Authorized group",
  );
  assert.ok(authorized);
  assert.ok(
    context.destinations.some(
      (item) =>
        item.kind === authorized.destination.kind &&
        item.id === authorized.destination.id,
    ),
  );
  const body = {
    destination: authorized.destination,
    text: "Local agent live eval",
    idempotency_key: "live-eval:one",
  };
  const sent = await invoke("send_message", body);
  const retry = await invoke("send_message", body);
  const status = await invoke("get_send_result", {
    idempotency_key: body.idempotency_key,
  });
  assert.equal(sent.duplicate, false);
  assert.equal(retry.duplicate, true);
  assert.equal(retry.message.id, sent.message.id);
  assert.equal(status.message_id, sent.message.id);
  assert.equal(status.acceptance, "accepted");
  return {
    sent: sent.acceptance === "accepted",
    duplicate: retry.duplicate,
    unavailable_destination: !conversations.some(
      (item) => item.name === "Ungranted group",
    ),
    acceptance: status.acceptance,
    message_id: sent.message.id,
  };
}

try {
  process.stdout.write(`${JSON.stringify(await run())}\n`);
} catch {
  // Keep credentials, request headers and arbitrary exception text out of diagnostics.
  process.stderr.write("Deterministic local-agent provider failed.\n");
  process.exitCode = 1;
}
