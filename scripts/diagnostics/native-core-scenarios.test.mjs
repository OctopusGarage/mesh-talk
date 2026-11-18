import assert from "node:assert/strict";
import { test } from "node:test";
import { Writable } from "node:stream";
import { runInNewContext } from "node:vm";
import { writeChildCommand } from "./native-core-scenarios.mjs";
function linuxKeyFixture() {
  const host = {}, listeners = new Map(), body = {};
  const document = {
    addEventListener(type, fn, capture) { assert.equal(capture, true); assert.ok(!listeners.has(type)); listeners.set(type, fn); },
    removeEventListener(type, fn, capture) { assert.equal(capture, true); assert.equal(listeners.get(type), fn); listeners.delete(type); },
  };
  const c = {
    current: () => true,
    execute: async (script, args) => Function("window", "document", "args", `return (function(){${script}}).apply(null,args);`)(host, document, args),
    until: async (label, probe, timeout) => {
      assert.equal(timeout, 10000);
      const result = await probe();
      if (!result) throw new Error(`Timed out: ${label}`);
      return result;
    },
  };
  function edge(type, options = {}) {
    listeners.get(type)?.({ type, target: body, key: "Enter", repeat: false, isTrusted: true, ...options });
  }
  return { c, host, listeners, edge };
}

test("Linux native input requires one trusted press and release across a focus handoff", async () => {
  const { sendNativeLinuxKey } = await import("./native-core-scenarios.mjs");
  for (const [name, edges, expected] of [
    ["focused control removed, release on BODY", [["keydown", { target: {} }], ["keyup"]], null],
    ["unrelated key and untrusted input", [["keydown", { key: "x" }], ["keydown", { isTrusted: false }], ["keydown"], ["keyup"]], null],
    ["missing release", [["keydown"]], /Timed out/],
    ["repeat", [["keydown"], ["keydown", { repeat: true }], ["keyup"]], /invalid/],
    ["duplicate press", [["keydown"], ["keydown"], ["keyup"]], /invalid/],
    ["duplicate release", [["keydown"], ["keyup"], ["keyup"]], /invalid/],
    ["release without a press", [["keyup"]], /invalid/],
  ]) {
    const { c, host, listeners, edge } = linuxKeyFixture();
    const dispatch = sendNativeLinuxKey({ ...c, input: async () => { for (const args of edges) edge(...args); } }, 42, "Enter");
    if (expected) await assert.rejects(dispatch, expected, name);
    else await dispatch;
    assert.equal(listeners.size, 0, name);
    assert.equal(host.__nativeLinuxKey, undefined, name);
  }
});

test("a failed or replaced Linux key observer releases only its own listeners", async () => {
  const { sendNativeLinuxKey } = await import("./native-core-scenarios.mjs");
  const { c, listeners, edge } = linuxKeyFixture();
  let emitted = false;
  await assert.rejects(sendNativeLinuxKey({ ...c, current: () => false, input: () => { emitted = true; } }, 42, "Enter"), /live application owner/);
  assert.equal(emitted, false);
  assert.equal(listeners.size, 0);
  await assert.rejects(sendNativeLinuxKey({ ...c, input: () => { throw new Error("X11 input failed"); } }, 42, "Enter"), /X11 input failed/);
  assert.equal(listeners.size, 0);

  const firstStarted = Promise.withResolvers(), secondStarted = Promise.withResolvers();
  const firstRelease = Promise.withResolvers(), secondRelease = Promise.withResolvers();
  const first = sendNativeLinuxKey({ ...c, input: () => { firstStarted.resolve(); return firstRelease.promise; } }, 42, "Enter");
  await firstStarted.promise;
  const second = sendNativeLinuxKey({ ...c, input: () => { secondStarted.resolve(); return secondRelease.promise; } }, 42, "Enter");
  await secondStarted.promise;
  firstRelease.resolve();
  await assert.rejects(first, /replaced/);
  assert.equal(listeners.size, 2);
  edge("keydown"); edge("keyup");
  secondRelease.resolve();
  await second;
  assert.equal(listeners.size, 0);
});

