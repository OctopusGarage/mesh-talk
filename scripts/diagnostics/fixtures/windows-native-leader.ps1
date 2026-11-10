param([string]$Directory,[string]$Token,[string]$Node,[switch]$Hold,[int]$ExitCode=0)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class OwnedProbeLaunch {
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
 public struct SI {
  public uint cb; public string reserved,desktop,title;
  public uint x,y,xSize,ySize,xChars,yChars,fill,flags;
  public ushort show,reservedSize;
  public IntPtr reservedPointer,input,output,error;
 }
 [StructLayout(LayoutKind.Sequential)]
 public struct PI { public IntPtr process,thread; public uint pid,tid; }
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
 static extern bool CreateProcessW(string application,StringBuilder command,IntPtr pa,IntPtr ta,bool inherit,uint flags,IntPtr environment,string cwd,ref SI info,out PI process);
 [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
 public static uint Start(string node,string leaf,string ready,string token,string cwd) {
  var info=new SI(); info.cb=(uint)Marshal.SizeOf(typeof(SI)); PI child;
  var args=new StringBuilder("\""+node+"\" \""+leaf+"\" \""+ready+"\" \""+token+"\"");
  if(!CreateProcessW(node,args,IntPtr.Zero,IntPtr.Zero,false,0x08000000,IntPtr.Zero,cwd,ref info,out child))
   throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
  CloseHandle(child.thread); CloseHandle(child.process); return child.pid;
 }
}
'@
$ready=Join-Path $Directory 'leaf-ready.json'
$leafId=[OwnedProbeLaunch]::Start($Node,(Join-Path $Directory 'leaf.mjs'),$ready,$Token,$Directory)
$watch=[System.Diagnostics.Stopwatch]::StartNew()
while ($true) {
  if (Test-Path $ready) {
    $receipt=Get-Content $ready -Raw | ConvertFrom-Json
    if ($receipt.token -eq $Token -and $receipt.pid -eq $leafId) { break }
  }
  if ($watch.ElapsedMilliseconds -gt 10000) { throw 'Owned leaf readiness not acknowledged' }
  Start-Sleep -Milliseconds 10
}
[System.IO.File]::WriteAllText((Join-Path $Directory 'leader-ready.json'),(@{token=$Token;pid=$PID;leafPid=$leafId}|ConvertTo-Json -Compress))
if($Hold){while($true){Start-Sleep -Seconds 1}}
exit $ExitCode
