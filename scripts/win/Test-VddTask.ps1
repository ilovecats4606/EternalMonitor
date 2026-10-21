# Pure script checks for host/src/vdd_task.ps1. Task Scheduler is replaced in
# a child process, so no real task runs. The host runs the script the same way:
# `& { <script> } -Task <name> -After <other direction>`.
$ErrorActionPreference = 'Stop'
$script = Get-Content -Raw (Join-Path $PSScriptRoot '..\..\host\src\vdd_task.ps1')
$mock = @'
# $EMBusy[name]: how many more state reads report that task as running.
function New-Object {
    param([string]$ComObject, [Parameter(ValueFromRemainingArguments=$true)]$Rest)
    if ($ComObject -ne 'Schedule.Service') { throw "Unexpected COM object $ComObject" }
    $service = [pscustomobject]@{}
    $service | Add-Member ScriptMethod Connect {}
    $service | Add-Member ScriptMethod GetFolder {
        param($path)
        $folder = [pscustomobject]@{}
        $folder | Add-Member ScriptMethod GetTask {
            param($name)
            $task = [pscustomobject]@{ Name = $name; Enabled = $true; LastTaskResult = 0
                Definition = [pscustomobject]@{ Settings = [pscustomobject]@{ AllowDemandStart = $true } } }
            $task | Add-Member ScriptProperty State {
                if ($global:EMBusy[$this.Name] -gt 0) { $global:EMBusy[$this.Name]--; 4 } else { 3 }
            }
            $task | Add-Member ScriptMethod Run {
                param($arguments)
                $busy = ($global:EMBusy.Keys | Where-Object { $global:EMBusy[$_] -gt 0 }) -join ','
                [Console]::Out.WriteLine("RUN $($this.Name) busy=[$busy]")
                $running = [pscustomobject]@{ State = 3 }
                $running | Add-Member ScriptMethod Refresh {}
                $running
            }
            $task
        }
        $folder
    }
    $service
}
'@

function Invoke-Case([hashtable]$busy, [string]$arguments) {
    $file = Join-Path $env:TEMP ('em-vdd-task-test-' + [guid]::NewGuid().ToString('N') + '.ps1')
    $busyText = ($busy.GetEnumerator() | ForEach-Object { "'$($_.Key)' = $($_.Value)" }) -join '; '
    $body = "`$global:EMBusy = @{ $busyText }`n$mock`n& {`n$script`n} $arguments`n"
    Set-Content -Path $file -Value $body -Encoding UTF8
    # The child's stderr is expected output here, not a failure of this script.
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $file 2>&1 | Out-String
        [pscustomobject]@{ Code = $LASTEXITCODE; Output = $output }
    } finally { Remove-Item $file -ErrorAction SilentlyContinue }
}

# A Disable waits for an Enable still running in Task Scheduler, then runs.
$case = Invoke-Case @{ Enable = 20 } "-Task 'Disable' -After 'Enable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable busy=\[\]') {
    throw "Disable did not wait for the running Enable: $($case.Output)"
}
# It also still waits for an earlier Disable.
$case = Invoke-Case @{ Disable = 20 } "-Task 'Disable' -After 'Enable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable busy=\[\]') {
    throw "Disable did not wait for the earlier Disable: $($case.Output)"
}
# An Enable that never finishes ends the wait within its bound, without
# starting the Disable, and reads as a timeout to the host.
$started = Get-Date
$case = Invoke-Case @{ Enable = 1000000 } "-Task 'Disable' -After 'Enable'"
$took = ((Get-Date) - $started).TotalSeconds
if ($case.Code -ne 1 -or $case.Output -notmatch 'Previous VDD task timed out: Enable' -or $case.Output -match 'RUN') {
    throw "A hung Enable was not reported as a timeout: $($case.Output)"
}
if ($took -gt 25) { throw "The wait took $took s" }
# Without -After, as older hosts call it, only this task is waited for.
$case = Invoke-Case @{ Enable = 1000000 } "-Task 'Disable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable') {
    throw "A call without -After waited for another task: $($case.Output)"
}
Write-Output 'VDD task checks passed: waits for the other direction, earlier runs, hung Enable bound, no -After'
