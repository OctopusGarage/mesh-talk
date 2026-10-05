// Runtime assertions against the owned native webview, real CLI, and read-only
// production observations. This module does not replace any application IPC.
import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

export function writeChildCommand(child, text) {
  return new Promise((resolve, reject) => {
    const stream = child.stdin;
    if (!stream || stream.destroyed || !stream.writable || stream.writableEnded) {
      reject(new Error("Owned child stdin is not writable"));
      return;
    }
    stream.once("error", reject);
    try {
      stream.write(text, error => {
        if (error) reject(error);
        else { stream.removeListener("error", reject); resolve(); }
      });
    } catch (error) { reject(error); }
  });
}

export function nativeMessageContents(nonce) {
  return [`你好 🌍 café ${nonce}`, `first ${nonce}
second line
第三行`, `long-${nonce}-` + "mesh消息🙂 ".repeat(300) + `end-${nonce}`];
}

// CI-only OS input, never DOM keyboard events. INPUT's union includes MOUSEINPUT
// so Marshal.SizeOf retains the native x64 40-byte layout (not keyboard-only 32).
export function nativeWindowsKeyboardScript(pid, key) {
  const vk = new Map([["\uE004", 9], ["\uE007", 13], ["\uE00C", 27], ["\uE00E", 33]]).get(key);
  assert.ok(Number.isSafeInteger(pid) && pid > 0 && pid <= 0xffffffff, "owned Windows PID");
  assert.ok(vk, "allowlisted native control key");
  return `$ErrorActionPreference = 'Stop';
function Mark($phase) { [Console]::WriteLine('keyboard:' + $phase); [Console]::Out.Flush() }
Mark 'start';
$shell = New-Object -ComObject WScript.Shell;
if (-not $shell.AppActivate(${pid})) { throw 'Owned application could not be activated' }
Mark 'activate'; Start-Sleep -Milliseconds 200;
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NativeKeyboard {
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public MOUSEINPUT mi; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION data; }
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
  [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, INPUT[] inputs, int size);
  public static void SendOwned(uint ownedPid, ushort vk) {
    if (vk != 9 && vk != 13 && vk != 27 && vk != 33) throw new InvalidOperationException("Unsupported control key");
    const uint KEYEVENTF_KEYUP = 2;
    uint extended = vk == 33 ? 1u : 0u;
    INPUT[] inputs = new INPUT[2];
    inputs[0].type = inputs[1].type = 1;
    inputs[0].data.ki.wVk = inputs[1].data.ki.wVk = vk;
    inputs[0].data.ki.dwFlags = extended;
    inputs[1].data.ki.dwFlags = extended | KEYEVENTF_KEYUP;
    uint actualPid;
    if (GetWindowThreadProcessId(GetForegroundWindow(), out actualPid) == 0 || actualPid != ownedPid) throw new InvalidOperationException("Foreground window is not owned application");
    Console.WriteLine("keyboard:owned"); Console.Out.Flush();
    if (GetWindowThreadProcessId(GetForegroundWindow(), out actualPid) == 0 || actualPid != ownedPid) throw new InvalidOperationException("Foreground ownership changed before SendInput");
    uint inserted = SendInput(2, inputs, Marshal.SizeOf(typeof(INPUT)));
    if (inserted != 2) throw new InvalidOperationException("SendInput inserted=" + inserted + " Win32Error=" + Marshal.GetLastWin32Error());
  }
}
'@
Mark 'compiled';
[NativeKeyboard]::SendOwned(${pid}, ${vk});
Mark 'sent';`;
}

export function orderedReceiptStatuses(ids, records) {
  assert.equal(records.length, ids.length, "all requested receipt IDs have a projection");
  return ids.map(id => {
    const matches = records.filter(record => record.id === id);
    assert.equal(matches.length, 1, "exactly one projection for original ID");
    assert.ok(["awaiting", "delivered"].includes(matches[0].status));
    return matches[0].status;
  });
}

