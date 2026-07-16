[CmdletBinding()]
param(
    [string]$Repository = "siddarthan007/findex",
    [switch]$Silent,
    [ValidateSet("none", "all", "codex", "claude", "cursor", "antigravity")]
    [string]$SetupAgent = "none"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$architecture = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "aarch64" } else { "x64" }
$release = Invoke-RestMethod -Headers @{ "User-Agent" = "findex-installer" } -Uri "https://api.github.com/repos/$Repository/releases/latest"
$pattern = if ($architecture -eq "aarch64") { "*aarch64*setup.exe" } else { "*x64*setup.exe" }
$asset = $release.assets | Where-Object { $_.name -like $pattern } | Select-Object -First 1
if (-not $asset) {
    throw "No Windows $architecture installer is attached to release $($release.tag_name)."
}
$checksums = $release.assets | Where-Object { $_.name -eq "SHA256SUMS" } | Select-Object -First 1
if (-not $checksums) {
    throw "Release $($release.tag_name) has no SHA256SUMS; refusing an unverified install."
}

$staging = Join-Path ([System.IO.Path]::GetTempPath()) "findex-$($release.tag_name)-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force -Path $staging | Out-Null
try {
    $installer = Join-Path $staging $asset.name
    $checksumFile = Join-Path $staging "SHA256SUMS"
    Invoke-WebRequest -Headers @{ "User-Agent" = "findex-installer" } -Uri $asset.browser_download_url -OutFile $installer
    Invoke-WebRequest -Headers @{ "User-Agent" = "findex-installer" } -Uri $checksums.browser_download_url -OutFile $checksumFile
    $line = Get-Content -LiteralPath $checksumFile | Where-Object { $_ -match "\s+$([regex]::Escape($asset.name))$" } | Select-Object -First 1
    if (-not $line) { throw "SHA256SUMS does not contain $($asset.name)." }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $installer).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "SHA-256 mismatch for $($asset.name)." }

    $arguments = if ($Silent) { @("/S") } else { @() }
    $process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Findex installer exited with code $($process.ExitCode)." }

    if ($SetupAgent -ne "none") {
        $findex = Get-Command findex -ErrorAction SilentlyContinue
        if (-not $findex) {
            $appPath = (Get-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\findex.exe" -ErrorAction SilentlyContinue).'(default)'
            if ($appPath) { $findex = @{ Source = $appPath } }
        }
        if (-not $findex) { throw "Findex installed, but the CLI was not discoverable for agent setup." }
        & $findex.Source setup-agent $SetupAgent
        if ($LASTEXITCODE -ne 0) { throw "Findex installed, but agent setup failed." }
    }
    Write-Host "Findex $($release.tag_name) installed and verified."
} finally {
    Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
}
