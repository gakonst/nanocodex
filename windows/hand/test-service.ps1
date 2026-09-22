[CmdletBinding()]
param([string]$InstallDir = "$env:ProgramFiles\Nanocodex Hand")

# Run elevated against an installed, authenticated Hand on a test PC.
# Exercises process death and stop/start; it keeps credentials and finishes running.
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$serviceName = "NanocodexHand"
$binary = Join-Path $InstallDir "nanocodex2.exe"
function Get-Worker {
    @(Get-CimInstance Win32_Process -Filter "Name='nanocodex2.exe'" | Where-Object {
        $_.ExecutablePath -eq $binary -and $_.CommandLine -match '(?:^|\s)hand(?:\s|$)'
    })
}
function Wait-Worker([int]$PreviousId = 0) {
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        $workers = @(Get-Worker | Where-Object { $_.ProcessId -ne $PreviousId })
        if ($workers.Count -eq 1 -and $workers[0].SessionId -gt 0) { return $workers[0] }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Expected one replacement Hand in a signed-in desktop session"
}
$service = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
if ($null -eq $service -or $service.StartMode -ne "Auto") { throw "Automatic service not installed" }
Start-Service $serviceName
$initial = Wait-Worker
try {
    Stop-Process -Id $initial.ProcessId -Force
    $recovered = Wait-Worker $initial.ProcessId
    # SCM must recover the actual supervisor, including its worker process tree.
    $supervisor = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
    Stop-Process -Id $supervisor.ProcessId -Force
    $afterSupervisor = Wait-Worker $recovered.ProcessId
    $newSupervisor = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
    if ($newSupervisor.ProcessId -eq $supervisor.ProcessId) { throw "Supervisor did not restart" }

    Stop-Service $serviceName
    (Get-Service $serviceName).WaitForStatus("Stopped", [TimeSpan]::FromSeconds(30))
    if (@(Get-Worker).Count -ne 0) { throw "Stop left a Hand worker running" }
    foreach ($old in @($initial, $recovered, $afterSupervisor)) {
        if (Get-Process -Id $old.ProcessId -ErrorAction SilentlyContinue) { throw "Stale worker survived: $($old.ProcessId)" }
    }
    Start-Service $serviceName
    $final = Wait-Worker
    [pscustomobject]@{
        automaticStartup = $true
        workerCrashRecovered = $true
        supervisorCrashRecovered = $true
        stopRemovedWorkers = $true
        finalWorkerSession = $final.SessionId
        finalWorkerPid = $final.ProcessId
    } | ConvertTo-Json
} finally {
    if ((Get-Service $serviceName).Status -ne "Running") { Start-Service $serviceName }
}