test("Linux X11 input selects the unique managed main window despite visible tooltips", { skip: process.platform === "win32" }, async () => {
  const { sendNativeLinuxKey } = await import("./native-core-scenarios.mjs");
  const { mkdtemp, mkdir, writeFile, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { execFileSync } = await import("node:child_process");
  const dir = await mkdtemp(join(tmpdir(), "mesh-fixture-x11-"));
  try {
    const numericCwd = join(dir, "numeric-cwd");
    await mkdir(numericCwd);
    await writeFile(join(numericCwd, "17"), "");
    const lines = [
      "#!/bin/sh", 'case "$1" in',
      'search) [ "$*" = "search --onlyvisible --pid $OWNED_PID --limit 33" ] || exit 1; printf "%s\\n" "$VISIBLE_WINDOWS";;',
      'windowfocus|windowactivate) echo "Unexpected focus mutation" >&2; exit 1;;',
      'getwindowpid) case "$2" in 17) echo "$CLIENT_PID";; 29) echo "$OWNED_PID";; *) echo "$FOREIGN_PID";; esac;;',
      'getwindowfocus) echo "$FOCUSED_WINDOW";;',
      'getactivewindow) echo "$ACTIVE_WINDOW";;',
      'key) printf "key\\n" >> "$KEY_MARKER"; echo "$*";;',
      '*) exit 1;;', 'esac',
    ];
    await writeFile(join(dir, "xdotool"), lines.join("\n") + "\n", { mode: 0o755 });
    // xprop advances format-32 data by native long size, not wire CARD32 size.
    // The actual LP64 probe returned only 9 IDs for -len 68 and all 17 for 136.
    await writeFile(join(dir, "xprop"), String.raw`#!/bin/sh
[ "$#" -eq 9 ] || exit 1
[ "$1" = -root ] && [ "$2" = -len ] && [ "$3" -gt 0 ] && [ "$3" -le 256 ] && [ "$4" = -notype ] && [ "$5" = -f ] && [ "$6" = _NET_CLIENT_LIST ] && [ "$7" = 32c ] && [ "$9" = _NET_CLIENT_LIST ] || exit 1
printf '%s\n' "$MANAGED_LINE" | awk -v bytes="$3" '
  { eq=index($0,"="); if (!eq) { print; next }
    printf "%s", substr($0,1,eq); count=split(substr($0,eq+1),ids,","); cap=int((bytes+7)/8);
    for (i=1;i<=count && i<=cap;i++) { if(i>1)printf ","; printf "%s",ids[i] }
    printf "\n"
  }'
`, { mode: 0o755 });
    const owner = process.pid;
    const cases = [
      ["tooltip first", "29\n17", "17", "17", "17", owner, true],
      ["main first", "17\n29", "17", "17", "17", owner, true],
      ["foreign managed client", "29\n17", "19, 17", "17", "17", owner, true],
      ["main last in full managed budget", "29\n17", [...Array.from({ length: 15 }, (_, i) => i + 1000), 17].join(", "), "17", "17", owner, true],
      ["conflicting owner beyond short read", "29\n17", [17, ...Array.from({ length: 15 }, (_, i) => i + 1000), 29].join(", "), "17", "17", owner, false],
      ["foreign keyboard focus", "29\n17", "17", "19", "17", owner, false],
      ["foreign active client", "29\n17", "17", "17", "19", owner, false],
      ["wrong process", "29\n17", "17", "17", "17", owner + 1, false],
      ["unmanaged tooltip only", "29", "17", "29", "17", owner, false],
      ["ambiguous managed windows", "29\n17", "17, 29", "17", "17", owner, false],
      ["missing managed clients", "29\n17", "", "17", "17", owner, false],
      ["malformed managed clients", "29\n17", "17, not-a-window", "17", "17", owner, false],
      ["managed wildcard is not a filename", "17", "*", "17", "17", owner, false],
      ["visible wildcard is not a filename", "*", "17", "17", "17", owner, false],
      ["malformed visible window", "17\nnot-a-window", "17", "17", "17", owner, false],
      ["missing property", "17", "[missing]", "17", "17", owner, false],
      ["duplicate managed clients", "29\n17", "17, 17", "17", "17", owner, false],
      ["duplicate visible IDs", "17\n17", "17", "17", "17", owner, false],
      ["too many managed clients", "29\n17", Array.from({ length: 17 }, (_, i) => i + 17).join(", "), "17", "17", owner, false],
      ["too many visible windows", Array.from({ length: 33 }, (_, i) => i + 17).join("\n"), "17", "17", "17", owner, false],
      ["non-window ID", "29\n17", "0", "17", "17", owner, false],
      ["out-of-range window ID", "29\n17", "4294967296", "17", "17", owner, false],
    ];
    for (const [name, visible, managed, focus, active, pid, admitted] of cases) {
      const marker = join(dir, `key-${cases.findIndex(c => c[0] === name)}`);
      const { c, edge } = linuxKeyFixture();
      const dispatch = sendNativeLinuxKey({ ...c, input: (bin, args) => {
        const env = { ...process.env, PATH: `${dir}:${process.env.PATH}`, VISIBLE_WINDOWS: visible, MANAGED_LINE: managed === "[missing]" ? "_NET_CLIENT_LIST: no such atom" : `_NET_CLIENT_LIST=${managed}`, FOCUSED_WINDOW: focus, ACTIVE_WINDOW: active, CLIENT_PID: String(pid), OWNED_PID: String(owner), FOREIGN_PID: String(owner + 1), KEY_MARKER: marker };
        const output = execFileSync(bin, args, { encoding: "utf8", stdio: "pipe", env, cwd: numericCwd });
        assert.equal(output.trim(), "key --clearmodifiers Return");
        edge("keydown"); edge("keyup");
      } }, owner, "Enter");
      if (admitted) {
        await dispatch;
        assert.equal(await readFile(marker, "utf8"), "key\n", name);
      } else {
        await assert.rejects(dispatch, /X11/, name);
        await assert.rejects(readFile(marker), { code: "ENOENT" }, `${name} must not send any key`);
      }
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("AppKit accepts every native control key emitted by the fixture", async () => {
  const { nativeControlKeyName } = await import("./native-core-scenarios.mjs");
  const { readFile } = await import("node:fs/promises");
  const appKit = await readFile(new URL("../../src-tauri/examples/native-contact-eval.rs", import.meta.url), "utf8");
  const dispatch = appKit.match(/let \(code, characters\) = match key \{([\s\S]*?)\n\s*\};/);
  assert.ok(dispatch, "locate the native AppKit key match");
  const accepted = new Set([...dispatch[1].matchAll(/^\s*"([^"]+)"\s*=>/gm)].map(match => match[1]));
  // WebDriver encodes control keys in the Unicode private-use range.
  const emitted = new Set();
  for (let code = 0xe000; code <= 0xf8ff; code++) {
    const name = nativeControlKeyName(String.fromCharCode(code));
    if (name) emitted.add(name);
  }
  assert.ok(emitted.has("PageDown"), "historical receipt navigation emits PageDown");
  for (const name of emitted) assert.ok(accepted.has(name), `AppKit must accept emitted key ${name}`);
});

test("Windows native helper only emits bounded owned-PID control keys with ABI-sized SendInput", async () => {
  const { nativeWindowsKeyboardScript } = await import("./native-core-scenarios.mjs");
  for (const [key, vk] of [["\uE004", 9], ["\uE007", 13], ["\uE00C", 27], ["\uE00E", 33], ["\uE00F", 34]]) {
    const script = nativeWindowsKeyboardScript(12345, key);
    assert.ok(script.includes(`[NativeKeyboard]::SendOwned(12345, ${vk})`));
    for (const text of ["GetForegroundWindow", "GetWindowThreadProcessId", "actualPid != ownedPid", "Marshal.SizeOf(typeof(INPUT))", "SendInput(2", "inserted != 2", "GetLastWin32Error", "LayoutKind.Explicit", "MOUSEINPUT", "KEYEVENTF_KEYUP", "EnumWindows", "IsWindowVisible", "SetForegroundWindow", "FocusOwned", "Mark 'start'", "Mark 'activate'", "Mark 'compiled'", "keyboard:owned", "Mark 'sent'", "Out.Flush"]) assert.ok(script.includes(text), text);
    assert.ok(!script.includes("SendWait") && !script.includes("dispatchEvent") && !script.includes("WScript.Shell"));
    assert.match(script, /static string DescribeWindow\(IntPtr window\)/);
    assert.match(script, /GetWindowRect\(window, out rect\)/);
    assert.match(script, /GetProcessById\(\(int\)process\).SessionId/);
    assert.match(script, /SetLastError\(0\);\s*bool activated = SetForegroundWindow\(target\);\s*int\? activationError = activated \? \(int\?\)null : Marshal.GetLastWin32Error\(\);/);
    assert.match(script, /Owned application did not become foreground[\s\S]*DescribeWindow\(GetForegroundWindow\(\)\)[\s\S]*DescribeWindow\(target\)[\s\S]*activationError.HasValue/);
    assert.ok(!script.includes("GetWindowText"), "foreground diagnostics never collect window titles");
  }
  for (const pid of [0, -1, NaN, 1.5, "123;evil"]) assert.throws(() => nativeWindowsKeyboardScript(pid, "\uE00C"));
  for (const key of ["a", "{ESC}", "\uE008"]) assert.throws(() => nativeWindowsKeyboardScript(12345, key));
  const { readFile } = await import("node:fs/promises");
  const runner = await readFile(new URL("./hidden-contacts.mjs", import.meta.url), "utf8");
  assert.ok(runner.includes('until("native keyboard helper completion", () => !owned.has(child), process.platform === "win32" ? 60000 : 10000)'));
  assert.ok(runner.includes("redact(child.output.slice(-2000))"));
  assert.ok(runner.includes("nativeWindowsKeyboardScript(pid, value)"));
});
test("every emitted Windows control key is accepted by the native policy", async () => {
  const { nativeWindowsKeyboardScript } = await import("./native-core-scenarios.mjs");
  for (const [key, vk] of [["\uE004", 9], ["\uE007", 13], ["\uE00C", 27], ["\uE00E", 33], ["\uE00F", 34]]) {
    const script = nativeWindowsKeyboardScript(12345, key);
    const rejected = script.match(/if \((vk !=[^)]*)\) throw new InvalidOperationException\("Unsupported control key"\)/)?.[1];
    assert.ok(rejected, "generated native key rejection policy is present");
    const acceptedKeys = [...rejected.matchAll(/vk != (\d+)/g)].map(match => Number(match[1]));
    assert.ok(acceptedKeys.includes(vk), `emitted VK${vk} is accepted by native policy`);
    assert.deepEqual(acceptedKeys, [9, 13, 27, 33, 34], "native policy rejects every unsupported VK");
  }
});
test("Windows paging keys retain the extended-key flag without changing other controls", async () => {
  const { nativeWindowsKeyboardScript } = await import("./native-core-scenarios.mjs");
  for (const [key, vk, extended] of [["\uE004", 9, false], ["\uE007", 13, false], ["\uE00C", 27, false], ["\uE00E", 33, true], ["\uE00F", 34, true]]) {
    const script = nativeWindowsKeyboardScript(12345, key);
    const policy = script.match(/uint extended = (.*?) \? 1u : 0u;/)?.[1];
    assert.ok(policy, "generated native extended-key policy is present");
    const extendedKeys = [...policy.matchAll(/vk == (\d+)/g)].map(match => Number(match[1]));
    assert.equal(extendedKeys.includes(vk), extended, `VK${vk} native extended flag`);
    assert.deepEqual(extendedKeys, [33, 34], "only paging keys use the extended flag");
  }
});
test("cold receipt checkpoint requires exited peer and durable original IDs before navigation", async () => {
  const { receiptColdRestartCheckpoint } = await import("./native-core-scenarios.mjs");
  const ids = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
  const state = { owner: "owner", ids };
  const c = { owner: "owner", account: "target", peerExited: () => true, observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "delivered" })) };
  const checkpoint = await receiptColdRestartCheckpoint(c, state);
  assert.deepEqual(checkpoint.ids, ids);
  assert.equal(checkpoint.offlineBackendDurabilityVerified, true);
  await assert.rejects(receiptColdRestartCheckpoint({ ...c, peerExited: () => false }, state));
  await assert.rejects(receiptColdRestartCheckpoint({ ...c, observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })) }, state));
});
test("receipt evidence rejects sparse missing or changed message IDs", async () => {
  const { orderedReceiptStatuses } = await import("./native-core-scenarios.mjs");
  const ids = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
  const records = ids.map(id => ({ id, status: "delivered" }));
  assert.deepEqual(orderedReceiptStatuses(ids, records.reverse()), ["delivered", "delivered", "delivered"]);
  assert.throws(() => orderedReceiptStatuses(ids, records.slice(1)));
  assert.throws(() => orderedReceiptStatuses(ids, [...records, records[0]]));
});

