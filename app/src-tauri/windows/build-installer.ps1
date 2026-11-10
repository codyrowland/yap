# Builds the Windows installer (target/release/bundle/nsis).
# Downloads the Visual C++ runtime installer the first time; it is checked
# for a valid Microsoft signature and not committed.
param(
    [switch]$Signed
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$redist = Join-Path $PSScriptRoot 'vc_redist.x64.exe'

if (-not (Test-Path $redist)) {
    Write-Host 'Downloading vc_redist.x64.exe from Microsoft...'
    Invoke-WebRequest 'https://aka.ms/vc14/vc_redist.x64.exe' -OutFile $redist
}

$signature = Get-AuthenticodeSignature $redist
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
    Remove-Item $redist
    throw 'vc_redist.x64.exe is not signed by Microsoft'
}

Push-Location $root
try {
    if ($Signed) {
        cargo tauri bundle --config windows/tauri.signing.conf.json
    } else {
        cargo tauri build
    }

    if ($LASTEXITCODE -ne 0) {
        throw 'Tauri installer build failed'
    }
} finally {
    Pop-Location
}
