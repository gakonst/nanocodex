[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Nanocodex2,

    [string]$Version = "dev",

    [string]$NumericVersion = "0.0.0.0",

    # Optional local copy of the pinned archive for offline/repeated builds.
    [string]$FfmpegArchive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$payload = Join-Path $root "payload"
$output = Join-Path (Split-Path -Parent (Split-Path -Parent $root)) "dist\windows-hand"
$compiler = @(
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
    (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"),
    (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe")
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $compiler) { throw "Inno Setup 6 is required to build the Windows installer" }
foreach ($path in @($Nanocodex2)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing Windows Hand payload: $path"
    }
}

New-Item -ItemType Directory -Force -Path $payload, $output | Out-Null
Copy-Item -LiteralPath $Nanocodex2 -Destination (Join-Path $payload "nanocodex2.exe") -Force

$ffmpegVersion = "9.0.1"
$ffmpegDigest = "fec81ae03971d9dd4be3ebe02e263bd2ec1d789483f931bdba5f5715e65da2e9"
$ffmpegUrl = "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-$ffmpegVersion-essentials_build.zip"
$downloadedArchive = $false
try {
    if (-not $FfmpegArchive) {
        $FfmpegArchive = Join-Path $env:TEMP ("nanocodex-ffmpeg-" + [Guid]::NewGuid().ToString("N") + ".zip")
        $downloadedArchive = $true
        Invoke-WebRequest -Uri $ffmpegUrl -OutFile $FfmpegArchive -UseBasicParsing
    }
    if ((Get-FileHash -LiteralPath $FfmpegArchive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ffmpegDigest) {
        throw "FFmpeg archive checksum mismatch"
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $FfmpegArchive).ProviderPath)
    try {
        foreach ($item in @{
            "bin/ffmpeg.exe" = "ffmpeg.exe"
            "LICENSE" = "ffmpeg-LICENSE.txt"
            "README.txt" = "ffmpeg-README.txt"
        }.GetEnumerator()) {
            $entry = $zip.GetEntry("ffmpeg-$ffmpegVersion-essentials_build/" + $item.Key)
            if ($null -eq $entry) { throw "FFmpeg archive lacks $($item.Key)" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $payload $item.Value), $true)
        }
    } finally { $zip.Dispose() }
    @"
FFmpeg $ffmpegVersion Windows essentials build by Gyan Doshi.
Archive: $ffmpegUrl
Archive SHA256: $ffmpegDigest
FFmpeg source: https://ffmpeg.org/releases/ffmpeg-$ffmpegVersion.tar.xz
Build configuration and library versions: ffmpeg-README.txt and ffmpeg.exe -version.
License: ffmpeg-LICENSE.txt (GPLv3). FFmpeg runs as a separate process.
"@ | Set-Content -LiteralPath (Join-Path $payload "ffmpeg-NOTICE.txt") -Encoding UTF8
    & $compiler "/DAppVersion=$Version" "/DNumericVersion=$NumericVersion" (Join-Path $root "installer.iss")
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup failed with exit code $LASTEXITCODE"
    }
} finally {
    if ($downloadedArchive) { Remove-Item -LiteralPath $FfmpegArchive -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $payload -Recurse -Force -ErrorAction SilentlyContinue
}

$installer = Join-Path $output "nanocodex-hand-setup-x86_64.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "Installer was not created: $installer"
}
Write-Output $installer
