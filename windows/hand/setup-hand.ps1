[CmdletBinding()]
param(
    [ValidateSet("Install", "Repair", "Start", "Stop", "Status", "Uninstall", "Plan")]
    [string]$Action = "Install",

    [string]$InstallDir = $PSScriptRoot,

    [string]$Workspace = $env:USERPROFILE,

    [switch]$SkipLogin,

    [switch]$NoStart,

    [switch]$Service
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($Service -or ($Action -ne "Plan" -and $null -ne (Get-Service -Name "NanocodexHand" -ErrorAction SilentlyContinue))) {
    $parameters = @{} + $PSBoundParameters
    [void]$parameters.Remove("Service")
    & (Join-Path $PSScriptRoot "setup-service.ps1") @parameters
    exit $LASTEXITCODE
}

$taskName = "Nanocodex Hand"
$dataDir = Join-Path $env:LOCALAPPDATA "Nanocodex\Hand"
$accountFile = Join-Path $dataDir "account.json"
$binary = Join-Path $InstallDir "nanocodex2.exe"
$runner = Join-Path $InstallDir "run-hand.ps1"
$powershell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$userSid = $identity.User.Value

function Quote-Argument([string]$Value) {
    if ($Value.Contains('"')) {
        throw "Windows paths containing a double quote are not supported"
    }
    return '"' + $Value + '"'
}

$taskArguments = @(
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy Bypass",
    "-WindowStyle Hidden",
    "-File $(Quote-Argument $runner)",
    "-InstallDir $(Quote-Argument $InstallDir)",
    "-Workspace $(Quote-Argument $Workspace)",
    "-DataDir $(Quote-Argument $dataDir)"
) -join " "

function Get-HandTask {
    Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
}

function Stop-Hand {
    $task = Get-HandTask
    if ($null -ne $task) {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    }
}

function Wait-ForHand([string]$ExpectedState, [int]$Seconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        $task = Get-HandTask
        if ($null -ne $task -and [string]$task.State -eq $ExpectedState) {
            return $true
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Get-AccountStatus {
    $output = & $binary status --account-file $accountFile 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Could not check the Nanocodex account: $($output -join [Environment]::NewLine)"
    }
    try {
        return ($output -join [Environment]::NewLine) | ConvertFrom-Json
    } catch {
        throw "Nanocodex returned an invalid account status"
    }
}

function Assert-Payload {
    foreach ($path in @($binary, $runner)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Nanocodex Hand is incomplete: missing $path"
        }
    }
    & $binary --version | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "nanocodex2.exe could not start"
    }
}

if ($Action -eq "Plan") {
    [pscustomobject]@{
        taskName = $taskName
        userSid = $userSid
        executable = $powershell
        arguments = $taskArguments
        installDir = $InstallDir
        workspace = $Workspace
        dataDir = $dataDir
        accountFile = $accountFile
    } | ConvertTo-Json -Depth 3
    exit 0
}

if ($Action -eq "Stop") {
    Stop-Hand
    Write-Host "Nanocodex Hand is stopped. Use Start Nanocodex Hand when you want it again."
    exit 0
}

if ($Action -eq "Uninstall") {
    Stop-Hand
    if ($null -ne (Get-HandTask)) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
    if (Test-Path -LiteralPath $dataDir) {
        Remove-Item -LiteralPath $dataDir -Recurse -Force
    }
    exit 0
}

if ($Action -eq "Status") {
    $task = Get-HandTask
    $account = if (Test-Path -LiteralPath $binary) { Get-AccountStatus } else { $null }
    [pscustomobject]@{
        installed = $null -ne $task
        running = $null -ne $task -and [string]$task.State -eq "Running"
        authenticated = $null -ne $account -and $account.authenticated -eq $true
        workspace = $Workspace
        log = Join-Path $dataDir "hand.log"
    } | ConvertTo-Json
    exit 0
}

Assert-Payload
# Provision the official per-user OpenAI Sky runtime before starting the Hand.
if ($env:NANOCODEX_COMPUTER -cnotin @("off", "none", "0")) {
    & $binary computer setup --refresh
    if ($LASTEXITCODE -ne 0) {
        throw "OpenAI Sky setup failed; retry nanocodex2 computer setup before starting the Hand."
    }
}

New-Item -ItemType Directory -Force -Path $Workspace, $dataDir | Out-Null

if (-not $SkipLogin) {
    $status = Get-AccountStatus
    if ($status.authenticated -ne $true) {
        Write-Host "Sign in to connect this computer to your Nanocodex account."
        & $binary login --account-file $accountFile --label "Nanocodex Windows Hand"
        if ($LASTEXITCODE -ne 0) {
            throw "Nanocodex sign-in did not finish"
        }
        $status = Get-AccountStatus
        if ($status.authenticated -ne $true) {
            throw "Nanocodex sign-in was not saved"
        }
    }
}

$taskAction = New-ScheduledTaskAction `
    -Execute $powershell `
    -Argument $taskArguments `
    -WorkingDirectory $Workspace
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $userSid
$principal = New-ScheduledTaskPrincipal `
    -UserId $userSid `
    -LogonType Interactive `
    -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew

Stop-Hand
Register-ScheduledTask `
    -TaskName $taskName `
    -Description "Connect this signed-in Windows desktop to its Nanocodex account." `
    -Action $taskAction `
    -Trigger $trigger `
    -Principal $principal `
    -Settings $settings `
    -Force | Out-Null

if (-not $NoStart) {
    Start-ScheduledTask -TaskName $taskName
    if (-not (Wait-ForHand "Running" 15)) {
        $info = Get-ScheduledTaskInfo -TaskName $taskName
        throw "Nanocodex Hand did not remain running (last task result: $($info.LastTaskResult))"
    }
    Write-Host "Nanocodex Hand is connected. You can close this window."
} else {
    Write-Host "Nanocodex Hand is installed and will start the next time you sign in."
}