test("exact native content fixtures respect the composer's intentional outer trimming", async () => {
  const { nativeMessageContents } = await import("./native-core-scenarios.mjs");
  const contents = nativeMessageContents("fixture-nonce");
  assert.equal(contents.length, 3);
  for (const content of contents) assert.ok(content === content.trim(), "exact rendering/transport fixture must not include intentionally discarded outer whitespace");
  assert.equal(contents[1].split("\n").length, 3);
  assert.ok(contents[2].length > 2000);
  assert.ok(contents[2].endsWith("end-fixture-nonce"), "long-message tail must also be preserved exactly");
});

test("signed discovery waits for the restarted UI roster and a fresh CLI response", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let polls = 0;
  const peer = { output: `peer ${userId}\n`, stdin: new Writable({ write(_bytes, _encoding, done) { peer.output += `peer ${userId}\n`; done(); } }) };
  const found = await signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => ++polls === 1 ? [] : [rawPeer], until: async (_label, poll) => { for (let i = 0; i < 2; i++) { const value = await poll(); if (value) return value; } throw new Error("missing fresh observation"); } });
  assert.equal(found, rawPeer);
  assert.equal(polls, 2);
});

test("signed discovery cannot use a stale pre-command CLI roster line", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  const peer = { output: `peer ${userId}\n`, stdin: new Writable({ write(_bytes, _encoding, done) { done(); } }) };
  await assert.rejects(signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => [rawPeer], until: async (_label, poll) => { const value = await poll(); if (value) return value; throw new Error("missing fresh observation"); } }), /missing fresh observation/);
});

test("signed discovery queries again when the first CLI roster is still empty", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let queries = 0;
  const peer = { output: "", stdin: new Writable({ write(_bytes, _encoding, done) { peer.output += ++queries === 1 ? "(no peers yet)\n" : `peer ${userId}\n`; done(); } }) };
  await signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => [rawPeer], until: async (_label, poll) => { for (let i = 0; i < 3; i++) { const value = await poll(); if (value) return value; } throw new Error("missing fresh observation"); } });
  assert.equal(queries, 2);
});

test("private restart retains UI discovery without requiring public CLI presence", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  const peer = { output: "", stdin: new Writable({ write(_bytes, _encoding, done) { done(new Error("private peer must not query public roster")); } }) };
  const found = await signedPeerObservation({ peer, userId: "a".repeat(32), peerName: rawPeer.name, observe: async () => [rawPeer], publiclyVisible: false, until: async (_label, poll) => { const value = await poll(); if (value) return value; throw new Error("missing private observation"); } });
  assert.equal(found, rawPeer);
});

test("native attachment rendering uses the visible jump-to-latest control when history is scrolled", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  let observations = 0;
  const commands = [];
  await revealLatestNativeMessage({ execute: async () => ++observations < 3 ? { visible: false, jump: true } : { visible: true, jump: false }, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "jump" }; }, until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("not visibly rendered"); } }, "attachment.txt");
  assert.equal(commands[0].body.value, '[data-testid="jump-to-latest"]');
  assert.equal(commands[1].path, "/element/jump/click");
  assert.equal(commands.length, 2, "do not restart the smooth scroll on every poll");
  assert.equal(observations, 3);
});

