param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    # Also enable the Virtual Display Driver and place it left of, then above,
    # the primary. The layout is recorded first and restored afterwards.
    [switch]$WithVirtualDisplay,
    # Also record which raw pen frame sequences Windows rejects, at the
    # centre of the probe window on the primary display.
    [switch]$PenSequences
)
# Checks where Pencil and touch land on a whole monitor, for monitors whose
# desktop origin differs from the virtual screen's. Run it in the interactive
# console session (Invoke-InSession.ps1 -RequireIdle) with pen_probe.exe.
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $EvidenceDirectory | Out-Null
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public sealed class EMDisplay {
    public string Name, Adapter;
    public int X, Y, Width, Height, Dpi;
    public bool Primary;
}
public static class EMLayout {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct DisplayDevice {
        public int cb;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string Name;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Description;
        public int State;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Id;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Key;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct DevMode {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
        public short SpecVersion, DriverVersion, Size, DriverExtra;
        public int Fields, PositionX, PositionY, Orientation, FixedOutput;
        public short Color, Duplex, YResolution, TTOption, Collate;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string FormName;
        public short LogPixels;
        public int BitsPerPel, PelsWidth, PelsHeight, DisplayFlags, DisplayFrequency;
        public int ICMMethod, ICMIntent, MediaType, DitherType, Reserved1, Reserved2, PanningWidth, PanningHeight;
    }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplayDevices(string device, int index, ref DisplayDevice dd, int flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool EnumDisplaySettingsEx(string device, int mode, ref DevMode dm, int flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int ChangeDisplaySettingsEx(string device, ref DevMode dm, IntPtr window, int flags, IntPtr param);
    [DllImport("user32.dll", EntryPoint = "ChangeDisplaySettingsExW")] static extern int ApplyPending(IntPtr device, IntPtr dm, IntPtr window, int flags, IntPtr param);
    [DllImport("user32.dll")] static extern IntPtr MonitorFromPoint(Point point, int flags);
    [DllImport("shcore.dll")] static extern int GetDpiForMonitor(IntPtr monitor, int type, out uint x, out uint y);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
    [DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
    public static void BecomeAware() { SetProcessDpiAwarenessContext(new IntPtr(-4)); }
    public static List<EMDisplay> Read() {
        var displays = new List<EMDisplay>();
        for (int i = 0; ; i++) {
            var device = new DisplayDevice();
            device.cb = Marshal.SizeOf(device);
            if (!EnumDisplayDevices(null, i, ref device, 0)) break;
            if ((device.State & 1) == 0) continue;
            var mode = new DevMode();
            mode.Size = (short)Marshal.SizeOf(mode);
            if (!EnumDisplaySettingsEx(device.Name, -1, ref mode, 0)) continue;
            var display = new EMDisplay {
                Name = device.Name, Adapter = device.Description,
                X = mode.PositionX, Y = mode.PositionY, Width = mode.PelsWidth, Height = mode.PelsHeight,
                Primary = (device.State & 4) != 0
            };
            uint dpiX, dpiY;
            var monitor = MonitorFromPoint(new Point(display.X + display.Width / 2, display.Y + display.Height / 2), 0);
            display.Dpi = monitor != IntPtr.Zero && GetDpiForMonitor(monitor, 0, out dpiX, out dpiY) == 0 ? (int)dpiX : 0;
            displays.Add(display);
        }
        return displays;
    }
    public static int Move(string name, int x, int y) {
        var mode = new DevMode();
        mode.Size = (short)Marshal.SizeOf(mode);
        if (!EnumDisplaySettingsEx(name, -1, ref mode, 0)) return -100;
        mode.Fields = 0x20; // DM_POSITION
        mode.PositionX = x;
        mode.PositionY = y;
        int result = ChangeDisplaySettingsEx(name, ref mode, IntPtr.Zero, 0x1 | 0x10000000, IntPtr.Zero);
        return result != 0 ? result : ApplyPending(IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 0, IntPtr.Zero);
    }
}
public sealed class EMPlacementProbe : Form {
    [StructLayout(LayoutKind.Sequential)] struct PointerInfo {
        public uint type, id, frame, flags;
        public IntPtr source, target;
        public Point pixel, himetric, rawPixel, rawHimetric;
        public uint time, history;
        public int input;
        public uint keys;
        public ulong performance;
        public uint change;
    }
    [StructLayout(LayoutKind.Sequential)] struct PenInfo {
        public PointerInfo pointer;
        public uint flags, mask, pressure, rotation;
        public int tiltX, tiltY;
    }
    [StructLayout(LayoutKind.Sequential)] struct MouseInput { public int x, y; public uint data, flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct Input { public uint type; public MouseInput mouse; }
    [DllImport("user32.dll")] static extern bool GetPointerPenInfo(uint id, out PenInfo info);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] static extern uint SendInput(uint count, Input[] inputs, int size);
    public sealed class Sample { public string Kind; public int Message; public uint Flags; public int X, Y; public long Ms; }
    public readonly List<Sample> Samples = new List<Sample>();
    public readonly List<string> UsedPoints = new List<string>();
    public readonly List<string> SkippedPoints = new List<string>();
    public Rectangle ClientBounds;
    public long ChildStartMs = -1, MovedAtMs = -1;
    public int ChildExit = -1, MoveResult = 0;
    public string ChildOutput = "";
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly System.Text.StringBuilder output = new System.Text.StringBuilder();
    readonly string executable, mode, moveDevice;
    readonly string[] points;
    readonly int steps, moveX, moveY;
    Process child;
    bool focusClickSent;
    long exitedAt = -1;
    public EMPlacementProbe(string exe, Rectangle target, string probeMode, string[] probePoints, int strokeSteps,
                            string moveName, int moveToX, int moveToY) {
        executable = exe; mode = probeMode; points = probePoints; steps = strokeSteps;
        moveDevice = moveName; moveX = moveToX; moveY = moveToY;
        Text = "EternalMonitor input probe";
        FormBorderStyle = FormBorderStyle.None;
        ShowInTaskbar = false;
        StartPosition = FormStartPosition.Manual;
        Bounds = target;
        BackColor = Color.White;
        TopMost = true;
        Shown += delegate { Activate(); Focus(); SetForegroundWindow(Handle); };
        timer.Interval = 20;
        timer.Tick += delegate { Tick(); };
        timer.Start();
    }
    bool Ours(Point point) { return GetAncestor(WindowFromPoint(point), 2) == Handle; }
    void Tick() {
        long now = clock.ElapsedMilliseconds;
        if (now > 60000) { Close(); return; }
        if (child == null) {
            if (now < 1000) return;
            if (GetForegroundWindow() != Handle) {
                Activate(); Focus(); SetForegroundWindow(Handle);
                // Windows can refuse programmatic activation; one click on
                // our own visible form is allowed, as in Test-PenInput.ps1.
                Point centre = PointToScreen(new Point(ClientSize.Width / 2, ClientSize.Height / 2));
                if (!focusClickSent && Ours(centre)) {
                    Cursor.Position = centre;
                    if (Ours(Cursor.Position)) {
                        focusClickSent = true;
                        Input[] click = { new Input { mouse = new MouseInput { flags = 2 } }, new Input { mouse = new MouseInput { flags = 4 } } };
                        SendInput(2, click, Marshal.SizeOf(typeof(Input)));
                    }
                }
                return;
            }
            ClientBounds = RectangleToScreen(ClientRectangle);
            string arguments;
            if (mode == "mapping") {
                // Inject only where this window is the topmost hit target, so
                // a corner never reaches the taskbar or another window.
                foreach (var point in points) {
                    var parts = point.Split(',');
                    long nx = long.Parse(parts[0]), ny = long.Parse(parts[1]);
                    var pixel = new Point(ClientBounds.X + (int)(nx * (ClientBounds.Width - 1) / 65535),
                                          ClientBounds.Y + (int)(ny * (ClientBounds.Height - 1) / 65535));
                    (Ours(pixel) ? UsedPoints : SkippedPoints).Add(point);
                }
                if (UsedPoints.Count == 0) { Close(); return; }
                arguments = String.Join(" ", UsedPoints.ToArray());
            } else if (mode == "stroke") {
                arguments = steps.ToString();
            } else {
                arguments = "";
            }
            child = new Process();
            child.StartInfo = new ProcessStartInfo(executable, String.Format("{0} {1} {2} {3} {4} {5} {6}",
                mode, Process.GetCurrentProcess().Id, ClientBounds.X, ClientBounds.Y, ClientBounds.Width, ClientBounds.Height, arguments));
            child.StartInfo.UseShellExecute = false;
            child.StartInfo.CreateNoWindow = true;
            child.StartInfo.RedirectStandardOutput = true;
            child.StartInfo.RedirectStandardError = true;
            child.OutputDataReceived += delegate(object sender, DataReceivedEventArgs args) { if (args.Data != null) lock (output) output.AppendLine(args.Data); };
            child.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs args) { if (args.Data != null) lock (output) output.AppendLine(args.Data); };
            child.Start();
            child.BeginOutputReadLine();
            child.BeginErrorReadLine();
            ChildStartMs = now;
            return;
        }
        // Move another display while the stroke is held: a display change
        // cancels injected contacts and moves the virtual screen's origin.
        if (moveDevice != null && MovedAtMs < 0 && now - ChildStartMs > 1200) {
            MoveResult = EMLayout.Move(moveDevice, moveX, moveY);
            MovedAtMs = clock.ElapsedMilliseconds;
        }
        if (child.HasExited) {
            if (exitedAt < 0) exitedAt = now;
            if (now - exitedAt > 300) {
                child.WaitForExit();
                ChildExit = child.ExitCode;
                lock (output) ChildOutput = output.ToString();
                Close();
            }
        }
    }
    protected override void WndProc(ref Message message) {
        if (message.Msg >= 0x245 && message.Msg <= 0x247) {
            PenInfo info;
            if (GetPointerPenInfo((uint)(message.WParam.ToInt64() & 0xffff), out info)) {
                Samples.Add(new Sample { Kind = "pen", Message = message.Msg, Flags = info.pointer.flags,
                    X = info.pointer.pixel.X, Y = info.pointer.pixel.Y, Ms = clock.ElapsedMilliseconds });
                message.Result = IntPtr.Zero;
                return;
            }
        }
        if (message.Msg == 0x201 && ChildStartMs >= 0) {
            long lParam = message.LParam.ToInt64();
            Point screen = PointToScreen(new Point((short)(lParam & 0xffff), (short)((lParam >> 16) & 0xffff)));
            Samples.Add(new Sample { Kind = "mouse", Message = message.Msg, X = screen.X, Y = screen.Y, Ms = clock.ElapsedMilliseconds });
        }
        base.WndProc(ref message);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop();
        if (child != null) { if (!child.HasExited) child.Kill(); child.Dispose(); }
        base.OnFormClosed(e);
    }
    public static EMPlacementProbe Run(string exe, Rectangle target, string mode, string[] points, int steps,
                                       string moveDevice, int moveX, int moveY) {
        var probe = new EMPlacementProbe(exe, target, mode, points, steps, moveDevice, moveX, moveY);
        Application.Run(probe);
        return probe;
    }
}
'@

[EMLayout]::BecomeAware()
# Keep the display and system awake for the run.
[EMLayout]::SetThreadExecutionState([uint32]2147483651) | Out-Null
function Get-Layout {
    @([EMLayout]::Read() | ForEach-Object {
        [pscustomobject]@{ name = $_.Name; adapter = $_.Adapter; x = $_.X; y = $_.Y;
            width = $_.Width; height = $_.Height; dpi = $_.Dpi; primary = $_.Primary }
    })
}
function Get-VirtualScreen {
    [pscustomobject]@{ left = [EMLayout]::GetSystemMetrics(76); top = [EMLayout]::GetSystemMetrics(77);
        width = [EMLayout]::GetSystemMetrics(78); height = [EMLayout]::GetSystemMetrics(79) }
}
function Get-Vdd { Get-Layout | Where-Object { $_.adapter -like '*Virtual Display*' } | Select-Object -First 1 }
function Wait-Vdd([bool]$Present) {
    $deadline = (Get-Date).AddSeconds(30)
    while ([bool](Get-Vdd) -ne $Present) {
        if ((Get-Date) -gt $deadline) { throw "Virtual display present=$(!$Present) after 30 s" }
        Start-Sleep -Milliseconds 500
    }
    Start-Sleep -Seconds 2
}
function Move-Display($Display, [int]$X, [int]$Y) {
    $result = [EMLayout]::Move($Display.name, $X, $Y)
    if ($result -ne 0) { throw "Moving $($Display.name) to $X,$Y failed with $result" }
    Start-Sleep -Seconds 2
}
function Expected($Bounds, [long]$Nx, [long]$Ny) {
    [pscustomobject]@{ x = $Bounds.X + [int][math]::Floor($Nx * ($Bounds.Width - 1) / 65535)
                       y = $Bounds.Y + [int][math]::Floor($Ny * ($Bounds.Height - 1) / 65535) }
}
$points = @('0,0', '65535,0', '0,65535', '65535,65535', '32768,32768')
$cases = New-Object System.Collections.ArrayList
function Invoke-Case([string]$Name, $Display, [string]$Mode = 'mapping', $MoveDisplay = $null, [int]$MoveX = 0, [int]$MoveY = 0) {
    $target = New-Object System.Drawing.Rectangle $Display.x, $Display.y, $Display.width, $Display.height
    $screenBefore = Get-VirtualScreen
    $probe = [EMPlacementProbe]::Run($Executable, $target, $Mode, $points, 120,
        $(if ($MoveDisplay) { $MoveDisplay.name } else { $null }), $MoveX, $MoveY)
    $bounds = $probe.ClientBounds
    $directory = Join-Path $EvidenceDirectory ($Name -replace '[^A-Za-z0-9]+', '-')
    New-Item -ItemType Directory -Force $directory | Out-Null
    $probe.ChildOutput | Set-Content (Join-Path $directory 'injector.log')
    $probe.Samples | ConvertTo-Json -Compress | Set-Content (Join-Path $directory 'samples.json')
    $rejected = $probe.ChildOutput -match 'rejected|unavailable|guard'
    $case = [ordered]@{ name = $Name; mode = $Mode; display = $Display.name; dpi = $Display.dpi
        monitor = "$($Display.x),$($Display.y) $($Display.width)x$($Display.height)"
        window = "$($bounds.X),$($bounds.Y) $($bounds.Width)x$($bounds.Height)"
        virtual_screen = $screenBefore; child_exit = $probe.ChildExit; injector_warnings = [bool]$rejected
        skipped_points = @($probe.SkippedPoints) }
    if ($Mode -eq 'mapping') {
        $pen = @($probe.Samples | Where-Object { $_.Kind -eq 'pen' -and $_.Message -eq 0x246 })
        $mouse = @($probe.Samples | Where-Object { $_.Kind -eq 'mouse' })
        $rows = @(); $ok = $probe.ChildExit -eq 0 -and $pen.Count -eq $probe.UsedPoints.Count -and $mouse.Count -eq $probe.UsedPoints.Count
        for ($i = 0; $i -lt $probe.UsedPoints.Count; $i++) {
            $n = $probe.UsedPoints[$i] -split ','
            $want = Expected $bounds $n[0] $n[1]
            $penAt = if ($i -lt $pen.Count) { "$($pen[$i].X),$($pen[$i].Y)" } else { 'none' }
            $mouseAt = if ($i -lt $mouse.Count) { "$($mouse[$i].X),$($mouse[$i].Y)" } else { 'none' }
            $ok = $ok -and $penAt -eq "$($want.x),$($want.y)" -and $mouseAt -eq "$($want.x),$($want.y)"
            $rows += [ordered]@{ point = $probe.UsedPoints[$i]; expected = "$($want.x),$($want.y)"; pen = $penAt; mouse = $mouseAt }
        }
        $case.points = $rows
        $case.passed = $ok -and !$rejected
    } elseif ($Mode -eq 'sequences') {
        $results = [ordered]@{}
        foreach ($line in ($probe.ChildOutput -split "`r?`n" | Where-Object { $_ -like 'SEQ *' })) {
            $parts = $line -split ' '
            $results[$parts[1]] = @($results[$parts[1]]) + $parts[3] | Where-Object { $_ }
        }
        $case.sequences = $results
        $case.passed = $probe.ChildExit -eq 0 -and
            (@($results['control_stroke']) -join ',') -eq 'ok,ok,ok' -and
            (@($results['control_hover_then_down_elsewhere']) -join ',') -eq 'ok,ok,ok'
    } else {
        $y = (Expected $bounds 0 32768).y
        $allowed = @{}
        for ($step = 0; $step -lt 120; $step++) { $allowed[(Expected $bounds (6000 + $step * [math]::Floor(54000 / 119)) 0).x] = $true }
        $pen = @($probe.Samples | Where-Object { $_.Kind -eq 'pen' -and $_.Message -ne 0x247 })
        $misplaced = @($pen | Where-Object { $_.Y -ne $y -or !$allowed.ContainsKey($_.X) })
        $after = @($pen | Where-Object { $probe.MovedAtMs -ge 0 -and $_.Ms -gt $probe.MovedAtMs + 300 })
        $case.moved_to = "$MoveX,$MoveY"; $case.move_result = $probe.MoveResult
        $case.virtual_screen_after = Get-VirtualScreen
        $case.samples = $pen.Count; $case.samples_after_change = $after.Count; $case.misplaced = $misplaced.Count
        $case.downs = @($pen | Where-Object { $_.Message -eq 0x246 }).Count
        $case.passed = $probe.ChildExit -eq 0 -and $probe.MoveResult -eq 0 -and $pen.Count -gt 20 -and
            $misplaced.Count -eq 0 -and $after.Count -gt 10
    }
    [void]$cases.Add([pscustomobject]$case)
}

$original = Get-Layout
$report = [ordered]@{ original_layout = $original; original_virtual_screen = Get-VirtualScreen; cases = $cases }
$vddWasPresent = [bool](Get-Vdd)
$remembered = $null
$restoreErrors = @()
try {
    $primary = $original | Where-Object { $_.primary } | Select-Object -First 1
    Invoke-Case 'current layout, primary' $primary
    foreach ($other in @($original | Where-Object { !$_.primary })) {
        Invoke-Case "current layout, $($other.name)" $other
    }
    if ($PenSequences) {
        Invoke-Case 'raw pen sequences, primary' $primary 'sequences'
    }
    if ($WithVirtualDisplay) {
        if (!$vddWasPresent) {
            & schtasks /Run /TN 'EternalMonitor VDD Enable' | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Could not start the VDD enable task' }
            Wait-Vdd $true
        }
        # Windows places a returning display where it was last; put it back there.
        $remembered = Get-Vdd
        $report.virtual_display_initial = $remembered
        $vdd = $remembered
        Move-Display $vdd (-$vdd.width) 0
        $report.layout_vdd_left = Get-Layout
        $vdd = Get-Vdd; $primary = Get-Layout | Where-Object { $_.primary } | Select-Object -First 1
        Invoke-Case 'virtual display left, virtual display' $vdd
        Invoke-Case 'virtual display left, primary' $primary
        Move-Display $vdd -200 (-$vdd.height)
        $report.layout_vdd_above = Get-Layout
        $vdd = Get-Vdd; $primary = Get-Layout | Where-Object { $_.primary } | Select-Object -First 1
        Invoke-Case 'virtual display above, virtual display' $vdd
        Invoke-Case 'virtual display above, primary' $primary
        Invoke-Case 'stroke on primary while the virtual display moves left' $primary 'stroke' $vdd (-$vdd.width) 0
    }
} finally {
    try {
        if ($remembered) { Move-Display $remembered $remembered.x $remembered.y }
    } catch { $restoreErrors += "$_" }
    try {
        if ($WithVirtualDisplay -and !$vddWasPresent -and (Get-Vdd)) {
            & schtasks /Run /TN 'EternalMonitor VDD Disable' | Out-Null
            Wait-Vdd $false
        }
    } catch { $restoreErrors += "$_" }
    $final = Get-Layout
    $key = { param($l) ($l | Sort-Object name | ForEach-Object { "$($_.name)|$($_.x)|$($_.y)|$($_.width)|$($_.height)|$($_.primary)" }) -join ';' }
    $report.final_layout = $final
    $report.layout_restored = (& $key $original) -eq (& $key $final)
    $report.restore_errors = $restoreErrors
    [EMLayout]::SetThreadExecutionState([uint32]2147483648) | Out-Null
    $report | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $EvidenceDirectory 'result.json')
}
$failed = @($cases | Where-Object { !$_.passed })
Get-Content (Join-Path $EvidenceDirectory 'result.json')
if ($failed.Count -gt 0 -or !$report.layout_restored) {
    throw "Pen mapping check failed: $($failed.name -join '; '); layout restored=$($report.layout_restored)"
}
