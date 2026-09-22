[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallDir,

    [Parameter(Mandatory = $true)]
    [string]$Workspace,

    [Parameter(Mandatory = $true)]
    [string]$DataDir,

    [switch]$Validate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$binary = Join-Path $InstallDir "nanocodex2.exe"
$state = Join-Path $DataDir "state"
$log = Join-Path $DataDir "hand.log"
$account = Join-Path $DataDir "account.json"

foreach ($path in @($binary)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Nanocodex Hand is incomplete: missing $path"
    }
}

New-Item -ItemType Directory -Force -Path $Workspace, $DataDir, $state | Out-Null
$env:NANOCODEX_ACCOUNT_FILE = $account
# Resolve the bundled capture encoder without changing the machine PATH.
$env:PATH = $InstallDir + [IO.Path]::PathSeparator + $env:PATH

$arguments = @(
    "hand", "--workspace", $Workspace, "--state-dir", $state,
    "--machine-name", $env:COMPUTERNAME, "--log-format", "json", "--log-file", $log
)
# Exercise this exact startup contract without authenticating or attaching.
if ($Validate) { $arguments += "--help" }
& $binary @arguments
exit $LASTEXITCODE
