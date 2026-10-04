// Read-only runner capability diagnostic; it never seeds a product roster.
import { createSocket } from "node:dgram";
import { networkInterfaces } from "node:os";
import { randomBytes } from "node:crypto";

const group = "224.0.0.167";
const marker = randomBytes(16);
const receiver = createSocket({ type: "udp4", reuseAddr: true });
const sender = createSocket("udp4");
const report = { platform: process.platform, joins: [], received: false, errors: [] };
let finish;
const done = new Promise(resolve => { finish = resolve; });
receiver.on("message", data => { if (data.equals(marker)) { report.received = true; finish(); } });
for (const socket of [receiver, sender]) socket.on("error", error => { report.errors.push(error.message); finish(); });
const timer = setTimeout(finish, 5000);
try {
  await new Promise((resolve, reject) => { receiver.once("error", reject); receiver.bind(0, resolve); });
  for (const [name, addresses] of Object.entries(networkInterfaces())) {
    for (const address of addresses ?? []) {
      if (address.family !== "IPv4" || address.internal) continue;
      try { receiver.addMembership(group, address.address); report.joins.push(name); }
      catch (error) { report.errors.push(`${name}: ${error.message}`); }
    }
  }
  try { receiver.addMembership(group); report.joins.push("default"); }
  catch (error) { report.errors.push(`default: ${error.message}`); }
  sender.send(marker, receiver.address().port, group);
  await done;
} catch (error) {
  report.errors.push(error.message);
} finally {
  clearTimeout(timer);
  for (const socket of [receiver, sender]) {
    try { socket.close(); } catch { /* a failed socket may already be closed */ }
  }
  console.log(JSON.stringify(report));
}