test("native attachment rendering retries the same button when WebDriver click does not move the list", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  let clock = 0, domClicks = 0;
  const commands = [];
  await revealLatestNativeMessage({
    now: () => clock,
    execute: async script => {
      if (script.includes("b.click()")) { domClicks++; return true; }
      return { visible: domClicks > 0, jump: domClicks === 0 };
    },
    command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "jump" }; },
    until: async (_label, poll) => { for (let i = 0; i < 5; i++) { clock += 1000; if (await poll()) return true; } throw new Error("not visibly rendered"); },
  }, "attachment.txt");
  assert.equal(commands.length, 2, "WebDriver still tries the real button first");
  assert.equal(domClicks, 1, "the fallback activates the same button only once");
});

test("native attachment rendering still fails if no visible message or jump control exists", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  await assert.rejects(revealLatestNativeMessage({ execute: async () => ({ visible: false, jump: false }), command: async () => { throw new Error("unexpected action"); }, until: async (_label, poll) => { if (await poll()) return true; throw new Error("not visibly rendered"); } }, "attachment.txt"), /not visibly rendered/);
});

test("native settings use the observed default and minimum client viewports, not nominal outer dimensions", async () => {
  const { nativeSettingsTargets } = await import("./native-core-scenarios.mjs");
  const targets = nativeSettingsTargets({ width: 1024, height: 702 }, { width: 1024, height: 674 }, { width: 760, height: 520 });
  assert.deepEqual(targets, [
    { name: "defaultLayout", request: { width: 1024, height: 702 }, viewport: { width: 1024, height: 674 } },
    { name: "minimumLayout", request: { width: 760, height: 548 }, viewport: { width: 760, height: 520 } },
  ]);
});

test("native minimum client dimensions include the measured Windows non-client border", async () => {
  const { nativeMinimumWindowRequest } = await import("./native-core-scenarios.mjs");
  assert.deepEqual(nativeMinimumWindowRequest({ width: 1056, height: 728 }, { width: 1040, height: 720 }), { width: 776, height: 528 });
  assert.deepEqual(nativeMinimumWindowRequest({ width: 1040, height: 720 }, { width: 1040, height: 720 }), { width: 760, height: 520 });
  assert.throws(() => nativeMinimumWindowRequest({ width: 1000, height: 700 }, { width: 1040, height: 720 }), /native window chrome/);
});

test("native historical rendering uses the focusable message log and real PageUp until visible", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  const commands = [], keys = [];
  let polls = 0;
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: ++polls === 3 } : true, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async value => keys.push(value), until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("history not visible"); } }, "old message");
  assert.equal(commands[0].body.value, '[role="log"]');
  assert.equal(commands[1].path, "/element/log/click");
  assert.deepEqual(keys, ["\uE00E", "\uE00E"]);
});

test("native historical rendering scrolls until the receipt badge clears both log edges", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  for (const [initialTop, expectedKey, hasBadge] of [[93, "\uE00E", true], [610, "\uE00F", true], [93, null, false]]) {
    let top = initialTop;
    const keys = [];
    const rect = (top, bottom) => ({ top, bottom, left: 300, right: 1010, width: 710, height: bottom - top });
    const badge = { getBoundingClientRect: () => rect(top, top + 24) };
    const bubble = { textContent: "old receipt", getBoundingClientRect: () => rect(top - 55, top + 26), querySelectorAll: () => hasBadge ? [badge] : [] };
    const log = { getBoundingClientRect: () => rect(102, 624) };
    const document = { activeElement: log, querySelector: () => log, querySelectorAll: () => [bubble] };
    await revealHistoricalNativeMessage({
      execute: async (script, args = []) => Function("document", "innerWidth", "innerHeight", `return function(){${script}}`)(document, 1040, 720)(...args),
      command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }),
      key: async value => { keys.push(value); top = 300; },
      until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("receipt remains clipped"); },
    }, "old receipt");
    assert.deepEqual(keys, expectedKey ? [expectedKey] : [], "an intersecting body is sufficient only when there is no receipt footer");
  }
});

test("native historical rendering does not turn missing history into success", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  await assert.rejects(revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: false } : true, command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }), key: async () => {}, until: async (_label, poll) => { if (await poll()) return true; throw new Error("history not visible"); } }, "missing message"), /history not visible/);
});

test("native historical rendering waits for asynchronous history hydration before locating the scroller", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  let hydrated = false, polls = 0;
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: true } : (hydrated = ++polls > 1), command: async () => { assert.equal(hydrated, true); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async () => { throw new Error("visible history needs no key"); }, until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("not hydrated"); } }, "restored message");
  assert.equal(polls, 2);
});

test("native layout coverage rejects invalid or indistinguishable viewport tiers", async () => {
  const { nativeSettingsTargets } = await import("./native-core-scenarios.mjs");
  for (const viewport of [{ width: NaN, height: 674 }, { width: 760, height: 400 }, { width: 760, height: 520 }]) {
    assert.throws(() => nativeSettingsTargets({ width: 1024, height: 702 }, viewport, { width: 760, height: 520 }), /native layout/);
  }
});

test("child command awaits delivery and preserves exact newlines", async () => {
  let received;
  const child = { stdin: new Writable({ write(bytes, _encoding, done) { received = bytes.toString(); done(); } }) };
  await writeChildCommand(child, "/peers\n");
  assert.equal(received, "/peers\n");
});

test("broken stdin rejects through the awaited task without an uncaught error", async () => {
  const failure = Object.assign(new Error("owned child pipe closed"), { code: "EPIPE" });
  const child = { stdin: new Writable({ write(_bytes, _encoding, done) { done(failure); } }) };
  await assert.rejects(writeChildCommand(child, "/peers\n"), { code: "EPIPE" });
  await new Promise(resolve => setImmediate(resolve));
});

test("destroyed stdin fails immediately", async () => {
  const child = { stdin: new Writable({ write(_bytes, _encoding, done) { done(); } }) };
  child.stdin.destroy();
  await assert.rejects(writeChildCommand(child, "/quit\n"), /not writable/);
});

test("each native receipt card is captured while its own expected state is visible", async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], labels = ["text", "sticker", "file"];
  let visible, recorded, failureContexts = 0;
  const c = {
    receiptFailureContext: async () => { failureContexts++; throw new Error("must not observe endpoints on pass"); },
    owner: "owner", account: "account",
    observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })),
    until: async (_description, poll) => { const result = await poll(); assert.ok(result); return result; },
    execute: async (_script, args) => { visible = args[0]; return args.length === 1 ? { visible: true } : true; },
    command: async () => { throw new Error("already visible"); },
    captureReceiptCard: async (name, expected) => { assert.equal(expected.label, visible); assert.equal(expected.status, "awaiting"); return { filename: `${name}.png`, label: visible, status: expected.status, visible: true }; },
    passed: async (_name, observations, captures) => { recorded = { observations, captures }; },
  };
  await receiptPhase(c, { owner: "owner", ids, labels }, "receipt-offline-awaiting", "awaiting");
  assert.deepEqual(recorded.captures.map(capture => capture.label), [...labels].reverse());
  assert.deepEqual(recorded.observations.captures, recorded.captures);
  assert.equal(failureContexts, 0);
});

