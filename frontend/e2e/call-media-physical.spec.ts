import { test, expect, chromium } from "@playwright/test";
import {
  spawn,
  execFileSync,
  type ChildProcessWithoutNullStreams,
} from "node:child_process";
import { accessSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { prepareCallMedia } from "./helpers/call-media-harness";

// Explicit opt-in: requires two prepared test SDK binaries, SSH access to
// windows10, and a direct Tailscale route. Never runs in the normal UI suite.
test.use({
  launchOptions: {
    channel: "chromium",
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
      "--autoplay-policy=no-user-gesture-required",
      "--disable-features=WebRtcHideLocalIpsWithMdns",
    ],
  },
});

const enabled = process.env.MESH_TALK_PHYSICAL_MEDIA === "1";
const windowsHost = process.env.MESH_TALK_MEDIA_WINDOWS_SSH ?? "windows10";
const localIp = process.env.MESH_TALK_MEDIA_LOCAL_IP ?? "100.113.196.110";
const remoteIp = process.env.MESH_TALK_MEDIA_REMOTE_IP ?? "100.123.85.82";
const encoded = (script: string) =>
  Buffer.from(script, "utf16le").toString("base64");
const psCommand = (script: string) =>
  `powershell -NoProfile -EncodedCommand ${encoded(script)}`;
const remote = (script: string) =>
  execFileSync(
    "ssh",
    [
      "-o",
      "BatchMode=yes",
      windowsHost,
      psCommand(
        `$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; ${script}`,
      ),
    ],
    { encoding: "utf8", timeout: 30_000 },
  );

class Host {
  process: ChildProcessWithoutNullStreams;
  lines: string[] = [];
  signal?: (from: string, payload: string) => Promise<void>;
  errors: string[] = [];
  constructor(command: string, args: string[]) {
    this.process = spawn(command, args);
    this.process.on("error", (error) => this.errors.push(String(error)));
    this.process.stdin.on("error", (error) => this.errors.push(String(error)));
    createInterface({ input: this.process.stdout }).on("line", (line) => {
      this.lines.push(line);
      if (line.startsWith("error ")) this.errors.push(line);
      const match = /^signal (\S+) (\S+)$/.exec(line);
      if (match)
        void this.signal?.(
          match[1],
          Buffer.from(match[2], "base64").toString(),
        ).catch((error) => this.errors.push(String(error)));
    });
    this.process.stderr.on("data", (data) => process.stderr.write(data));
  }
  send(command: string) {
    this.process.stdin.write(command + "\n");
  }
  async ready() {
    await expect
      .poll(() => this.lines.some((line) => line.startsWith("ready ")), {
        timeout: 90_000,
      })
      .toBe(true);
    const [, id, account, tcp, udp] = this.lines
      .find((line) => line.startsWith("ready "))!
      .split(" ");
    return { id, account, tcp, udp };
  }
  async stop() {
    if (this.process.exitCode !== null) return;
    this.send("quit");
    await Promise.race([
      new Promise<void>((resolve) =>
        this.process.once("exit", () => resolve()),
      ),
      new Promise<void>((resolve) => setTimeout(resolve, 5000)),
    ]);
    if (this.process.exitCode === null) this.process.kill();
  }
}