export async function receiptColdRestartCheckpoint(c, state) {
  assert.equal(c.peerExited(), true, "receiver exited before cold backend checkpoint");
  assert.equal(c.owner, state.owner, "original receipt owner restored");
  const history = await c.observe("owner_account_history", { owner: c.owner, account: c.account, limit: 500 });
  for (const id of state.ids) assert.equal(history.filter(item => item.id === id && item.from_me).length, 1, "cold history retains original ID exactly once");
  const statuses = orderedReceiptStatuses(state.ids, await c.observe("owner_delivery_statuses", { owner: c.owner, account: c.account, ids: state.ids }));
  assert.deepEqual(statuses, ["delivered", "delivered", "delivered"], "Delivered persisted without a live receipt producer");
  assert.equal(c.peerExited(), true, "receiver remained exited throughout cold backend checkpoint");
  return { owner: c.owner, account: c.account, ids: [...state.ids], statuses, offlineBackendDurabilityVerified: true, peerOfflineDuringSourceRestart: true };
}

async function receiptPhase(c, state, name, expected, historical = false, cold = undefined) {
  assert.equal(c.owner, state.owner, "receipt owner unchanged");
  if (historical) {
    assert.equal(cold?.offlineBackendDurabilityVerified, true, "cold checkpoint precedes receiver restart/UI navigation");
    assert.equal(cold.owner, c.owner);
    assert.equal(cold.account, c.account);
    assert.deepEqual(cold.ids, state.ids);
    assert.deepEqual(cold.statuses, ["delivered", "delivered", "delivered"]);
    assert.equal(c.peerExited(), false, "UI projection verified after receiver rediscovery");
  }
  const history = await c.observe("owner_account_history", { owner: c.owner, account: c.account, limit: 500 });
  for (const id of state.ids) assert.equal(history.filter(item => item.id === id && item.from_me).length, 1, "original accepted history entry retained once");
  const statuses = await c.until(`original IDs become ${expected}`, async () => {
    const values = orderedReceiptStatuses(state.ids, await c.observe("owner_delivery_statuses", { owner: c.owner, account: c.account, ids: state.ids }));
    return values.every(value => value === expected) && values;
  });
  for (const label of [...state.labels].reverse()) {
    if (historical) await revealHistoricalNativeMessage(c, label);
    else await revealLatestNativeMessage(c, label);
    await c.until(`native ${label} card is ${expected}`, () => c.execute("const e=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).find(e=>e.textContent.includes(arguments[0]));return !!e&&e.querySelectorAll('[data-delivery]').length===1&&e.querySelector('[data-delivery]')?.getAttribute('data-delivery')===arguments[1];", [label, expected]));
  }
  await c.passed(name, { owner: c.owner, account: c.account, ids: state.ids, originalIds: state.ids, statuses, cardsVerified: true, actualPeerExit: true, sameKeystorePeerRestart: expected === "delivered", actualProcessRestart: historical, peerOfflineDuringSourceRestart: historical && cold.peerOfflineDuringSourceRestart, offlineBackendDurabilityVerified: historical && cold.offlineBackendDurabilityVerified, coldIds: cold?.ids, coldStatuses: cold?.statuses, uiVerifiedAfterPeerRestart: historical, acceptancePath: "text: native composer; sticker/file: real owner IPC plus native history rendering", fileReceiptMeaning: "target durable custody, not user download/read" });
}