test("native receipt timeout retains cached ordered statuses and observation failure", async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], timeout = new Error("Timed out: original IDs become delivered");
  let observations = 0;
  const c = {
    owner: "owner", account: "account",
    observe: async command => {
      if (command === "owner_account_history") return ids.map(id => ({ id, from_me: true }));
      observations++;
      if (observations === 1) return [{ id: "c", status: "delivered" }, { id: "a", status: "awaiting", observation: { kind: "attempting", age_ms: 123, body: "must not copy" } }, { id: "b", status: "awaiting", observation: { kind: "retry_scheduled", age_ms: 456, reason: "attempt_timed_out" } }];
      throw new Error("owner observation unavailable");
    },
    until: async (...args) => {
      assert.equal(args.length, 2, "retain the existing default deadline");
      assert.equal(args[0], "original IDs become delivered");
      assert.equal(await args[1](), false);
      await assert.rejects(args[1](), /owner observation unavailable/);
      throw timeout;
    },
  };
  await assert.rejects(receiptPhase(c, { owner: "owner", ids, labels: [] }, "receipt-peer-restart-delivered", "delivered"), error => {
    assert.equal(error.cause, timeout);
    assert.match(error.message, /^Timed out: original IDs become delivered;/);
    assert.match(error.message, /"statuses":\["awaiting","awaiting","delivered"\]/);
    assert.match(error.message, /"observationError":"owner observation unavailable"/);
    assert.deepEqual(JSON.parse(error.message.split("; receipt observation=")[1]).deliveryObservations, [{ kind: "attempting", age_ms: 123, reason: null }, { kind: "retry_scheduled", age_ms: 456, reason: "attempt_timed_out" }, { kind: "UNKNOWN", age_ms: null, reason: null }]);
    assert.ok(!error.message.includes("must not copy"));
    return true;
  });
  assert.equal(observations, 2, "failure reporting performs no further IPC");
});

test("receipt failure context runs once only after the original timeout", async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], original = new Error("Timed out: original IDs become delivered");
  let timedOut = false, contexts = 0;
  const c = {
    owner: "owner", account: "account",
    observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })),
    until: async (...args) => { assert.equal(args.length, 2); assert.equal(await args[1](), false); timedOut = true; throw original; },
    receiptFailureContext: async () => { assert.equal(timedOut, true); contexts++; return { currentListener: { tcpPort: 41002 }, observedPeers: [{ addr: "127.0.0.1:41001" }] }; },
  };
  await assert.rejects(receiptPhase(c, { owner: "owner", ids, labels: [] }, "restart", "delivered"), error => {
    assert.equal(error.cause, original);
    assert.match(error.message, /"tcpPort":41002/);
    assert.match(error.message, /"addr":"127.0.0.1:41001"/);
    return true;
  });
  assert.equal(contexts, 1);
});

test("receipt failure context errors cannot replace the original failure", async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], original = new Error("Timed out: original IDs become delivered");
  let contexts = 0;
  const c = {
    owner: "owner", account: "account",
    observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })),
    until: async (_description, poll) => { assert.equal(await poll(), false); throw original; },
    receiptFailureContext: async () => { contexts++; throw new Error("endpoint unavailable " + "x".repeat(2000)); },
  };
  await assert.rejects(receiptPhase(c, { owner: "owner", ids, labels: [] }, "restart", "delivered"), error => {
    assert.equal(error.cause, original);
    assert.match(error.message, /^Timed out: original IDs become delivered;/);
    assert.match(error.message, /endpoint unavailable/);
    assert.ok(error.message.length < 1000, "secondary failure remains bounded");
    return true;
  });
  assert.equal(contexts, 1);
});

test("CLI listener evidence uses only the existing exact startup line", async () => {
  const { cliListenerEndpoint } = await import("./native-core-scenarios.mjs");
  const id = "a".repeat(32);
  assert.equal(cliListenerEndpoint(`node ${id} listening`), null);
  assert.equal(cliListenerEndpoint(`node ${id} listening on tcp/0`), null);
  assert.equal(cliListenerEndpoint(`node ${id} listening on tcp/65536`), null);
  assert.deepEqual(cliListenerEndpoint(`node ${id} listening on tcp/41001, discovery udp/47474`), { deviceId: id, tcpPort: 41001 });
  assert.deepEqual(cliListenerEndpoint(`node ${id} listening on tcp/41002, discovery udp/47474`), { deviceId: id, tcpPort: 41002 });
});

test("receipt endpoint snapshot exposes only the exact fixture peer and preserves errors", async () => {
  const { receiptPeerEndpointContext } = await import("./native-core-scenarios.mjs");
  const previousPeer = { deviceId: "a".repeat(32), tcpPort: 41001, exitCode: 0 };
  const currentPeer = { deviceId: previousPeer.deviceId, tcpPort: 41002, exitCode: null };
  let calls = 0;
  const result = await receiptPeerEndpointContext({ expectedAccount: "expected", previousPeer, currentPeer, observe: async command => {
    calls++; assert.equal(command, "list_peers");
    return [{ user_id: "unrelated", account_id: "private-account", addr: "private-address", name: "private-name" },
      { user_id: currentPeer.deviceId, account_id: "observed", addr: "127.0.0.1:41001", name: "omitted" }];
  } });
  assert.equal(calls, 1);
  assert.deepEqual(result, { expectedAccount: "expected", previousPeer, currentPeer,
    observedPeers: [{ deviceId: currentPeer.deviceId, accountId: "observed", addr: "127.0.0.1:41001" }], observationError: null });
  assert.ok(!JSON.stringify(result).includes("private-"));
  const failed = await receiptPeerEndpointContext({ expectedAccount: "expected", previousPeer, currentPeer,
    observe: async () => { throw new Error("unavailable " + "x".repeat(2000)); } });
  assert.equal(failed.observedPeers, null);
  assert.equal(failed.observationError.length, 512);
  assert.deepEqual(failed.currentPeer, currentPeer);
});