test("physical Mac/Windows Tailscale media uses authenticated backend signaling", async ({
  browser,
  baseURL,
}, testInfo) => {
  test.skip(
    !enabled,
    "Set MESH_TALK_PHYSICAL_MEDIA=1 after building the network-acceptance example for native and Windows targets.",
  );
  test.setTimeout(240_000);
  const root = resolve(process.cwd(), "..");
  const windowsBinary =
    process.env.MESH_TALK_MEDIA_WINDOWS_BINARY ??
    resolve(
      root,
      "target/x86_64-pc-windows-gnu/debug/examples/network-acceptance.exe",
    );
  accessSync(windowsBinary);
  accessSync(resolve(root, "target/debug/examples/network-acceptance"));
  const token = `mesh-talk-media-${process.pid}-${Date.now()}`;
  const localDir = mkdtempSync(resolve(tmpdir(), token));
  const remoteDir = `D:\\Temp\\${token}`;
  const localPort = Number(new URL(baseURL!).port);
  const remoteVitePort = 59161,
    remoteCdpPort = 59162,
    localCdpPort = 59163;
  const rule = token;
  let tunnel: ReturnType<typeof spawn> | undefined;
  let edgeSession: ReturnType<typeof spawn> | undefined;
  let a: Host | undefined, b: Host | undefined;
  let remoteBrowser:
    Awaited<ReturnType<typeof chromium.connectOverCDP>> | undefined;
  const localContext = await browser.newContext({
    permissions: ["microphone", "camera"],
  });
  try {
    remote(
      `New-Item -ItemType Directory -Force -Path '${remoteDir}' | Out-Null`,
    );
    execFileSync(
      "scp",
      [
        "-q",
        windowsBinary,
        `${windowsHost}:D:/Temp/${token}/network-acceptance.exe`,
      ],
      { timeout: 60_000 },
    );
    remote(
      `New-NetFirewallRule -DisplayName '${rule}-sdk' -Direction Inbound -Action Allow -Program '${remoteDir}\\network-acceptance.exe' -RemoteAddress '${localIp}' | Out-Null; New-NetFirewallRule -DisplayName '${rule}-media' -Direction Inbound -Action Allow -Protocol UDP -Program 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe' -RemoteAddress '${localIp}' | Out-Null`,
    );
    tunnel = spawn("ssh", [
      "-N",
      "-o",
      "BatchMode=yes",
      "-o",
      "ExitOnForwardFailure=yes",
      "-L",
      `${localCdpPort}:127.0.0.1:${remoteCdpPort}`,
      "-R",
      `${remoteVitePort}:localhost:${localPort}`,
      windowsHost,
    ]);
    tunnel.stderr?.on("data", (data) => process.stderr.write(data));
    edgeSession = spawn("ssh", [
      "-T",
      "-o",
      "BatchMode=yes",
      windowsHost,
      psCommand(
        `$env:TEMP='${remoteDir}'; $env:TMP='${remoteDir}'; $p=Start-Process -PassThru -WorkingDirectory '${remoteDir}' 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe' -ArgumentList @('--headless=new','--no-first-run','--no-default-browser-check','--use-fake-device-for-media-stream','--use-fake-ui-for-media-stream','--autoplay-policy=no-user-gesture-required','--disable-features=WebRtcHideLocalIpsWithMdns','--remote-debugging-port=${remoteCdpPort}','--user-data-dir=${remoteDir}\\edge-profile','about:blank'); $p.Id | Set-Content '${remoteDir}\\edge.pid'; Wait-Process -Id $p.Id`,
      ),
    ]);
    edgeSession.stderr?.on("data", (data) => process.stderr.write(data));
    await expect
      .poll(
        async () => {
          try {
            remoteBrowser = await chromium.connectOverCDP(
              `http://127.0.0.1:${localCdpPort}`,
              { timeout: 1500 },
            );
            return true;
          } catch {
            return false;
          }
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    a = new Host(resolve(root, "target/debug/examples/network-acceptance"), [
      "--data",
      resolve(localDir, "node"),
      "--name",
      "Media Mac",
      "--discovery-port",
      "59164",
    ]);
    b = new Host("ssh", [
      "-T",
      "-o",
      "BatchMode=yes",
      windowsHost,
      psCommand(
        `$env:TEMP='${remoteDir}'; $env:TMP='${remoteDir}'; Set-Location '${remoteDir}'; & '${remoteDir}\\network-acceptance.exe' --data '${remoteDir}\\node' --name 'Media Windows' --discovery-port 59165`,
      ),
    ]);
    const [identityA, identityB] = await Promise.all([a.ready(), b.ready()]);
    a.send(`probe ${remoteIp}:${identityB.udp}`);
    b.send(`probe ${localIp}:${identityA.udp}`);
    await expect
      .poll(
        () => {
          a!.send("peers");
          return a!.lines.some(
            (line) =>
              line.startsWith(`peer ${identityB.id} `) &&
              line.includes(remoteIp),
          );
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    await expect
      .poll(
        () => {
          b!.send("peers");
          return b!.lines.some(
            (line) =>
              line.startsWith(`peer ${identityA.id} `) &&
              line.includes(localIp),
          );
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    const pageA = await localContext.newPage();
    const pageB = await remoteBrowser!.contexts()[0].newPage();
    for (const [page, host, identity] of [
      [pageA, a, identityA],
      [pageB, b, identityB],
    ] as const) {
      await page.exposeFunction(
        "__deliverCallSignal",
        async ({ target, payload }: { target: string; payload: string }) => {
          host.send(
            `signal ${target} ${Buffer.from(payload).toString("base64")}`,
          );
        },
      );
      host.signal = async (from, payload) => {
        expect(from).toBe(identity === identityA ? identityB.id : identityA.id);
        await page.evaluate(
          ({ from, payload }) =>
            window.__callMediaHarness.signal(from, payload),
          { from, payload },
        );
      };
    }
    await Promise.all([
      prepareCallMedia(pageA, baseURL!, identityA.id, true),
      prepareCallMedia(
        pageB,
        `http://127.0.0.1:${remoteVitePort}`,
        identityB.id,
        true,
      ),
    ]);
    await pageA.evaluate(
      (target) => window.__callMediaHarness.start(target),
      identityB.id,
    );
    await expect
      .poll(() => pageB.evaluate(() => window.__callMediaHarness.phase()), {
        timeout: 20_000,
      })
      .toBe("incoming");
    await pageB.evaluate(() => window.__callMediaHarness.accept());
    const evidence = [];
    for (const [page, expectedLocal, expectedRemote] of [
      [pageA, localIp, remoteIp],
      [pageB, remoteIp, localIp],
    ] as const) {
      await expect
        .poll(() => page.evaluate(() => window.__callMediaHarness.phase()), {
          timeout: 30_000,
        })
        .toBe("connected");
      await expect
        .poll(
          async () => {
            const stats = await page.evaluate(() =>
              window.__callMediaHarness.stats(),
            );
            return (
              stats.dtls === true &&
              [
                "audioSent",
                "audioReceived",
                "videoSent",
                "videoReceived",
                "videoFrames",
                "audioSamples",
              ].every((key) => Number(stats[key]) > 0) &&
              Number(stats.videoFrames) >= 15 &&
              Number(stats.audioSamples) >= 48_000
            );
          },
          { timeout: 20_000 },
        )
        .toBe(true);
      const stats = await page.evaluate(() =>
        window.__callMediaHarness.stats(),
      );
      expect(stats.localAddress).toBe(expectedLocal);
      expect(stats.remoteAddress).toBe(expectedRemote);
      evidence.push(stats);
    }
    await pageA.evaluate(() => window.__callMediaHarness.hangup());
    for (const page of [pageA, pageB]) {
      await expect
        .poll(() => page.evaluate(() => window.__callMediaHarness.phase()))
        .toBe("idle");
      await expect
        .poll(() => page.evaluate(() => window.__callMediaHarness.cleaned()))
        .toBe(true);
    }
    expect(a.errors).toEqual([]);
    expect(b.errors).toEqual([]);
    await testInfo.attach("physical-tailscale-media", {
      body: JSON.stringify(evidence, null, 2),
      contentType: "application/json",
    });
    console.log(
      "Physical Tailscale ICE/DTLS/RTP evidence",
      JSON.stringify(evidence),
    );
  } finally {
    await Promise.allSettled([
      localContext.close(),
      remoteBrowser?.close(),
      a?.stop(),
      b?.stop(),
    ]);
    tunnel?.kill();
    edgeSession?.kill();
    try {
      remote(
        `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*${token}*' -and $_.Name -in @('msedge.exe','network-acceptance.exe') } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }; Get-NetFirewallRule -DisplayName '${rule}-*' -ErrorAction SilentlyContinue | Remove-NetFirewallRule; Remove-Item -Recurse -Force '${remoteDir}' -ErrorAction SilentlyContinue; if (Test-Path '${remoteDir}') { throw 'Temporary media directory cleanup failed' }; if (Get-NetFirewallRule -DisplayName '${rule}-*' -ErrorAction SilentlyContinue) { throw 'Temporary media firewall cleanup failed' }`,
      );
    } finally {
      rmSync(localDir, { recursive: true, force: true });
    }
  }
});
