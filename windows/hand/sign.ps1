[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Path
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGNING_CERT_BASE64)) {
    Write-Host "Authenticode certificate is not configured; leaving development artifacts unsigned."
    exit 0
}
if ([string]::IsNullOrWhiteSpace($env:WINDOWS_SIGNING_CERT_PASSWORD)) {
    throw "WINDOWS_SIGNING_CERT_PASSWORD is required when a signing certificate is configured"
}

$signTool = Get-ChildItem `
    -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin" `
    -Filter signtool.exe `
    -Recurse `
    -ErrorAction Stop |
    Where-Object { $_.FullName -like "*\x64\signtool.exe" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if ($null -eq $signTool) {
    throw "Windows SDK signtool.exe was not found"
}

$certificate = Join-Path $env:RUNNER_TEMP "nanocodex-windows-signing.pfx"
try {
    [IO.File]::WriteAllBytes(
        $certificate,
        [Convert]::FromBase64String($env:WINDOWS_SIGNING_CERT_BASE64)
    )
    foreach ($item in $Path) {
        if (-not (Test-Path -LiteralPath $item -PathType Leaf)) {
            throw "Cannot sign missing artifact: $item"
        }
        & $signTool.FullName sign `
            /fd SHA256 `
            /td SHA256 `
            /tr https://timestamp.digicert.com `
            /f $certificate `
            /p $env:WINDOWS_SIGNING_CERT_PASSWORD `
            $item
        if ($LASTEXITCODE -ne 0) {
            throw "Authenticode signing failed for $item"
        }
        & $signTool.FullName verify /pa /v $item
        if ($LASTEXITCODE -ne 0) {
            throw "Authenticode verification failed for $item"
        }
    }
} finally {
    Remove-Item -LiteralPath $certificate -Force -ErrorAction SilentlyContinue
}