test("localized wrong-password error is captured before successful sign-in recovery", async () => {
  const { wrongPasswordCheckpoint } = await import("./native-core-scenarios.mjs");
  const steps = [], errorCapture = { filename: "auth-logout-wrong-password-error.png", text: "用户名或密码错误", visible: true };
  let observed;
  await wrongPasswordCheckpoint({
    userName: "user", invalidCredentialTexts: ["用户名或密码错误"],
    captureAuthError: async (name, expected) => { steps.push("error capture"); assert.equal(name, "auth-logout-wrong-password-error"); assert.deepEqual(expected.texts, ["用户名或密码错误"]); return errorCapture; },
    login: async name => { steps.push("recovery"); assert.equal(name, "user"); return {owner:"a".repeat(36),backendReady:true,shellReady:true}; },
    passed: async (name, observations, captures) => { steps.push("passed"); assert.equal(name, "auth-logout-wrong-password"); observed = observations; assert.deepEqual(captures, [errorCapture]); },
  });
  assert.deepEqual(steps, ["error capture", "recovery", "passed"]);
  assert.equal(observed.invalidCredentialsKey, "login.invalidCredentials");
  assert.equal(observed.errorCapture.text, "用户名或密码错误");
});

test("attachment completion waits for backend readiness and completed rendered action", async () => {
  const { settledIncomingAttachment } = await import("./native-core-scenarios.mjs");
  assert.equal(typeof settledIncomingAttachment,"function");
  let ready=false,rendered=false,polls=0;
  const c={observe:async()=>[{file_conv:"conv",ready}],execute:async()=>rendered,until:async(_label,poll)=>{for(let i=0;i<3;i++){polls++; if(i===1)ready=true;if(i===2)rendered=true;const value=await poll();if(value)return value;}throw Error("not ready");}};
  const observed=await settledIncomingAttachment(c,"conv","file.txt");
  assert.equal(observed.backendReady,true);assert.equal(observed.renderedComplete,true);assert.equal(polls,3);
});

test("session readiness waits for matching backend owner and rendered ready shell", async () => {
  const { settledSignedInSession } = await import("./native-core-scenarios.mjs");
  let polls=0;
  const c={observe:async()=>({owner:polls<2?'stale':'owner'}),execute:async()=>polls>2,until:async(_label,poll)=>{for(let i=0;i<3;i++){polls++;const result=await poll();if(result)return result;}throw Error('not ready');}};
  assert.deepEqual(await settledSignedInSession(c,'owner'),{owner:'owner',backendReady:true,shellReady:true});
  assert.equal(polls,3);
  await assert.rejects(settledSignedInSession({...c,observe:async()=>({owner:'stale'})},'owner'),/not ready/);
});
test("privacy evidence waits for confirmation removal before settled settings", async()=>{
  const { settledPrivacyMode } = await import("./native-core-scenarios.mjs");
  let present=true;
  const c={execute:async()=>!present,until:async(_label,poll)=>{assert.equal(await poll(),false);present=false;assert.equal(await poll(),true);},settledDialog:async id=>{assert.equal(present,false);assert.equal(id,'settings-dialog');}};
  assert.deepEqual(await settledPrivacyMode(c),{confirmationDismissed:true,dialogSettled:true});
});
test("attachment completion retains the verified backend checkpoint after automatic-save pruning",async()=>{
  const { settledIncomingAttachment } = await import("./native-core-scenarios.mjs");
  const c={observe:async()=>[{file_conv:'conv',ready:false}],execute:async()=>true,until:async(_label,poll)=>{const result=await poll();if(!result)throw Error('not ready');return result;}};
  await assert.rejects(settledIncomingAttachment(c,'conv','file'),/not ready/);
  assert.deepEqual(await settledIncomingAttachment(c,'conv','file',true),{backendReady:true,renderedComplete:true});
});

test("native attachment fixture creates an owned download directory before configuring real settings", async () => {
  const { mkdtemp, stat, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { prepareNativeDownloadFixture } = await import("./native-core-scenarios.mjs");
  const fixture = await mkdtemp(join(tmpdir(), "native-download-fixture-"));
  const owner = "fixture-user";
  const account = "0123456789abcdef0123456789abcdef";
  let settings = { download_dir: "", stay_signed_in: false, retention_days: 30 };
  const calls = [];
  try {
    await assert.rejects(stat(join(fixture, "downloads")), { code: "ENOENT" });
    const invoke = async (command, args) => {
      calls.push(command);
      if (command === "account_id") { assert.deepEqual(args, {}); return account; }
      assert.equal(args.owner, owner, "fixture settings remain scoped to the signed-in owner");
      if (command === "get_app_settings") {
        const snapshot = { ...settings };
        if (calls.filter((call) => call === "get_app_settings").length === 1)
          settings = { ...settings, retention_days: 7 };
        return snapshot;
      }
      assert.equal(command, "set_app_settings");
      assert.equal(args.expectedAccount, account, "settings target the current cryptographic account");
      assert.deepEqual(Object.keys(args).sort(), ["expectedAccount", "owner", "patch", "settings"]);
      assert.equal(args.settings.retention_days, 30, "required IPC settings carrier may be stale");
      assert.equal((await stat(args.patch.download_dir)).isDirectory(), true, "save destination exists before it is configured");
      assert.deepEqual(args.patch, { download_dir: join(fixture, "downloads") });
      settings = { ...settings, ...args.patch };
    };
    const directory = await prepareNativeDownloadFixture({ fixture, owner, invoke });
    assert.equal(directory, join(fixture, "downloads"));
    assert.deepEqual(settings, { download_dir: directory, stay_signed_in: false, retention_days: 7 });
    assert.deepEqual(calls, ["account_id", "get_app_settings", "set_app_settings", "get_app_settings"]);
  } finally { await rm(fixture, { recursive: true, force: true }); }
});


test("native attachment verifies durable export after transient chunks are pruned", async () => {
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const { tmpdir } = await import("node:os");
  const { verifyNativeAttachmentDownload } = await import("./native-core-scenarios.mjs");
  const root = await mkdtemp(join(tmpdir(), "native-download-"));
  const path = join(root, "unique.txt"), expected = Buffer.from("exact decrypted 你好 bytes");
  try {
    let polls = 0;
    const c = { until: async (_label, poll) => {
      assert.equal(await poll(), false, "missing file cannot count as received");
      polls++;
      await writeFile(path, expected);
      return poll();
    } };
    assert.equal(await verifyNativeAttachmentDownload(c, path, expected), true);
    assert.equal(polls, 1);
    await writeFile(path, Buffer.from("corrupt"));
    await assert.rejects(verifyNativeAttachmentDownload({ until: (_label, poll) => poll() }, path, expected), /complete original bytes/);
  } finally { await rm(root, { recursive: true, force: true }); }
});


test("public restoration rechecks an early empty CLI roster despite stale pre-private output", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let queries = 0;
  const peer = { output: `peer ${userId} before private mode\n`, stdin: new Writable({
    write(_bytes, _encoding, done) {
      peer.output += ++queries === 1 ? "(no peers yet)\n" : `peer ${userId} after public restoration\n`;
      done();
    },
  }) };
  const found = await signedPeerObservation({ peer, userId, peerName: rawPeer.name,
    observe: async () => [rawPeer], until: async (_label, poll) => {
      for (let i = 0; i < 3; i++) { const result = await poll(); if (result) return result; }
      throw new Error("fresh public discovery missing");
    },
  });
  assert.equal(found, rawPeer);
  assert.equal(queries, 2, "old stdout and one early empty query cannot prove current public discovery");
});

