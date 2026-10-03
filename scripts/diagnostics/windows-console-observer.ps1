param(
    [Parameter(Mandatory = $true)][string]$Artifacts,
    [Parameter(Mandatory = $true)][int]$ApplicationPid
)
$ErrorActionPreference = 'Stop'

# Windows PowerShell supplies the desktop .NET/Management assemblies on hosted runners.
# No injection, registry changes, privileged application IPC, or raw window titles.
Add-Type -ReferencedAssemblies System, System.Core, System.Management, System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Management;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

public class NativeConsoleEvent {
    public string kind;
    public long timestamp;
    public long deliveredAt;
    public long hwnd;
    public uint processId;
    public uint parentPid;
    public string name;
    public string className;
    public bool visible;
    public bool isNetsh;
    public bool isFixture;
    public uint eventId;
}

public static class NativeConsoleObserver {
    private delegate void WinEventProc(IntPtr hook, uint evt, IntPtr hwnd,
        int objectId, int childId, uint thread, uint time);
    private delegate bool EnumProc(IntPtr hwnd, IntPtr parameter);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr module,
        WinEventProc callback, uint process, uint thread, uint flags);
    [DllImport("user32.dll")] private static extern bool UnhookWinEvent(IntPtr hook);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc callback, IntPtr parameter);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hwnd, StringBuilder value, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int size);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("kernel32.dll")] private static extern uint GetTickCount();
    [DllImport("kernel32.dll")] private static extern bool FreeConsole();
    [DllImport("kernel32.dll")] private static extern bool AttachConsole(uint process);
    [DllImport("kernel32.dll")] private static extern IntPtr GetConsoleWindow();
    [DllImport("kernel32.dll")] private static extern uint GetConsoleProcessList([Out] uint[] processes, uint size);
    private static readonly List<NativeConsoleEvent> Events = new List<NativeConsoleEvent>();
    private static readonly HashSet<long> Sampled = new HashSet<long>();
    private static readonly HashSet<long> Captured = new HashSet<long>();
    private static readonly HashSet<uint> Bound = new HashSet<uint>();
    private static readonly List<Task> Screenshots = new List<Task>();
    private static string DirectoryPath;
    private static int ApplicationPid;
    private static long Now() { return DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(); }
    private static void Record(NativeConsoleEvent entry) { lock (Events) { Events.Add(entry); } }

    private static void Window(IntPtr hwnd, uint evt, long timestamp) {
        if (hwnd == IntPtr.Zero) return;
        StringBuilder cls = new StringBuilder(256);
        GetClassName(hwnd, cls, cls.Capacity);
        bool console = cls.ToString() == "ConsoleWindowClass";
        if (!console && evt != 3) return;
        StringBuilder title = new StringBuilder(1024);
        GetWindowText(hwnd, title, title.Capacity);
        // Preserve only classification booleans, never an SSID or arbitrary window title.
        string caption = title.ToString().ToLowerInvariant();
        uint process;
        GetWindowThreadProcessId(hwnd, out process);
        NativeConsoleEvent entry = new NativeConsoleEvent {
            kind = evt == 3 ? "foreground" : "window", timestamp = timestamp, deliveredAt = Now(),
            hwnd = hwnd.ToInt64(), processId = process, className = cls.ToString(),
            visible = IsWindowVisible(hwnd), isNetsh = caption.Contains("netsh"),
            isFixture = caption.Contains("mesh-talk-console-fixture"), eventId = evt
        };
        if (evt == 0 && (!entry.visible || !Sampled.Add(entry.hwnd))) return;
        Record(entry);
        if (console && entry.visible && (entry.isNetsh || entry.isFixture) &&
            Captured.Count < 20 && Captured.Add(entry.hwnd)) {
            // Asynchronous desktop samples: these may be taken after a brief window disappears.
            Screenshots.Add(Task.Run(delegate {
                Rectangle bounds = SystemInformation.VirtualScreen;
                using (Bitmap image = new Bitmap(bounds.Width, bounds.Height)) {
                    using (Graphics graphics = Graphics.FromImage(image)) {
                        graphics.CopyFromScreen(bounds.Location, Point.Empty, bounds.Size);
                    }
                    image.Save(Path.Combine(DirectoryPath, "desktop-" + entry.timestamp + "-" + entry.hwnd + ".png"), ImageFormat.Png);
                    Record(new NativeConsoleEvent {
                        kind = "desktop-sample", timestamp = Now(), hwnd = entry.hwnd
                    });
                }
            }));
        }
    }

    private static void ProbeConsoleOwners() {
        NativeConsoleEvent[] candidates;
        lock (Events) {
            candidates = Events.FindAll(e => e.kind == "process-start" &&
                e.parentPid == ApplicationPid && Now() - e.timestamp < 2000).ToArray();
        }
        foreach (NativeConsoleEvent child in candidates) {
            if (Bound.Contains(child.processId) || !AttachConsole(child.processId)) continue;
            try {
                IntPtr hwnd = GetConsoleWindow();
                uint[] members = new uint[64];
                uint count = GetConsoleProcessList(members, (uint)members.Length);
                if (hwnd != IntPtr.Zero && count > 0 && count <= members.Length &&
                    Array.IndexOf(members, child.processId) >= 0) {
                    Record(new NativeConsoleEvent {
                        kind = "console-owner", timestamp = Now(), hwnd = hwnd.ToInt64(),
                        processId = child.processId, parentPid = child.parentPid
                    });
                    Bound.Add(child.processId);
                }
            } finally { FreeConsole(); }
        }
    }

    public static NativeConsoleEvent[] Run(string directory, int applicationPid) {
        DirectoryPath = directory;
        ApplicationPid = applicationPid;
        // Detach this hidden observer console so it can briefly query only app-owned children.
        // AttachConsole neither creates a console nor changes its visibility; always detach again.
        FreeConsole();
        WinEventProc callback = delegate(IntPtr hook, uint evt, IntPtr hwnd,
            int objectId, int childId, uint thread, uint time) {
            if (objectId == 0 && childId == 0) {
                long timestamp = Now() - unchecked(GetTickCount() - time);
                Window(hwnd, evt, timestamp);
            }
        };
        IntPtr objects = SetWinEventHook(0x8000, 0x800C, IntPtr.Zero, callback, 0, 0, 2);
        IntPtr foreground = SetWinEventHook(3, 3, IntPtr.Zero, callback, 0, 0, 2);
        if (objects == IntPtr.Zero || foreground == IntPtr.Zero)
            throw new InvalidOperationException("SetWinEventHook failed: " + Marshal.GetLastWin32Error());
        using (ManagementEventWatcher watcher = new ManagementEventWatcher(
            new WqlEventQuery("SELECT * FROM Win32_ProcessStartTrace WHERE ProcessName = 'netsh.exe'"))) {
            watcher.EventArrived += delegate(object sender, EventArrivedEventArgs args) {
                ManagementBaseObject data = args.NewEvent;
                Record(new NativeConsoleEvent {
                    kind = "process-start", name = "netsh.exe",
                    timestamp = (long)(Convert.ToUInt64(data["TIME_CREATED"]) / 10000) - 11644473600000L,
                    processId = Convert.ToUInt32(data["ProcessID"]),
                    parentPid = Convert.ToUInt32(data["ParentProcessID"])
                });
            };
            try {
                watcher.Start();
                File.WriteAllText(Path.Combine(directory, "observer-ready"), applicationPid.ToString());
                long deadline = Now() + 240000;
                EnumProc sample = delegate(IntPtr hwnd, IntPtr parameter) { Window(hwnd, 0, Now()); return true; };
                while (!File.Exists(Path.Combine(directory, "observer-stop"))) {
                    if (Now() > deadline) throw new TimeoutException("Observer exceeded four minutes");
                    // Out-of-context WinEvents require a message pump on this exact thread.
                    Application.DoEvents();
                    ProbeConsoleOwners();
                    EnumWindows(sample, IntPtr.Zero);
                    Thread.Sleep(5);
                }
                watcher.Stop();
                if (!Task.WaitAll(Screenshots.ToArray(), 10000))
                    throw new TimeoutException("Desktop screenshots did not finish");
                lock (Events) { return Events.ToArray(); }
            } finally {
                UnhookWinEvent(objects);
                UnhookWinEvent(foreground);
                GC.KeepAlive(callback);
            }
        }
    }
}
'@

$events = [NativeConsoleObserver]::Run($Artifacts, $ApplicationPid)
ConvertTo-Json -InputObject @($events) -Depth 5 | Set-Content -LiteralPath (Join-Path $Artifacts 'events.json') -Encoding UTF8
