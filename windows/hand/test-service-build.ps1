[CmdletBinding()]
param()

# Run with inbox Windows PowerShell, matching setup-service.ps1 compilation.
# No service, account, or interactive Windows session is created by this check.
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

foreach ($script in Get-ChildItem -LiteralPath $PSScriptRoot -Filter "*.ps1") {
    $tokens = $null
    $parseErrors = $null
    [Management.Automation.Language.Parser]::ParseFile($script.FullName, [ref]$tokens, [ref]$parseErrors) | Out-Null
    if ($parseErrors.Count -gt 0) {
        throw "Invalid PowerShell in $($script.Name): $($parseErrors -join [Environment]::NewLine)"
    }
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) ("nanocodex-service-build-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $supervisor = Join-Path $temporary "nanocodex-hand-service.exe"
    Add-Type -Path (Join-Path $PSScriptRoot "hand-service.cs") `
        -ReferencedAssemblies "System.ServiceProcess.dll", "System.Core.dll", "System.Xml.dll" `
        -OutputAssembly $supervisor -OutputType WindowsApplication
    if (-not (Test-Path -LiteralPath $supervisor -PathType Leaf)) { throw "Supervisor compilation produced no executable" }
    [Reflection.AssemblyName]::GetAssemblyName($supervisor) | Out-Null

    $workspace = Join-Path $temporary "Workspace with spaces"
    $plan = & (Join-Path $PSScriptRoot "setup-service.ps1") -Action Plan -InstallDir $temporary -Workspace $workspace | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "Service plan failed" }
    if ($plan.mode -ne "Service" -or $plan.serviceName -ne "NanocodexHand") { throw "Unexpected service registration plan" }
    if ($plan.workspace -ne $workspace) { throw "Service plan lost the workspace" }
    if ($plan.supervisor -ne $supervisor) { throw "Service plan lost the installed supervisor path" }
    if ($plan.workerRequiresUserSession -ne $true) { throw "Service must keep the worker in a signed-in user's session" }
    if ($plan.userSid -ne [Security.Principal.WindowsIdentity]::GetCurrent().User.Value) { throw "Service plan changed Windows user" }
    if (Test-Path -LiteralPath $workspace) { throw "Planning unexpectedly created a workspace" }
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}

Write-Host "Windows Hand scripts parse and the service supervisor compiles without registration or sign-in."