test("native account entry submits once and defers the optional backup reminder", async () => {
  const { enterNativeAccount } = await import("./native-core-scenarios.mjs");
  for (const register of [false, true]) {
    const calls = [];
    const ui = {
      click: async id => calls.push(["click", id]),
      fill: async (id, value) => calls.push(["fill", id, value]),
      element: async id => calls.push(["element", id]),
    };
    await enterNativeAccount(ui, "alice", "password123", register);
    assert.deepEqual(calls, [
      ...(register ? [["click", "login-tab-register"]] : []),
      ["fill", "login-username", "alice"],
      ["fill", "login-password", "password123"],
      ["click", "login-submit"],
      ["element", "chat-shell"],
      ...(register ? [["click", "account-protection-later"]] : []),
    ]);
  }
});


test("native attachment fixture never writes settings without a current account", async () => {
  const { mkdtemp, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { prepareNativeDownloadFixture } = await import("./native-core-scenarios.mjs");
  const fixture = await mkdtemp(join(tmpdir(), "native-download-account-"));
  const calls = [];
  try {
    await assert.rejects(prepareNativeDownloadFixture({ fixture, owner: "fixture-user", invoke: async (command) => { calls.push(command); return undefined; } }), /requires the current account/);
    assert.deepEqual(calls, ["account_id"]);
  } finally { await rm(fixture, { recursive: true, force: true }); }
});


test("native historical receipts keep scrolling when a visible message has a clipped badge", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  let offset = 0;
  const log = { getBoundingClientRect: () => ({ top: 102, bottom: 624, left: 284, right: 1040 }) };
  const badge = { getBoundingClientRect: () => ({ top: 93 + offset, bottom: 117 + offset, left: 986, right: 1010, width: 24, height: 24 }) };
  const bubble = { textContent: "receipt-text", getBoundingClientRect: () => ({ top: 38 + offset, bottom: 119 + offset, left: 284, right: 1030, width: 746, height: 81 }), querySelectorAll: () => [badge] };
  const document = { activeElement: log, querySelector: () => log, querySelectorAll: () => [bubble] };
  const keys = [];
  await revealHistoricalNativeMessage({
    execute: async (script, args = []) => runInNewContext(`(function(){${script}})(...args)`, { document, args, innerWidth: 1040, innerHeight: 720 }),
    command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }),
    key: async key => { keys.push(key); offset = 300; },
    until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("badge remains clipped"); },
  }, "receipt-text");
  assert.deepEqual(keys, ["\uE00E"], "a partially visible message cannot certify its clipped receipt");
});

for (const mode of ["cancelled", "missing-owner", "healed"]) test(`receipt observations preserve ${mode} polling outcome without extra calls`, async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], original = mode === "cancelled" ? new DOMException("cancelled", "AbortError") : new Error(mode);
  let calls = 0, endpointCalls = 0, passed = 0;
  const c = {
    owner: "owner", account: "account",
    observe: async command => {
      if (command === "owner_account_history") return ids.map(id => ({ id, from_me: true }));
      calls++;
      if (mode === "missing-owner") throw original;
      return ids.map(id => ({ id, status: mode === "healed" && calls === 2 ? "delivered" : "awaiting", observation: { kind: "attempting", age_ms: 1 } }));
    },
    until: async (...args) => {
      assert.equal(args.length, 2, "original default deadline unchanged");
      if (mode === "missing-owner") await assert.rejects(args[1](), error => error === original);
      else assert.equal(await args[1](), false);
      if (mode === "healed") return args[1]();
      throw original;
    },
    receiptFailureContext: async () => { endpointCalls++; return null; },
    passed: async () => { passed++; },
  };
  const operation = receiptPhase(c, { owner: "owner", ids, labels: [] }, "restart", "delivered");
  if (mode === "healed") {
    await operation;
    assert.equal(passed, 1);
    assert.equal(endpointCalls, 0);
  } else await assert.rejects(operation, error => {
    assert.equal(error.cause, original);
    const diagnostic = JSON.parse(error.message.split("; receipt observation=")[1]);
    assert.deepEqual(diagnostic.deliveryObservations, mode === "missing-owner" ? ids.map(() => ({ kind: "UNKNOWN", age_ms: null, reason: null })) : ids.map(() => ({ kind: "attempting", age_ms: 1, reason: null })));
    assert.equal(endpointCalls, 1);
    return true;
  });
  assert.equal(calls, mode === "healed" ? 2 : 1, "no failure-only status requery");
});


test("attachment failure snapshot uses read-only IPC and excludes message bodies", async () => {
  const { attachmentFailureContext } = await import("./native-core-scenarios.mjs");
  const calls = [];
  const c = { userId: "recipient", account: "sender-account", peer: { output: "private message body\nfile sent to recipient\n" }, observe: async (name, args) => {
    calls.push([name, args]);
    if (name === "history") return [{ id: "raw", text: "secret text", from_me: false, file: { name: "attachment.txt", file_conv: "file", size: 12, secret: "secret payload" } }];
    if (name === "account_history") throw new Error("private error text");
    if (name === "diag_get_peers") return [{ user_id: "sender", account_id: "sender-account", ip: "127.0.0.1", tcp_port: 123, name: "private display name" }];
    return name === "my_id" ? "recipient" : "recipient-account";
  } };
  const result = await attachmentFailureContext(c, { senderDevice: "sender", fileName: "attachment.txt", outputOffset: 0 });
  assert.deepEqual(calls.map(([name]) => name), ["my_id", "account_id", "diag_get_peers", "history", "account_history"]);
  assert.deepEqual(calls[3][1], { peer: "sender", limit: 50 });
  assert.equal(result.send.accepted, true);
  assert.equal(result.history[0].file.file_conv, "file");
  assert.deepEqual(result.account_history, { observationFailed: true });
  assert.doesNotMatch(JSON.stringify(result), /secret|private/);
  const pending = await attachmentFailureContext({ ...c, peer: { output: "file sent to recipient\n" } }, { senderDevice: "sender", fileName: "attachment.txt", outputOffset: 23 });
  assert.equal(pending.send.accepted, false, "stale CLI output cannot qualify this send");
});


