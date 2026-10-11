[CmdletBinding()]
param()

# Windows upstream CUA is intentionally unsupported until the signed native
# helper's no-Codex policy contract is verified. Run the production
# provisioning script against synthetic state and require it to refuse before
# querying the Store, copying any executable, or writing the cache directory.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$provisionScript = Join-Path $PSScriptRoot '../../crates/experimental/nanocodex-computer/src/provision_windows.ps1'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('nanocodex-cua-refusal-' + [Guid]::NewGuid().ToString('N'))
$previousDirectory = $env:NANOCODEX_DIR
$env:NANOCODEX_DIR = Join-Path $temporary 'cache'
$fixture = @{ queries = 0; copies = 0 }
$expected = 'Windows upstream CUA is unsupported: the native helper policy contract without Codex has not been verified. No installation or configuration was changed.'

function Get-AppxPackage([string]$Name) { $fixture.queries++; return $null }
function Copy-Item([string]$LiteralPath, [string]$Destination) { $fixture.copies++ }

try {
    $failure = $null
    try { & $provisionScript | Out-Null } catch { $failure = $_.Exception.Message }
    if ($failure -ne $expected) { throw "Expected '$expected', received '$failure'" }
    if ($fixture.queries -ne 0) { throw 'Unsupported provisioning queried the Store package' }
    if ($fixture.copies -ne 0) { throw 'Unsupported provisioning copied an executable' }
    if ([IO.Directory]::Exists($env:NANOCODEX_DIR)) { throw 'Unsupported provisioning wrote the cache directory' }
    Write-Host 'Windows CUA provisioning refuses before any Store query, copy, or filesystem mutation.'
} finally {
    $env:NANOCODEX_DIR = $previousDirectory
    if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
}
