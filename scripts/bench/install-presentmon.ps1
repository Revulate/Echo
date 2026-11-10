[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()]
    [string]$Version = "2.5.1",

    [string]$OutputDirectory = "",

    [switch]$Force
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path (Split-Path -Parent $PSCommandPath) "..\..")).Path
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot "target\bench-tools\presentmon-$Version"
}
$outputDirectoryPath = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $outputDirectoryPath | Out-Null

$fileName = "PresentMon-$Version-x64.exe"
$destination = Join-Path $outputDirectoryPath $fileName
$knownSha256 = @{
    "2.5.1" = "9BEC3083069F58F911E6A512F4806DB51A27BD096103087BC1D05EF54C80A191"
}
if (-not $knownSha256.ContainsKey($Version)) {
    throw "No pinned SHA-256 is registered for PresentMon $Version. Update this script deliberately before using a new release."
}

function Assert-PresentMonBinary {
    param([Parameter(Mandatory = $true)][string]$Path)

    $actualHash = (Get-FileHash -Algorithm SHA256 -Path $Path).Hash
    $expectedHash = $knownSha256[$Version]
    if ($actualHash -ne $expectedHash) {
        throw "PresentMon SHA-256 mismatch for '$Path': expected $expectedHash, found $actualHash"
    }

    $signature = Get-AuthenticodeSignature -FilePath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "PresentMon Authenticode signature is not valid: $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -notmatch "Intel Corporation") {
        throw "PresentMon signer is not Intel Corporation: $($signature.SignerCertificate.Subject)"
    }

    return [pscustomobject]@{
        path = (Resolve-Path $Path).Path
        version = $Version
        sha256 = $actualHash
        bytes = (Get-Item $Path).Length
        signature_status = $signature.Status.ToString()
        signer = $signature.SignerCertificate.Subject
    }
}

if ((Test-Path $destination) -and -not $Force) {
    $verified = Assert-PresentMonBinary -Path $destination
    $verified | ConvertTo-Json -Depth 3
    Write-Output $verified.path
    exit 0
}

$download = "$destination.download"
$uri = "https://github.com/GameTechDev/PresentMon/releases/download/v$Version/$fileName"
try {
    Invoke-WebRequest -UseBasicParsing -Uri $uri -OutFile $download
    $verified = Assert-PresentMonBinary -Path $download
    Move-Item -Force -Path $download -Destination $destination
    $verified = Assert-PresentMonBinary -Path $destination
    $verified | ConvertTo-Json -Depth 3
    Write-Output $verified.path
} finally {
    if (Test-Path $download) {
        Remove-Item -Force -Path $download
    }
}