export async function receiptScenarios(c) {
  await c.stopPeer(); // actual exit, deliberately no presence TTL wait
  await c.click(c.row());
  const text = `receipt-text-${c.nonce}`, sticker = `receipt-sticker-${c.nonce}`, fileName = `receipt-file-${c.nonce}.txt`;
  await c.fill("composer-input", text);
  await c.click("composer-send");
  const textEntry = await c.until("offline text accepted with original event ID", async () => (await c.observe("owner_account_history", { owner: c.owner, account: c.account, limit: 500 })).find(item => item.from_me && item.text === text));
  const stickerId = await c.invoke("owner_enqueue_sticker", { owner: c.owner, account: c.account, stickerId: `native-${c.nonce}`, fallback: sticker });
  const path = join(c.fixture, fileName);
  await writeFile(path, `outbound custody fixture ${c.nonce}\n`);
  const file = await c.invoke("owner_enqueue_file", { owner: c.owner, account: c.account, path, media: false });
  const state = { owner: c.owner, ids: [textEntry.id, stickerId, file.id], labels: [text, sticker, fileName] };
  // Real navigation hydrates IPC-created fixtures through production owner history.
  const group = (await c.observe("list_channels"))[0];
  assert.ok(group, "existing native group fixture available for navigation");
  await c.click(`conversation-row-${group.channel_id}`);
  await c.click(c.row());
  await receiptPhase(c, state, "receipt-offline-awaiting", "awaiting");
  await c.restartPeer();
  await receiptPhase(c, state, "receipt-peer-restart-delivered", "delivered");
  return state;
}

export async function receiptRestartScenario(c, state, cold) {
  await c.click(c.row());
  await receiptPhase(c, state, "receipt-source-restart-durable", "delivered", true, cold);
}

export async function signedPeerObservation({ peer, userId, peerName, observe, until }) {
  // Sign-out/sign-in creates a fresh node. Discovery is asynchronous: the old
  // CLI output is not proof that the replacement UI node has rediscovered it.
  const rawPeer = await until("restarted UI rediscovers signed CLI identity", async () =>
    (await observe("list_peers")).find(p => p.name === peerName));
  let outputStart = peer.output.length;
  await until("fresh CLI roster sees UI node", async () => {
    if (peer.output.slice(outputStart).includes(`peer ${userId}`)) return true;
    // Discovery in the opposite direction may also lag. A single empty roster
    // response cannot become successful unless we actually query it again.
    outputStart = peer.output.length;
    await writeChildCommand(peer, "/peers\n");
    return peer.output.slice(outputStart).includes(`peer ${userId}`);
  });
  return rawPeer;
}

