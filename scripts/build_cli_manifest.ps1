[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Repository,
    [Parameter(Mandatory = $true)][string]$ArtifactsDir,
    [Parameter(Mandatory = $true)][string]$Output
)

$ErrorActionPreference = "Stop"
$Version = $Version.TrimStart('v')
$ReleaseBase = "https://github.com/$Repository/releases/download/v$Version"

$targets = @(
    @{ Key = "windows-x86_64"; Archive = "findex-windows-x86_64.zip"; Binary = "findex.exe" },
    @{ Key = "linux-x86_64"; Archive = "findex-linux-x86_64.zip"; Binary = "findex" },
    @{ Key = "macos-aarch64"; Archive = "findex-macos-aarch64.zip"; Binary = "findex" }
)

$platforms = [ordered]@{}
foreach ($target in $targets) {
    $archivePath = Join-Path $ArtifactsDir $target.Archive
    $signaturePath = "$archivePath.sig"
    if (-not (Test-Path -LiteralPath $archivePath)) {
        throw "Missing release archive: $archivePath"
    }
    if (-not (Test-Path -LiteralPath $signaturePath)) {
        throw "Missing release signature: $signaturePath"
    }
    $platforms[$target.Key] = [ordered]@{
        url = "$ReleaseBase/$($target.Archive)"
        signature = (Get-Content -LiteralPath $signaturePath -Raw).Trim()
        binary = $target.Binary
    }
}

$manifest = [ordered]@{
    version = $Version
    notes = "See the GitHub release notes for changes and migration guidance."
    pub_date = [DateTimeOffset]::UtcNow.ToString("o")
    platforms = $platforms
}

$parent = Split-Path -Parent $Output
if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Output -Encoding utf8NoBOM
Write-Host "Wrote signed CLI update manifest: $Output"
