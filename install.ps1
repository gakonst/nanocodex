[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "Nanocodex currently requires 64-bit Windows"
}

$repository = "gakonst/nanocodex"
$asset = "nanocodex-hand-setup-x86_64.exe"
$release = "https://github.com/$repository/releases/latest/download"
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("nanocodex-install-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $checksums = Join-Path $temporary "SHA256SUMS"
    $installer = Join-Path $temporary $asset
    Invoke-WebRequest -UseBasicParsing -Uri "$release/SHA256SUMS" -OutFile $checksums
    Invoke-WebRequest -UseBasicParsing -Uri "$release/$asset" -OutFile $installer

    $lines = @(Get-Content -LiteralPath $checksums | Where-Object {
        $_ -match ('^[0-9a-fA-F]{64}\s+\*?' + [regex]::Escape($asset) + '$')
    })
    if ($lines.Count -ne 1) { throw "SHA256SUMS must contain exactly one $asset entry" }
    $line = $lines[0]
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "Nanocodex installer checksum mismatch" }

    $process = Start-Process -FilePath $installer -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Nanocodex installer failed with exit code $($process.ExitCode)"
    }
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