test("privacy cancellation waits for returned trigger focus before reopening", async () => {
  const { settledPrivacyConfirmationDismissal } = await import("./native-core-scenarios.mjs");
  let phase = 0;
  const c = {
    execute: async (script) => Function("document", "getComputedStyle", "innerWidth", "innerHeight", script)({
      querySelector: () => phase === 0 ? {} : null,
      querySelectorAll: () => {
        const outer = { state: "open", getBoundingClientRect: () => ({ left: 0, top: 0, right: 1040, bottom: 720, width: 1040, height: 720 }) };
        const closing = { ...outer, state: "closed" };
        const unrelated = { state: "closed", getBoundingClientRect: () => ({ left: 20, top: 20, right: 120, bottom: 80, width: 100, height: 60 }) };
        return [outer, unrelated, ...(phase === 2 ? [closing] : [])].filter(e => e.state === "closed");
      },
      activeElement: { getAttribute: () => phase === 1 ? "elsewhere" : "invisible-switch" },
    }, () => ({ position: "fixed", display: "block", visibility: "visible" }), 1040, 720),
    until: async (_label, poll) => {
      assert.equal(await poll(), false, "mounted confirmation is pending");
      phase = 1;
      assert.equal(await poll(), false, "removed content is not yet returned focus");
      phase = 2;
      assert.equal(await poll(), false, "returned focus cannot outlive an unretired closing modal overlay");
      phase = 3;
      assert.equal(await poll(), true, "retired confirmation backdrop does not wait for the open settings backdrop or unrelated closed controls");
    },
  };
  assert.deepEqual(await settledPrivacyConfirmationDismissal(c), { confirmationDismissed: true, triggerFocusRestored: true, closingSurfacesRetired: true });
  await assert.rejects(settledPrivacyConfirmationDismissal({ execute: async () => false, until: async (_label, poll) => { assert.equal(await poll(), false); throw new Error("trigger focus never returned"); } }), /trigger focus never returned/);
});


test("privacy dialog diagnostics are bounded, content-free and retire their exact owner", async () => {
  const { startPrivacyDialogTrace } = await import("./native-core-scenarios.mjs");
  const handlers = new Map(), observers = [], host = {};
  let state = "open", focus = "invisible-cancel", clock = 0;
  const document = {
    body: {},
    querySelector: () => state === "absent" ? null : { closest: () => ({ getAttribute: () => state }) },
    get activeElement() { return { getAttribute: () => focus }; },
    addEventListener: (type, handler) => { assert.ok(!handlers.has(type)); handlers.set(type, handler); },
    removeEventListener: (type, handler) => { assert.equal(handlers.get(type), handler); handlers.delete(type); },
  };
  class Observer {
    constructor(callback) { this.callback = callback; this.disconnected = 0; observers.push(this); }
    observe(body) { assert.equal(body, document.body); }
    disconnect() { this.disconnected++; }
  }
  const c = { execute: async script => Function("window", "document", "MutationObserver", "performance", script)(host, document, Observer, { now: () => clock++ }) };
  const first = await startPrivacyDialogTrace(c);
  const finish = await startPrivacyDialogTrace(c);
  assert.equal(observers[0].disconnected, 1, "replacement retires the previous observer");
  assert.deepEqual(await first(), [], "stale cleanup cannot stop a replacement trace");
  assert.equal(handlers.size, 6);
  const target = { getAttribute: () => "invisible-cancel" };
  handlers.get("keydown")({ target, key: "PRIVATE-KEY-TEXT" });
  handlers.get("keydown")({ target: { getAttribute: () => "login-password" }, key: "Enter" });
  assert.equal(host.__nativePrivacyDialogTrace.events.length, 2, "unrelated input is not observed");
  assert.equal(host.__nativePrivacyDialogTrace.events.at(-1).key, null);
  assert.ok(!JSON.stringify(host.__nativePrivacyDialogTrace.events).includes("PRIVATE-KEY-TEXT"));
  handlers.get("keydown")({ target, key: "Enter" });
  assert.equal(host.__nativePrivacyDialogTrace.events.at(-1).key, "Enter");
  handlers.get("keyup")({ target: document.body, key: "Enter" });
  assert.deepEqual(
    [host.__nativePrivacyDialogTrace.events.at(-1).kind, host.__nativePrivacyDialogTrace.events.at(-1).target, host.__nativePrivacyDialogTrace.events.at(-1).key],
    ["keyup", null, "Enter"],
    "a trusted control can disappear before release reaches BODY",
  );
  handlers.get("pointerdown")({ target: { getAttribute: () => "PRIVATE-UNRELATED-ID" } });
  assert.equal(host.__nativePrivacyDialogTrace.events.at(-1).target, null);
  assert.ok(!JSON.stringify(host.__nativePrivacyDialogTrace.events).includes("PRIVATE-UNRELATED-ID"));
  for (let i = 0; i < 100; i++) handlers.get("click")({ target });
  state = "absent"; focus = "invisible-switch";
  observers[1].callback();
  const trace = await finish();
  assert.equal(trace.length, 48);
  assert.deepEqual(trace.at(-1), { at: clock - 1, kind: "state", target: null, key: null, state: "absent", focus });
  assert.ok(trace.every((event, i) => i === 0 || event.at > trace[i - 1].at));
  assert.ok(!JSON.stringify(trace).includes("PRIVATE-KEY-TEXT"));
  assert.equal(handlers.size, 0);
  assert.equal(observers[1].disconnected, 1);
  assert.equal(host.__nativePrivacyDialogTrace, undefined);
  assert.deepEqual(await finish(), []);
});


test("successful privacy confirmation retires its closing backdrop before settling settings", async () => {
  const { settledPrivacyMode } = await import("./native-core-scenarios.mjs");
  let backdrop = true, settled = false;
  const document = {
    querySelector: () => null,
    querySelectorAll: () => backdrop ? [{ getBoundingClientRect: () => ({ left: 0, top: 0, right: 1040, bottom: 720, width: 1040, height: 720 }) }] : [],
    activeElement: { getAttribute: () => "invisible-switch" },
  };
  const c = {
    execute: async script => Function("document", "getComputedStyle", "innerWidth", "innerHeight", script)(document, () => ({ position: "fixed", display: "block", visibility: "visible" }), 1040, 720),
    until: async (_label, poll) => {
      assert.equal(await poll(), false, "successful commit has not retired its closing backdrop");
      assert.equal(settled, false);
      backdrop = false;
      assert.equal(await poll(), true);
    },
    settledDialog: async id => { assert.equal(backdrop, false); assert.equal(id, "settings-dialog"); settled = true; },
  };
  assert.deepEqual(await settledPrivacyMode(c), { confirmationDismissed: true, dialogSettled: true });
  assert.equal(settled, true);
});
