[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $HOME ".findex\bin"),
    [switch]$SkipModel,
    [switch]$Cuda
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$FindexHome = Join-Path $HOME ".findex"

Write-Host "Building Findex in release mode..."
$buildArgs = @("build", "--release", "-p", "findex-cli")
if ($Cuda) {
    $buildArgs += @("--features", "cuda")
}
& cargo @buildArgs --manifest-path (Join-Path $ProjectRoot "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$sourceBinary = Join-Path $ProjectRoot "target\release\findex-cli.exe"
$installedBinary = Join-Path $InstallDir "findex.exe"
Copy-Item -LiteralPath $sourceBinary -Destination $installedBinary -Force

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$pathEntries = @($userPath -split ';' | Where-Object { $_ })
if (-not ($pathEntries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
    $updatedPath = (@($pathEntries) + $InstallDir) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
}
$env:Path = "$InstallDir;$env:Path"

if (-not $SkipModel) {
    Write-Host "Acquiring pinned embedding and reranking models..."
    & $installedBinary models
    if ($LASTEXITCODE -ne 0) {
        throw "model acquisition failed with exit code $LASTEXITCODE"
    }
    [Environment]::SetEnvironmentVariable("FINDEX_MODEL_POLICY", "offline", "User")
    $env:FINDEX_MODEL_POLICY = "offline"
}

$serverConfig = @{
    command = $installedBinary
    args = @("--db-path", ".findex_db", "mcp")
}
if (-not $SkipModel) {
    $serverConfig.env = @{ FINDEX_MODEL_POLICY = "offline" }
}
$config = @{
    mcpServers = @{ findex = $serverConfig }
}
$configPath = Join-Path $FindexHome "mcp-config.json"
New-Item -ItemType Directory -Force -Path $FindexHome | Out-Null
$config | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $configPath -Encoding utf8

Write-Host "Findex installed: $installedBinary"
Write-Host "MCP configuration: $configPath"
Write-Host "Open a new terminal if the updated user PATH is not visible in existing shells."
