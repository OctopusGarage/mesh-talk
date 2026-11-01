// Feature-local native ownership for the validation stage. Windows 10+ is
// required for creation-time JOB_LIST assignment; there is no unowned fallback.
using System;
using System.IO;
using System.Text;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public class OwnerResult
{
    public long exitCode=-1;
    public bool retired=false;
    public bool survivors=false;
    public bool cancelled=false;
    public string error;
    public long startupMs;
}
public static class WindowsCommandOwner
{
    const uint KillOnJobClose = 0x2000;
    const uint StartUseStdHandles = 0x100;
    const uint ExtendedStartupInfoPresent = 0x80000;
    const uint CreateUnicodeEnvironment = 0x400;
    const uint CreateNoWindow = 0x8000000;
    const int JobListAttribute = 0x2000d;
    const int HandleListAttribute = 0x20002;
    const uint WaitTimeout = 258;

    [StructLayout(LayoutKind.Sequential)] struct StartupInfo
    {
        public int cb;
        public IntPtr reserved, desktop, title;
        public int x,y,xs,ys,xc,yc,fill,flags;
        public short show,reserved2;
        public IntPtr reserved3, stdin,stdout,stderr;
    }
    [StructLayout(LayoutKind.Sequential)] struct StartupInfoEx
    {
        public StartupInfo si;
        public IntPtr attributes;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInformation
    {
        public IntPtr process,thread;
        public uint pid,tid;
    }
    [StructLayout(LayoutKind.Sequential)] struct SecurityAttributes
    {
        public int length;
        public IntPtr descriptor;
        public int inherit;
    }
    [StructLayout(LayoutKind.Sequential)] struct BasicLimit
    {
        public long processTime,jobTime;
        public uint flags;
        public UIntPtr min,max;
        public uint processes;
        public UIntPtr affinity;
        public uint priority,scheduling;
    }
    [StructLayout(LayoutKind.Sequential)] struct IoCounters
    {
        public ulong ro,wo,oo,rb,wb,ob;
    }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimit
    {
        public BasicLimit basic;
        public IoCounters io;
        public UIntPtr processMemory,jobMemory,peakProcess,peakJob;
    }
    [StructLayout(LayoutKind.Sequential)] struct Accounting
    {
        public long user,kernel,periodUser,periodKernel;
        public uint faults,total,active,terminated;
    }
    [DllImport("kernel32",SetLastError=true,CharSet=CharSet.Unicode)] static extern IntPtr CreateJobObjectW(IntPtr security,string name);
    [DllImport("kernel32",SetLastError=true)] static extern bool SetInformationJobObject(IntPtr job,int type,ref ExtendedLimit info,int size);
    [DllImport("kernel32",SetLastError=true)] static extern bool QueryInformationJobObject(IntPtr job,int type,out Accounting info,int size,IntPtr length);
    [DllImport("kernel32",SetLastError=true)] static extern bool TerminateJobObject(IntPtr job,uint code);
    [DllImport("kernel32",SetLastError=true)] static extern bool InitializeProcThreadAttributeList(IntPtr list,int count,int flags,ref IntPtr size);
    [DllImport("kernel32",SetLastError=true)] static extern bool UpdateProcThreadAttribute(IntPtr list,uint flags,IntPtr attribute,IntPtr value,IntPtr size,IntPtr previous,IntPtr returned);
    [DllImport("kernel32")] static extern void DeleteProcThreadAttributeList(IntPtr list);
    [DllImport("kernel32",SetLastError=true,CharSet=CharSet.Unicode)] static extern bool CreateProcessW(string app,StringBuilder command,IntPtr ps,IntPtr ts,bool inherit,uint flags,IntPtr environment,string cwd,ref StartupInfoEx startup,out ProcessInformation process);
    [DllImport("kernel32",SetLastError=true)] static extern bool CreatePipe(out IntPtr read,out IntPtr write,ref SecurityAttributes security,uint size);
    [DllImport("kernel32",SetLastError=true)] static extern bool SetHandleInformation(IntPtr handle,uint mask,uint flags);
    [DllImport("kernel32")] static extern uint WaitForSingleObject(IntPtr handle,uint ms);
    [DllImport("kernel32",SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr handle,out uint code);
    [DllImport("kernel32")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32",SetLastError=true,EntryPoint="QueryInformationJobObject")] static extern bool QueryMembers(IntPtr job,int type,IntPtr info,int size,IntPtr returned);
    [DllImport("kernel32",SetLastError=true)] static extern IntPtr OpenProcess(uint access,bool inherit,uint pid);
    [DllImport("kernel32",SetLastError=true)] static extern bool IsProcessInJob(IntPtr process,IntPtr job,out bool inside);
    // ActiveProcesses is accounting metadata and can lag process exit. Inspect
    // the complete member list and retained process handles before calling any
    // associated process runnable. Final retirement still requires job emptiness.
    static bool HasRunnableMembers(IntPtr job)
    {
        int capacity=64;
        while(true)
        {
            int size=checked(8+IntPtr.Size*capacity);
            IntPtr buffer=Marshal.AllocHGlobal(size);
            try
            {
                bool complete=QueryMembers(job,3,buffer,size,IntPtr.Zero);
                int error=Marshal.GetLastWin32Error();
                if(!complete && error!=234) throw new System.ComponentModel.Win32Exception(error);
                int assigned=Marshal.ReadInt32(buffer,0), count=Marshal.ReadInt32(buffer,4);
                if(!complete || count<assigned)
                {
                    capacity=Math.Max(checked(capacity*2),assigned);
                    continue;
                }
                for(int i=0;i<count;i++)
                {
                    uint pid=unchecked((uint)Marshal.ReadIntPtr(buffer,8+i*IntPtr.Size).ToInt64());
                    IntPtr member=OpenProcess(0x100000|0x1000,false,pid);
                    if(member==IntPtr.Zero)
                    {
                        error=Marshal.GetLastWin32Error();
                        if(error==87) continue; // The listed process has exited.
                        throw new System.ComponentModel.Win32Exception(error);
                    }
                    try
                    {
                        bool inside;
                        Check(IsProcessInJob(member,job,out inside));
                        if(!inside) continue; // PID reuse cannot grant ownership.
                        uint state=WaitForSingleObject(member,0);
                        if(state==WaitTimeout) return true;
                        if(state!=0) throw new Exception("Job member exit state is unknown");
                    }
                    finally { CloseHandle(member); }
                }
                return false;
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }
    }
    static void Check(bool ok)
    {
        if(!ok) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
    }
    static string Quote(string arg)
    {
        var b=new StringBuilder("\"");
        int slash=0;
        foreach(char c in arg)
        {
            if(c=='\\')
            {
                slash++;
                continue;
            }
            if(c=='\"') b.Append('\\',slash*2+1);
            else b.Append('\\',slash);
            slash=0;
            b.Append(c);
        }
        b.Append('\\',slash*2);
        return b.Append('"').ToString();
    }
    static uint Active(IntPtr job)
    {
        Accounting a;
        Check(QueryInformationJobObject(job,1,out a,Marshal.SizeOf(typeof(Accounting)),IntPtr.Zero));
        return a.active;
    }
    static bool WaitForRetirement(IntPtr job,IntPtr leader,System.Diagnostics.Stopwatch cleanup)
    {
        while((Active(job)>0 || WaitForSingleObject(leader,0)!=0) && cleanup.ElapsedMilliseconds<5000)
            Thread.Sleep(10);
        return Active(job)==0 && WaitForSingleObject(leader,0)==0;
    }
    static Task Copy(IntPtr handle,Stream destination)
    {
        return Task.Run(()=> { using(var source=new FileStream(new SafeFileHandle(handle,true),FileAccess.Read,4096,false)) { source.CopyTo(destination);destination.Flush(); } });
    }
    public static OwnerResult Run(string app,string[] args,string[] environment,string cwd)
    {
        var result=new OwnerResult();
        var clock=System.Diagnostics.Stopwatch.StartNew();
        IntPtr job=IntPtr.Zero,list=IntPtr.Zero,jobValue=IntPtr.Zero,handles=IntPtr.Zero,env=IntPtr.Zero;
        IntPtr inputRead=IntPtr.Zero,inputWrite=IntPtr.Zero,outputRead=IntPtr.Zero,outputWrite=IntPtr.Zero,errorRead=IntPtr.Zero,errorWrite=IntPtr.Zero;
        ProcessInformation process=new ProcessInformation();
        Task output=null,error=null;
        bool launched=false, attributesInitialized=false;
        var control=Task.Run(()=>Console.In.ReadLine());
        System.Diagnostics.Stopwatch cleanup=null;
        try
        {
            job=CreateJobObjectW(IntPtr.Zero,null);
            Check(job!=IntPtr.Zero);
            var limits=new ExtendedLimit();
            // Sole noninheritable job handle: descendants cannot break away, and
            // helper death closes ownership even when it cannot return proof.
            limits.basic.flags=KillOnJobClose;
            Check(SetInformationJobObject(job,9,ref limits,Marshal.SizeOf(typeof(ExtendedLimit))));
            var security=new SecurityAttributes { length=Marshal.SizeOf(typeof(SecurityAttributes)), inherit=1 };
            Check(CreatePipe(out inputRead,out inputWrite,ref security,0));
            Check(SetHandleInformation(inputWrite,1,0));
            Check(CreatePipe(out outputRead,out outputWrite,ref security,0));
            Check(SetHandleInformation(outputRead,1,0));
            Check(CreatePipe(out errorRead,out errorWrite,ref security,0));
            Check(SetHandleInformation(errorRead,1,0));
            IntPtr size=IntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero,2,0,ref size);
            list=Marshal.AllocHGlobal(size);
            Check(InitializeProcThreadAttributeList(list,2,0,ref size));
            attributesInitialized=true;
            jobValue=Marshal.AllocHGlobal(IntPtr.Size);
            Marshal.WriteIntPtr(jobValue,job);
            Check(UpdateProcThreadAttribute(list,0,(IntPtr)JobListAttribute,jobValue,(IntPtr)IntPtr.Size,IntPtr.Zero,IntPtr.Zero));
            handles=Marshal.AllocHGlobal(IntPtr.Size*3);
            Marshal.WriteIntPtr(handles,0,inputRead);
            Marshal.WriteIntPtr(handles,IntPtr.Size,outputWrite);
            Marshal.WriteIntPtr(handles,IntPtr.Size*2,errorWrite);
            Check(UpdateProcThreadAttribute(list,0,(IntPtr)HandleListAttribute,handles,(IntPtr)(IntPtr.Size*3),IntPtr.Zero,IntPtr.Zero));
            env=Marshal.StringToHGlobalUni(string.Join("\0",environment.OrderBy(value => value, StringComparer.OrdinalIgnoreCase))+"\0\0");
            var startup=new StartupInfoEx();
            startup.si.cb=Marshal.SizeOf(typeof(StartupInfoEx));
            startup.si.flags=(int)StartUseStdHandles;
            startup.si.stdin=inputRead;
            startup.si.stdout=outputWrite;
            startup.si.stderr=errorWrite;
            startup.attributes=list;
            if(control.IsCompleted)
            {
                result.cancelled=true;
                result.retired=true;
                result.exitCode=130;
                return result;
            }
            // JOB_LIST and the restricted stdio HANDLE_LIST apply before the
            // first target instruction. Assign-after-start leaves an orphan gap.
            Check(CreateProcessW(app,new StringBuilder(string.Join(" ",new[]{app}.Concat(args).Select(Quote))),IntPtr.Zero,IntPtr.Zero,true,ExtendedStartupInfoPresent|CreateUnicodeEnvironment|CreateNoWindow,env,cwd,ref startup,out process));
            launched=true;
            result.startupMs=clock.ElapsedMilliseconds;
            CloseHandle(process.thread);
            process.thread=IntPtr.Zero;
            CloseHandle(inputRead);
            inputRead=IntPtr.Zero;
            CloseHandle(outputWrite);
            outputWrite=IntPtr.Zero;
            CloseHandle(errorWrite);
            errorWrite=IntPtr.Zero;
            output=Copy(outputRead,Console.OpenStandardOutput());
            outputRead=IntPtr.Zero;
            error=Copy(errorRead,Console.OpenStandardError());
            errorRead=IntPtr.Zero;
            while(WaitForSingleObject(process.process,20)==WaitTimeout)
            {
                if(output.IsFaulted||error.IsFaulted) throw new Exception("Owned output forwarding failed");
                if(control.IsCompleted)
                {
                    result.cancelled=true;
                    cleanup=System.Diagnostics.Stopwatch.StartNew();
                    Check(TerminateJobObject(job,130));
                    break;
                }
            }
            if(cleanup==null) cleanup=System.Diagnostics.Stopwatch.StartNew();
            if(!result.cancelled)
            {
                result.survivors=HasRunnableMembers(job);
                if(result.survivors) Check(TerminateJobObject(job,1));
            }
            // Job emptiness closes descendant admission. The signaled leader
            // handle separately proves its exit; these observations can race.
            // Retirement, output drain and error recovery share one 5s budget,
            // inside the Node owner's 7s missing-receipt backstop.
            result.retired=WaitForRetirement(job,process.process,cleanup);
            if(!result.retired) throw new Exception("Job retirement could not be verified");
            uint code;
            Check(GetExitCodeProcess(process.process,out code));
            result.exitCode=code;
            if(!Task.WaitAll(new[]{output,error},Math.Max(0,5000-(int)cleanup.ElapsedMilliseconds))) throw new Exception("Owned output pipes did not close");
        }
        catch(Exception e)
        {
            result.error=e.Message;
            if(launched && job!=IntPtr.Zero)
            {
                TerminateJobObject(job,1);
                if(cleanup==null) cleanup=System.Diagnostics.Stopwatch.StartNew();
                try
                {
                    result.retired=WaitForRetirement(job,process.process,cleanup);
                }
                catch
                {
                    result.retired=false;
                }
            }
            else result.retired=true;
        }
        finally
        {
            foreach(var h in new[]{inputRead,inputWrite,outputRead,outputWrite,errorRead,errorWrite,process.process,process.thread,job})if(h!=IntPtr.Zero)CloseHandle(h);
            if(list!=IntPtr.Zero)
            {
                if(attributesInitialized) DeleteProcThreadAttributeList(list);
                Marshal.FreeHGlobal(list);
            }
            foreach(var h in new[]{jobValue,handles,env})if(h!=IntPtr.Zero)Marshal.FreeHGlobal(h);
        }
        return result;
    }
}
