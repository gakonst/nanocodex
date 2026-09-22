[CmdletBinding()]
param()

# Exercise the production script up to the first possible Node execution.
# All package metadata and bytes are synthetic; never install Store software,
# execute the fixture .exe, or read/change a real Nanocodex installation.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$provisionScript = Join-Path $PSScriptRoot '../../crates/experimental/nanocodex-computer/src/provision_windows.ps1'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('nanocodex-cua-hash-[fixture] ' + [Guid]::NewGuid().ToString('N'))
$sourceRoot = Join-Path $temporary 'package'
$sourceNode = Join-Path $sourceRoot 'app/resources/cua_node/bin/node.exe'
[void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($sourceNode))
[IO.File]::WriteAllBytes($sourceNode, [Text.Encoding]::ASCII.GetBytes('abc'))

# Reproduce a host where Utility cmdlets are available but its script functions
# cannot be discovered. Explicitly load the standard cmdlets before restricting
# autoloading; do not depend on Get-FileHash even to generate fixture expectations.
Import-Module Microsoft.PowerShell.Management
$utilityAssembly = (Get-Command Get-Date).ImplementingType.Assembly.Location
$PSModuleAutoLoadingPreference = 'None'
Remove-Module Microsoft.PowerShell.Utility -Force -ErrorAction SilentlyContinue
Import-Module $utilityAssembly
if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) { throw 'Hash cmdlet restriction was not applied' }

$previousDirectory = $env:NANOCODEX_DIR
$previousRefresh = $env:NANOCODEX_UPSTREAM_REFRESH
$env:NANOCODEX_DIR = Join-Path $temporary 'cache'
$env:NANOCODEX_UPSTREAM_REFRESH = '0'
$fixture = @{ copies = 0; verified = $false; corruptCopy = $false; package = $null }
$verifiedSentinel = 'Fixture reached the verified bootstrap boundary'

function Get-AppxPackage([string]$Name) {
    if ($Name -ne 'OpenAI.Codex') { throw 'Unexpected package query' }
    return $fixture.package
}
function Copy-Item([string]$LiteralPath, [string]$Destination) {
    $fixture.copies++
    [IO.File]::Copy($LiteralPath, $Destination)
    if ($fixture.corruptCopy) { [IO.File]::WriteAllBytes($Destination, [byte[]]@(0, 1, 2)) }
}
function Test-Path([string]$LiteralPath, [string]$PathType) {
    if ([IO.Path]::GetFileName($LiteralPath) -eq 'provider.json') {
        # The next branch could execute Node. Stop here after the real script's
        # hash comparison succeeds, before any fixture executable can be run.
        $fixture.verified = $true
        throw $verifiedSentinel
    }
    if ($PathType) { return Microsoft.PowerShell.Management\Test-Path -LiteralPath $LiteralPath -PathType $PathType }
    return Microsoft.PowerShell.Management\Test-Path -LiteralPath $LiteralPath
}
function Invoke-ProvisionExpecting([string]$Message) {
    $failure = $null
    try { & $provisionScript | Out-Null } catch { $failure = $_.Exception.Message }
    if ($failure -ne $Message) { throw "Expected '$Message', received '$failure'" }
    if ([IO.Directory]::Exists($env:NANOCODEX_DIR) -and [IO.Directory]::GetFiles($env:NANOCODEX_DIR, 'bootstrap-*.exe', [IO.SearchOption]::AllDirectories).Length -ne 0) {
        throw 'Bootstrap file was not cleaned up'
    }
}

try {
    foreach ($invalid in @('family', 'signature', 'status')) {
        $fixture.package = [pscustomobject]@{
            PackageFamilyName = 'OpenAI.Codex_2p2nqsd0c76g0'; SignatureKind = 'Store'; Status = 'Ok'
            InstallLocation = $sourceRoot; Version = '1.0.0.0'
        }
        switch ($invalid) {
            'family' { $fixture.package.PackageFamilyName = 'Other.Fixture_invalid' }
            'signature' { $fixture.package.SignatureKind = 'Developer' }
            'status' { $fixture.package.Status = 'Modified' }
        }
        Invoke-ProvisionExpecting 'A healthy Microsoft Store-signed OpenAI.Codex package is required.'
        if ($fixture.copies -ne 0 -or [IO.Directory]::Exists($env:NANOCODEX_DIR)) { throw 'Untrusted package reached the copy path' }
    }
    $fixture.package.Status = 'Ok'
    $fixture.corruptCopy = $true
    Invoke-ProvisionExpecting 'Node bootstrap copy differs'
    if ($fixture.verified -or $fixture.copies -ne 1) { throw 'Corrupt bootstrap passed verification' }

    $fixture.corruptCopy = $false
    Invoke-ProvisionExpecting $verifiedSentinel
    if (-not $fixture.verified -or $fixture.copies -ne 2) { throw 'Matching bootstrap did not pass verification' }
    if ([IO.File]::ReadAllText($sourceNode) -ne 'abc') { throw 'Source package bytes were modified' }
    Write-Host 'CUA bootstrap SHA-256 works without Get-FileHash; altered copies and untrusted package metadata are rejected before execution.'
} finally {
    $env:NANOCODEX_DIR = $previousDirectory
    $env:NANOCODEX_UPSTREAM_REFRESH = $previousRefresh
    [IO.Directory]::Delete($temporary, $true)
}