export async function revealLatestNativeMessage({ execute, until, command }, text) {
  await until("incoming attachment visibly rendered in native message log", async () => {
    const state = await execute("const e=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).find(e=>e.textContent.includes(arguments[0])),r=e?.getBoundingClientRect(),l=document.querySelector('[role=log]')?.getBoundingClientRect(),b=document.querySelector('button[aria-label=\"Jump to latest messages\"]'),q=b?.getBoundingClientRect();return {visible:!!r&&!!l&&r.width>0&&r.height>0&&r.bottom>l.top&&r.top<l.bottom&&r.right>l.left&&r.left<l.right,jump:!!q&&q.width>0&&q.height>0};", [text]);
    if (state.visible) return true;
    // The production virtual list deliberately preserves a history reader's
    // position. Reveal new content through its real user-facing control, not
    // by replacing IPC or directly mutating the scroll position.
    if (state.jump) {
      const button = await command("POST", "/element", { using: "css selector", value: 'button[aria-label="Jump to latest messages"]' });
      await command("POST", `/element/${button["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
    }
    return false;
  });
}

export async function revealHistoricalNativeMessage({ execute, until, command, key }, text) {
  // Virtuoso's real scroller is keyboard-focusable. Native PageUp exercises
  // normal browser scrolling, rather than mutating its virtual-list state.
  await until("restarted message log hydrated", () => execute("return !!document.querySelector('[role=log]');"));
  const log = await command("POST", "/element", { using: "css selector", value: '[role="log"]' });
  await command("POST", `/element/${log["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
  await until("historical message visibly rendered after native PageUp", async () => {
    const visible = await execute("const e=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).find(e=>e.textContent.includes(arguments[0])),r=e?.getBoundingClientRect(),l=document.querySelector('[role=log]')?.getBoundingClientRect();return !!r&&!!l&&r.width>0&&r.height>0&&r.bottom>l.top&&r.top<l.bottom&&r.right>l.left&&r.left<l.right;", [text]);
    if (visible) return true;
    assert.equal(await execute("return document.activeElement===document.querySelector('[role=log]');"), true, "native PageUp targets the focused message log");
    await key("\uE00E");
    return false;
  });
}

export function nativeMinimumWindowRequest(defaultRect, defaultViewport) {
  const width = defaultRect.width - defaultViewport.width, height = defaultRect.height - defaultViewport.height;
  assert.ok(Number.isFinite(width) && Number.isFinite(height) && width >= 0 && height >= 0, "native window chrome must be measured from valid outer/client dimensions");
  return { width: 760 + width, height: 520 + height };
}

export function nativeSettingsTargets(defaultRect, defaultViewport, minimumViewport) {
  for (const rect of [defaultRect, defaultViewport, minimumViewport]) {
    assert.ok(Number.isFinite(rect.width) && Number.isFinite(rect.height) && rect.width >= 760 && rect.height >= 520, "native layout needs a valid minimum-sized desktop viewport");
  }
  assert.ok(defaultViewport.width > minimumViewport.width || defaultViewport.height > minimumViewport.height, "default and minimum native layout tiers must differ");
  return [
    { name: "defaultLayout", request: { width: defaultRect.width, height: defaultRect.height }, viewport: defaultViewport },
    { name: "minimumLayout", request: nativeMinimumWindowRequest(defaultRect, defaultViewport), viewport: minimumViewport },
  ];
}

export async function coreScenarios(c) {
  const { execute, observe, until, passed, fill, click, peer, account, userId,
    fixture, history, row, privacySettings, key, signOut, login, userName } = c;
  const rendered = text => until("message rendered in real native webview", () => execute(
    "return Array.from(document.querySelectorAll('[data-testid=message-bubble]')).some(e=>e.textContent.includes(arguments[0]));", [text]));
  await passed("auth-register-signin", { registered: true, signedIn: true, owner: c.owner });
  await signOut();
  await fill("login-username", userName);
  await fill("login-password", "incorrect-eval-password");
  await click("login-submit");
  await until("wrong password rejected visibly", () => execute("return !!document.querySelector('[data-testid=login-form] .text-destructive')?.textContent && !document.querySelector('[data-testid=chat-shell]');"));
  await login(userName);
  await passed("auth-logout-wrong-password", { loggedOut: true, wrongPasswordRejected: true, signinRestored: true });
  const defaultWindow = await c.command("GET", "/window/rect");
  const defaultLayout = await profileLayout(c);
  await c.command("POST", "/window/rect", nativeMinimumWindowRequest(defaultWindow, defaultLayout.viewport));
  await until("minimum client layout applied", () => execute("return Math.abs(innerWidth-760)<=1 && Math.abs(innerHeight-520)<=1;"));
  const minimumLayout = await profileLayout(c);
  const settingsTargets = nativeSettingsTargets(defaultWindow, defaultLayout.viewport, minimumLayout.viewport);
  await c.openDialog("open-profile");
  await c.settledDialog("profile-dialog");
  await passed("native-profile-layout", { defaultLayout, minimumLayout, profileDialogVisible: true, profileUsernameVisible: true });
  await key("\uE00C");
  await until("profile evidence dialog dismissed", async () => !await c.exists("profile-dialog"));
  await c.command("POST", "/window/rect", settingsTargets[0].request);
  const rawPeer = await signedPeerObservation(c);
  assert.ok(rawPeer);
  assert.match(rawPeer.user_id, /^[0-9a-f]{32}$/);
  await passed("signed-peer-discovery", { accountId: account, peerUserId: rawPeer.user_id, cliSawGui: true });
  await click(row());
  const ownAccount = await observe("account_id");
  const outbound = `core-outbound-${c.nonce}`;
  await fill("composer-input", outbound);
  await click("composer-send");
  await rendered(outbound);
  await until("real CLI receives outbound text", () => peer.output.includes(outbound));
  await until("outbound persisted", async () => (await history()).some(h => h.text === outbound && h.from_me));
  const inbound = `core-inbound-${c.nonce}`;
  await writeChildCommand(peer, `/account-msg ${ownAccount} ${inbound}\n`);
  await rendered(inbound);
  await until("inbound persisted", async () => (await history()).some(h => h.text === inbound && !h.from_me));
  await passed("direct-messages", { uiToCliRendered: true, uiToCliPersisted: true, cliToUiRendered: true, cliToUiPersisted: true });
  const contents = nativeMessageContents(c.nonce);
  assert.equal(contents[1].split("\n").length, 3, "real multiline textarea fixture");
  let multilineLayout;
  for (const text of contents) {
    await fill("composer-input", text);
    await click("composer-send");
    await rendered(text);
    await until("message content persisted exactly", async () => (await history()).some(h => h.text === text));
    await until("message content reaches CLI", () => peer.output.includes(text));
    if (text.includes("\n")) {
      multilineLayout = await execute("const text=arguments[0],bubble=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).find(e=>e.textContent.includes(text)),content=bubble&&Array.from(bubble.querySelectorAll('span')).find(e=>e.textContent===text),r=content?.getBoundingClientRect();return {whiteSpace:content&&getComputedStyle(content).whiteSpace,width:r?.width,height:r?.height,lineHeight:content&&parseFloat(getComputedStyle(content).lineHeight),visible:!!r&&r.bottom>0&&r.top<innerHeight&&r.right>0&&r.left<innerWidth};", [text]);
      assert.ok(["pre-wrap", "pre-line", "break-spaces"].includes(multilineLayout.whiteSpace), "rendered multiline message preserves line breaks");
      assert.ok(multilineLayout.width > 0 && multilineLayout.height >= multilineLayout.lineHeight * 2, "multiline message occupies multiple visible lines");
      assert.equal(multilineLayout.visible, true);
    }
  }
  await passed("message-content", { unicodeRendered: true, multilineRendered: true, longRendered: true, persistedCount: contents.length, multilineLayout });
  const groupName = `core-group-${c.nonce}`;
  await writeChildCommand(peer, `/channel-new ${groupName} ${userId}\n`);
  const group = await until("real CLI group distributed", async () => (await observe("list_channels")).find(g => g.name === groupName));
  const groupText = `real-group-message-${c.nonce}`;
  await writeChildCommand(peer, `/channel-msg ${group.channel_id} ${groupText}\n`);
  await until("group message durable", async () => (await observe("channel_history", { channelId: group.channel_id, limit: 50 })).some(h => h.text === groupText));
  await click(`conversation-row-${group.channel_id}`);
  await rendered(groupText);
  await passed("group-message", { channelId: group.channel_id, rendered: true, persisted: true });
  await click(row());
  const fileName = `native-attachment-${c.nonce}.txt`;
  const bytes = Buffer.from(`fixture attachment 你好 ${c.nonce}\n`);
  const filePath = join(fixture, fileName);
  await writeFile(filePath, bytes);
  await writeChildCommand(peer, `/sendfile ${userId} ${filePath}\n`);
  const attachment = await until("real incoming attachment manifest durable", async () => (await history()).find(h => h.file?.name === fileName));
  assert.equal(attachment.file.media, false);
  assert.equal(attachment.file.size, bytes.length);
  await revealLatestNativeMessage(c, fileName);
  const classification = await execute("const e=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).find(e=>e.textContent.includes(arguments[0]));return !!e && !!e.querySelector('button') && !e.querySelector('[data-testid=file-image],[data-testid=file-video]');", [fileName]);
  assert.equal(classification, true);
  await until("incoming attachment decrypted bytes available", async () => {
    const received = await c.readFileBytes(attachment.file.file_conv);
    assert.deepEqual(received, Array.from(bytes));
    return true;
  });
  await passed("incoming-attachment", { fileName, manifestPersisted: true, genericFileRendered: true, bytesVerified: true, downloadVerified: false, limitation: "Real read_file bytes verified; OS save picker is outside embedded W3C DOM automation and was not invoked." });
  await privacySettings();
  const settingsLayouts = {};
  for (const { name, request, viewport } of settingsTargets) {
    const returned = await c.command("POST", "/window/rect", request);
    await until(`settings ${name} client resize`, () => execute("return Math.abs(innerWidth-arguments[0])<=1 && Math.abs(innerHeight-arguments[1])<=1;", [viewport.width, viewport.height]));
    await c.settledDialog("settings-dialog");
    const layout = await execute("const e=document.querySelector('[data-testid=settings-dialog]'),r=e.getBoundingClientRect(),p=document.querySelector('[data-testid=theme-picker]'),q=p.getBoundingClientRect();return {viewport:{width:innerWidth,height:innerHeight},dialog:{x:r.x,y:r.y,right:r.right,bottom:r.bottom},themePickerVisible:q.width>0 && q.height>0};");
    assert.ok(layout.dialog.x >= 0 && layout.dialog.y >= 0 && layout.dialog.right <= layout.viewport.width + 1 && layout.dialog.bottom <= layout.viewport.height + 1);
    assert.equal(layout.themePickerVisible, true);
    settingsLayouts[name] = { ...layout, requestedWindow: request, returnedWindow: returned, dialogContained: true };
  }
  await passed("native-settings-layout", { ...settingsLayouts, dialogContained: true, themePickerVisible: true });
  await click("theme-light");
  await until("visible theme selected", () => execute("return document.querySelector('[data-testid=theme-light]')?.getAttribute('aria-pressed')==='true' && localStorage.getItem('mesh-talk-theme')==='light' && !document.documentElement.classList.contains('dark');"));
  await key("\uE00C");
  await c.command("POST", "/window/rect", settingsTargets[0].request);
  return { outbound, inbound, contents, selectedTheme: "light", rendered, minimumWindow: settingsTargets[1].request };
}

async function profileLayout(c) {
  await c.focusOwnedWindow();
  const layout = await c.execute("const e=document.querySelector('[data-testid=open-profile]'),r=e.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+1);return {viewport:{width:innerWidth,height:innerHeight},avatar:{x:r.x,y:r.y,right:r.right,bottom:r.bottom},topEdgeHit:!!hit && (hit===e || e.contains(hit))};");
  assert.equal(layout.topEdgeHit, true, "own avatar top edge receives pointer input");
  layout.chrome = null;
  if (process.platform === "darwin") {
    const payload = await c.command("POST", "/execute/async", { script: "const done=arguments[arguments.length-1];let off=()=>{};window.__TAURI__.event.listen('native-contact-chrome-result',e=>{off();done(e.payload.result);}).then(fn=>{off=fn;return window.__TAURI__.event.emit('native-contact-chrome','bounds');}).catch(e=>done({Err:String(e)}));", args: [] });
    assert.ok(payload.Ok, `actual AppKit chrome observation failed: ${payload.Err}`);
    layout.chrome = payload.Ok;
    assert.equal(layout.chrome.buttons.length, 3);
    for (const b of layout.chrome.buttons) {
      const a = layout.avatar;
      assert.ok(a.right <= b.x || a.x >= b.right || a.bottom <= b.y || a.y >= b.bottom, "own avatar clears actual native traffic-light bounds");
    }
  }
  layout.chromeClear = process.platform === "darwin" ? true : null;
  await c.openDialog("open-profile");
  await c.settledDialog("profile-dialog");
  await c.element("profile-username");
  await c.key("\uE00C");
  await c.until("profile dialog dismissed", async () => !await c.exists("profile-dialog"));
  return layout;
}

export async function restartedCoreScenarios(c, state) {
  await c.click(c.row());
  for (const text of [state.outbound, state.inbound, ...state.contents].reverse()) {
    await revealHistoricalNativeMessage(c, text);
    assert.ok((await c.history()).some(h => h.text === text));
  }
  await c.passed("history-process-restart", { actualProcessRestart: true, rendered: true, persisted: true });
  await c.privacySettings();
  assert.equal(await c.execute("return document.querySelector('[data-testid=theme-light]')?.getAttribute('aria-pressed')==='true' && localStorage.getItem('mesh-talk-theme')==='light' && !document.documentElement.classList.contains('dark');"), true);
  await c.passed("settings-theme-persistence", { selected: state.selectedTheme, visibleSelection: true, persistedAfterRestart: true });
  await c.key("\uE00C");
}
