// The SCM supervisor has no account credential. It starts the existing Hand
// runner with the signed-in user's token, so capture/input never runs in Session 0.
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.ServiceProcess;
using System.Text;
using System.Threading;
using System.Xml;

internal sealed class HandService : ServiceBase
{
    private readonly ManualResetEvent stopping = new ManualResetEvent(false);
    private Thread supervisor;
    private IntPtr job;
    private Process worker;
    private int session = -1;
    private string userSid, arguments, workspace;
    private readonly string directory = AppDomain.CurrentDomain.BaseDirectory;
    private readonly string log = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "service.log");

    public HandService() { ServiceName = "NanocodexHand"; CanShutdown = true; }
    public static void Main() { ServiceBase.Run(new HandService()); }
    protected override void OnStart(string[] args)
    {
        var config = new XmlDocument { XmlResolver = null };
        config.Load(Path.Combine(directory, "hand-service.xml"));
        var root = config.DocumentElement;
        userSid = root.GetAttribute("userSid");
        workspace = root.GetAttribute("workspace");
        if (String.IsNullOrEmpty(userSid) || !Directory.Exists(workspace)) throw new InvalidDataException("Invalid Hand service configuration");
        arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File " + Quote(Path.Combine(directory, "run-hand.ps1"))
            + " -InstallDir " + Quote(directory.TrimEnd('\\')) + " -Workspace " + Quote(workspace) + " -DataDir " + Quote(root.GetAttribute("dataDir"));
        stopping.Reset();
        supervisor = new Thread(Supervise) { IsBackground = true, Name = "Nanocodex Hand supervisor" };
        supervisor.Start();
    }
    protected override void OnStop()
    {
        stopping.Set();
        // Token lookup and CreateProcessAsUser are synchronous and bounded by Windows.
        if (supervisor != null && !supervisor.Join(TimeSpan.FromSeconds(20)))
            throw new System.TimeoutException("Hand supervisor did not stop");
    }
    protected override void OnShutdown() { OnStop(); }
    private static string Quote(string value)
    {
        if (String.IsNullOrEmpty(value) || value.IndexOfAny(new[] {'"', '\r', '\n', '\0'}) >= 0)
            throw new InvalidDataException("Invalid Hand path");
        return "\"" + System.Text.RegularExpressions.Regex.Replace(value, @"(\\+)$", "$1$1") + "\"";
    }
    private void Log(string message)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(log));
            if (File.Exists(log) && new FileInfo(log).Length > 1024 * 1024)
            {
                File.Delete(log + ".1");
                File.Move(log, log + ".1");
            }
            File.AppendAllText(log, DateTime.UtcNow.ToString("o") + " " + message + Environment.NewLine);
        }
        catch { /* Logging cannot stop supervision. Never log credentials. */ }
    }
    private void Supervise()
    {
        int failures = 0;
        try
        {
            Log("Supervisor started; waiting for the configured Windows user.");
            while (!stopping.WaitOne(0))
            {
                IntPtr token = IntPtr.Zero;
                try
                {
                    int selected = FindSession(out token);
                    if (worker != null && (worker.HasExited || selected != session)) StopWorker();
                    if (selected >= 0 && worker == null)
                    {
                        StartWorker(selected, token);
                        failures = 0;
                        Log("Hand worker started in interactive session " + selected);
                    }
                }
                catch (Exception error)
                {
                    StopWorker();
                    failures = Math.Min(failures + 1, 10);
                    Log("Worker retry: " + error.GetType().Name + ": " + error.Message);
                }
                finally { if (token != IntPtr.Zero) CloseHandle(token); }
                stopping.WaitOne(TimeSpan.FromSeconds(Math.Max(3, failures * 3)));
            }
        }
        finally { StopWorker(); Log("Supervisor stopped."); }
    }
    private int FindSession(out IntPtr selectedToken)
    {
        selectedToken = IntPtr.Zero;
        IntPtr sessions;
        int count;
        if (!WTSEnumerateSessions(IntPtr.Zero, 0, 1, out sessions, out count)) throw new Win32Exception();
        int selected = -1, rank = -1;
        try
        {
            int size = Marshal.SizeOf(typeof(SessionInfo));
            for (int i = 0; i < count; i++)
            {
                var item = (SessionInfo)Marshal.PtrToStructure(IntPtr.Add(sessions, i * size), typeof(SessionInfo));
                // WTSActive=0, WTSDisconnected=4. Never use the service session.
                if (item.Id == 0 || (item.State != 0 && item.State != 4)) continue;
                IntPtr token;
                if (!WTSQueryUserToken(item.Id, out token)) continue;
                bool keep = false;
                try
                {
                    using (var identity = new WindowsIdentity(token))
                    {
                        int candidateRank = item.State == 0 ? 2 : (item.Id == session ? 1 : 0);
                        if (identity.User.Value == userSid && candidateRank > rank)
                        {
                            if (selectedToken != IntPtr.Zero) CloseHandle(selectedToken);
                            selectedToken = token; selected = item.Id; rank = candidateRank; keep = true;
                        }
                    }
                }
                finally { if (!keep) CloseHandle(token); }
            }
            return selected;
        }
        catch { if (selectedToken != IntPtr.Zero) CloseHandle(selectedToken); selectedToken = IntPtr.Zero; throw; }
        finally { WTSFreeMemory(sessions); }
    }
    private void StartWorker(int id, IntPtr token)
    {
        IntPtr environment = IntPtr.Zero;
        var info = new ProcessInformation();
        try
        {
            if (!CreateEnvironmentBlock(out environment, token, false)) throw new Win32Exception();
            job = CreateJobObject(IntPtr.Zero, null);
            if (job == IntPtr.Zero) throw new Win32Exception();
            var limits = new ExtendedLimits();
            limits.Basic.LimitFlags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            int size = Marshal.SizeOf(limits);
            IntPtr buffer = Marshal.AllocHGlobal(size);
            try
            {
                Marshal.StructureToPtr(limits, buffer, false);
                if (!SetInformationJobObject(job, 9, buffer, (uint)size)) throw new Win32Exception();
            }
            finally { Marshal.FreeHGlobal(buffer); }
            string executable = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "WindowsPowerShell", "v1.0", "powershell.exe");
            var startup = new StartupInfo { Size = Marshal.SizeOf(typeof(StartupInfo)), Desktop = "winsta0\\default", Flags = 1, ShowWindow = 0 };
            if (!CreateProcessAsUser(token, executable, new StringBuilder(Quote(executable) + " " + arguments), IntPtr.Zero, IntPtr.Zero, false,
                0x08000404, environment, workspace, ref startup, out info)) throw new Win32Exception(); // no window, Unicode env, suspended
            if (!AssignProcessToJobObject(job, info.Process)) throw new Win32Exception();
            worker = Process.GetProcessById(info.ProcessId);
            session = id;
            if (ResumeThread(info.Thread) == UInt32.MaxValue) throw new Win32Exception();
        }
        catch { if (info.Process != IntPtr.Zero) TerminateProcess(info.Process, 1); StopWorker(); throw; }
        finally
        {
            if (environment != IntPtr.Zero) DestroyEnvironmentBlock(environment);
            if (info.Process != IntPtr.Zero) CloseHandle(info.Process);
            if (info.Thread != IntPtr.Zero) CloseHandle(info.Thread);
        }
    }
    private void StopWorker()
    {
        if (job != IntPtr.Zero) { TerminateJobObject(job, 0); CloseHandle(job); job = IntPtr.Zero; }
        if (worker != null) { worker.Dispose(); worker = null; }
        session = -1;
    }
    [StructLayout(LayoutKind.Sequential)] private struct SessionInfo { public int Id; public IntPtr Name; public int State; }
    [StructLayout(LayoutKind.Sequential)] private struct ProcessInformation { public IntPtr Process, Thread; public int ProcessId, ThreadId; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] private struct StartupInfo
    {
        public int Size; public string Reserved, Desktop, Title; public int X, Y, XSize, YSize, XCountChars, YCountChars, FillAttribute, Flags;
        public short ShowWindow, ReservedSize; public IntPtr ReservedBytes, StdInput, StdOutput, StdError;
    }
    [StructLayout(LayoutKind.Sequential)] private struct BasicLimits
    {
        public long ProcessTime, JobTime; public uint LimitFlags; public UIntPtr MinimumWorkingSet, MaximumWorkingSet;
        public uint ActiveProcessLimit; public UIntPtr Affinity; public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)] private struct IoCounters { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
    [StructLayout(LayoutKind.Sequential)] private struct ExtendedLimits
    {
        public BasicLimits Basic; public IoCounters Io; public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }
    [DllImport("wtsapi32.dll", SetLastError = true)] private static extern bool WTSEnumerateSessions(IntPtr server, int reserved, int version, out IntPtr sessions, out int count);
    [DllImport("wtsapi32.dll", SetLastError = true)] private static extern bool WTSQueryUserToken(int session, out IntPtr token);
    [DllImport("wtsapi32.dll")] private static extern void WTSFreeMemory(IntPtr memory);
    [DllImport("userenv.dll", SetLastError = true)] private static extern bool CreateEnvironmentBlock(out IntPtr environment, IntPtr token, bool inherit);
    [DllImport("userenv.dll")] private static extern bool DestroyEnvironmentBlock(IntPtr environment);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool CreateProcessAsUser(IntPtr token, string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inherit, uint flags, IntPtr environment, string directory, ref StartupInfo startup, out ProcessInformation process);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool SetInformationJobObject(IntPtr job, int infoClass, IntPtr info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll")] private static extern bool TerminateJobObject(IntPtr job, uint code);
    [DllImport("kernel32.dll")] private static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
}
