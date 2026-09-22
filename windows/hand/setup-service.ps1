[CmdletBinding()]
param(
    [ValidateSet("Install", "Repair", "Start", "Stop", "Status", "Uninstall", "Plan")]
    [string]$Action = "Install",
    [string]$InstallDir = $PSScriptRoot,
    [string]$Workspace = $env:USERPROFILE,
    [switch]$SkipLogin,
    [switch]$NoStart,
    [string]$OriginatingUserSid
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$serviceName = "NanocodexHand"
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$userSid = $identity.User.Value
$dataDir = Join-Path $env:LOCALAPPDATA "Nanocodex\Hand"
$accountFile = Join-Path $dataDir "account.json"
$binary = Join-Path $InstallDir "nanocodex2.exe"
$serviceBinary = Join-Path $InstallDir "nanocodex-hand-service.exe"
$configFile = Join-Path $InstallDir "hand-service.xml"
$powershell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
$serviceLogDir = $InstallDir

function Quote-Argument([string]$Value) {
    if ($Value.IndexOfAny([char[]]"`"`r`n`0") -ge 0) { throw "Invalid argument" }
    return '"' + [regex]::Replace($Value, '(\\+)$', '$1$1') + '"'
}
function Get-HandService { Get-Service -Name $serviceName -ErrorAction SilentlyContinue }
function Stop-HandService {
    $service = Get-HandService
    if ($null -ne $service -and $service.Status -ne "Stopped") {
        Stop-Service -Name $serviceName -Force
        $service.WaitForStatus("Stopped", [TimeSpan]::FromSeconds(30))
    }
}
function Assert-NoRedirect([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path)
    while (-not [string]::IsNullOrEmpty($current)) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction SilentlyContinue
        if ($null -ne $item) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing redirected path: $current" }
            if (-not $item.PSIsContainer -and $item.LinkType -eq "HardLink") { throw "Refusing hard-linked payload: $current" }
        }
        $current = [IO.Path]::GetDirectoryName($current)
    }
}
function Protect-Directory([string]$Path, [string]$OwnerSid, [switch]$PublicRead) {
    Assert-NoRedirect $Path
    # Lock each directory before enumerating its children. Never recurse through
    # a junction before checking it, or preserve a previous explicit writable ACE.
    $item = Get-Item -LiteralPath $Path -Force
    $acl = if ($item.PSIsContainer) { New-Object Security.AccessControl.DirectorySecurity } else { New-Object Security.AccessControl.FileSecurity }
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner((New-Object Security.Principal.SecurityIdentifier($OwnerSid)))
    foreach ($sid in @($OwnerSid, "S-1-5-18", "S-1-5-32-544") | Select-Object -Unique) {
        $principal = New-Object Security.Principal.SecurityIdentifier($sid)
        $rule = if ($item.PSIsContainer) { New-Object Security.AccessControl.FileSystemAccessRule($principal, "FullControl", "ContainerInherit,ObjectInherit", "None", "Allow") } else { New-Object Security.AccessControl.FileSystemAccessRule($principal, "FullControl", "Allow") }
        $acl.AddAccessRule($rule)
    }
    if ($PublicRead) {
        $principal = New-Object Security.Principal.SecurityIdentifier("S-1-5-32-545")
        $rule = if ($item.PSIsContainer) { New-Object Security.AccessControl.FileSystemAccessRule($principal, "ReadAndExecute", "ContainerInherit,ObjectInherit", "None", "Allow") } else { New-Object Security.AccessControl.FileSystemAccessRule($principal, "ReadAndExecute", "Allow") }
        $acl.AddAccessRule($rule)
    }
    Set-Acl -LiteralPath $item.FullName -AclObject $acl
    if ($item.PSIsContainer) {
        foreach ($child in @(Get-ChildItem -LiteralPath $Path -Force)) {
            Protect-Directory $child.FullName $OwnerSid -PublicRead:$PublicRead
        }
    }
}

if ($Action -eq "Plan") {
    [pscustomobject]@{ mode = "Service"; serviceName = $serviceName; userSid = $userSid; installDir = $InstallDir; workspace = $Workspace; dataDir = $dataDir; accountFile = $accountFile; supervisor = $serviceBinary; workerRequiresUserSession = $true } | ConvertTo-Json
    exit 0
}
if ($Action -eq "Status") {
    $service = Get-HandService
    $workers = @(Get-CimInstance Win32_Process -Filter "Name='nanocodex2.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ExecutablePath -eq $binary -and $_.CommandLine -match '(?:^|\s)hand(?:\s|$)' })
    [pscustomobject]@{
        mode = "Service"
        installed = $null -ne $service
        running = $null -ne $service -and $service.Status -eq "Running"
        workerRunning = $workers.Count -gt 0
        workerSessions = @($workers | ForEach-Object { $_.SessionId })
        workerRequiresUserSession = $true
        log = Join-Path $dataDir "hand.log"
        serviceLog = Join-Path $serviceLogDir "service.log"
    } | ConvertTo-Json
    exit 0
}

$administrator = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $administrator.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $arguments = "-NoProfile -ExecutionPolicy Bypass -File $(Quote-Argument $PSCommandPath) -Action $Action -InstallDir $(Quote-Argument $InstallDir) -Workspace $(Quote-Argument $Workspace)"
    $arguments += " -OriginatingUserSid $userSid"
    if ($SkipLogin) { $arguments += " -SkipLogin" }
    if ($NoStart) { $arguments += " -NoStart" }
    $elevated = Start-Process -FilePath $powershell -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    exit $elevated.ExitCode
}
if ($Action -eq "Stop") { Stop-HandService; Write-Host "Nanocodex Hand stopped."; exit 0 }
if ($Action -eq "Uninstall") {
    Stop-HandService
    if ($null -ne (Get-HandService)) {
        & sc.exe delete $serviceName | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not remove Nanocodex Hand service" }
    }
    # Remove only this install's configured user's dedicated Hand state.
    if (Test-Path -LiteralPath $configFile) {
        [xml]$saved = Get-Content -LiteralPath $configFile -Raw
        $savedData = [string]$saved.hand.dataDir
        if ($savedData -eq $dataDir -and (Test-Path -LiteralPath $dataDir)) {
            Remove-Item -LiteralPath $dataDir -Recurse -Force
        }
        Remove-Item -LiteralPath $configFile -Force
    }
    exit 0
}
if ($Action -eq "Start" -and $null -ne (Get-HandService)) {
    Start-Service -Name $serviceName
    Write-Host "Nanocodex Hand service started. The Hand connects when its Windows user is signed in."
    exit 0
}

if ($OriginatingUserSid -and $OriginatingUserSid -ne $userSid) {
    throw "Sign in with an administrator Windows account before installing the Hand. Using a different account at the UAC prompt is not supported."
}
if (Test-Path -LiteralPath $configFile) {
    [xml]$saved = Get-Content -LiteralPath $configFile -Raw
    if ([string]$saved.hand.userSid -ne $userSid) { throw "Repair must run as the Windows user who configured this Hand." }
}
# A privileged supervisor must never execute files from a user-writable install.
$InstallDir = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
$programFiles = [IO.Path]::GetFullPath($env:ProgramFiles).TrimEnd('\') + '\'
if (-not [string]::Equals([IO.Path]::GetDirectoryName($InstallDir), $programFiles.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw "Always-on service installation requires a directory directly under Program Files. Run the Windows installer."
}
foreach ($path in @($InstallDir, $Workspace, $dataDir)) {
    Assert-NoRedirect $path
}
foreach ($name in @("nanocodex2.exe", "run-hand.ps1", "hand-service.cs")) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstallDir $name) -PathType Leaf)) { throw "Missing Windows Hand payload: $name" }
}
Stop-HandService
Protect-Directory $InstallDir "S-1-5-32-544" -PublicRead
& $binary --version | Out-Null
if ($LASTEXITCODE -ne 0) { throw "nanocodex2.exe could not start" }
# Provision the official per-user OpenAI Sky runtime before starting the Hand.
if ($env:NANOCODEX_COMPUTER -cnotin @("off", "none", "0")) {
    & $binary computer setup --refresh
    if ($LASTEXITCODE -ne 0) {
        throw "OpenAI Sky setup failed; retry nanocodex2 computer setup before starting the Hand."
    }
}

New-Item -ItemType Directory -Force -Path $Workspace, $dataDir, $serviceLogDir | Out-Null
Protect-Directory $dataDir $userSid


if (-not $SkipLogin) {
    $status = & $binary status --account-file $accountFile
    if ($LASTEXITCODE -ne 0) { throw "Could not check account status" }
    if (($status | ConvertFrom-Json).authenticated -ne $true) {
        & $binary login --account-file $accountFile --label "Nanocodex Windows Hand"
        if ($LASTEXITCODE -ne 0) { throw "Sign-in did not finish" }
        $status = & $binary status --account-file $accountFile
        if ($LASTEXITCODE -ne 0 -or ($status | ConvertFrom-Json).authenticated -ne $true) { throw "Sign-in was not saved" }
    }
}

Stop-HandService
$legacy = Get-ScheduledTask -TaskName "Nanocodex Hand" -ErrorAction SilentlyContinue
if ($null -ne $legacy) {
    Stop-ScheduledTask -TaskName "Nanocodex Hand" -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName "Nanocodex Hand" -Confirm:$false
}
$temporary = Join-Path $InstallDir ("service-" + [Guid]::NewGuid().ToString("N") + ".exe")
try {
    Add-Type -Path (Join-Path $InstallDir "hand-service.cs") -ReferencedAssemblies "System.ServiceProcess.dll", "System.Core.dll", "System.Xml.dll" -OutputAssembly $temporary -OutputType WindowsApplication
    Move-Item -LiteralPath $temporary -Destination $serviceBinary -Force
} finally { Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue }
$config = New-Object Xml.XmlDocument
$root = $config.CreateElement("hand")
$root.SetAttribute("userSid", $userSid)
$root.SetAttribute("workspace", $Workspace)
$root.SetAttribute("dataDir", $dataDir)
$config.AppendChild($root) | Out-Null
$config.Save($configFile)
if ($null -eq (Get-HandService)) {
    New-Service -Name $serviceName -DisplayName "Nanocodex Hand" -BinaryPathName (Quote-Argument $serviceBinary) -StartupType Automatic -Description "Keeps the signed-in user's Nanocodex Hand running in the background." | Out-Null
} else {
    & sc.exe config $serviceName binPath= (Quote-Argument $serviceBinary) start= auto | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not update service configuration" }
}
& sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/10000/restart/30000 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Could not enable service recovery" }
& sc.exe failureflag $serviceName 1 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Could not enable service failure actions" }
if (-not $NoStart) {
    Start-Service -Name $serviceName
    (Get-HandService).WaitForStatus("Running", [TimeSpan]::FromSeconds(20))
    Write-Host "Nanocodex Hand service installed and running. You can close this window."
} else { Write-Host "Nanocodex Hand service installed; it starts automatically at the next boot." }
