[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script = Join-Path $PSScriptRoot "setup-hand.ps1"
$workspace = Join-Path $env:RUNNER_TEMP "Nanocodex Hand Workspace"
$plan = & $script -Action Plan -InstallDir $InstallDir -Workspace $workspace | ConvertFrom-Json

if ($plan.taskName -ne "Nanocodex Hand") { throw "Unexpected task name" }
if ($plan.workspace -ne $workspace) { throw "Workspace was not preserved" }
if ($plan.executable -notlike "*WindowsPowerShell*v1.0*powershell.exe") { throw "Task does not use inbox Windows PowerShell" }
if ($plan.arguments -notlike "*run-hand.ps1*") { throw "Task does not use the Hand runner" }
if ($plan.arguments -notlike "*-WindowStyle Hidden*") { throw "Task would flash a console at sign-in" }
if ($plan.arguments -notlike "*-Workspace*$workspace*") { throw "Task lost the selected workspace" }
if ($plan.accountFile -notlike "*Nanocodex*Hand*account.json") { throw "Account state is not isolated to the Hand" }

try {
    & (Join-Path $PSScriptRoot "run-hand.ps1") -InstallDir $InstallDir -Workspace $workspace -DataDir $plan.dataDir -Validate | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Installed runner arguments do not match the bundled Hand CLI" }
    & $script `
        -Action Install `
        -InstallDir $InstallDir `
        -Workspace $workspace `
        -SkipLogin `
        -NoStart
    if ($LASTEXITCODE -ne 0) { throw "Task installation failed" }

    $status = & $script -Action Status -InstallDir $InstallDir -Workspace $workspace | ConvertFrom-Json
    if ($status.installed -ne $true) { throw "Installed task was not found" }
    if ($status.running -ne $false) { throw "NoStart unexpectedly ran the Hand" }
    if ($status.authenticated -ne $false) { throw "Test install unexpectedly used an account" }

    $task = Get-ScheduledTask -TaskName "Nanocodex Hand" -ErrorAction Stop
    if ($task.Settings.RestartCount -ne 999) { throw "Task does not recover from process failures" }
    if ([string]$task.Principal.RunLevel -ne "Limited") { throw "Task unexpectedly requests elevation" }
    if ($task.Actions.Execute -notlike "*WindowsPowerShell*v1.0*powershell.exe") { throw "Task action changed" }
} finally {
    & $script -Action Uninstall -InstallDir $InstallDir -Workspace $workspace
}

if ($null -ne (Get-ScheduledTask -TaskName "Nanocodex Hand" -ErrorAction SilentlyContinue)) {
    throw "Task remained registered after uninstall"
}
if (Test-Path -LiteralPath $plan.dataDir) { throw "Private Hand state remained after uninstall" }

Write-Host "Windows Hand install, recovery, and uninstall lifecycle is valid for the current user."
