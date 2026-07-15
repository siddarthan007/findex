[CmdletBinding()]
param([switch]$Offline)

$ErrorActionPreference = "Stop"
$args = @("run", "--release", "-p", "findex-cli", "--", "models")
if ($Offline) { $args += "--offline" }
& cargo @args --manifest-path (Join-Path $PSScriptRoot "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "Findex model acquisition failed with exit code $LASTEXITCODE"
}
Write-Host "Pinned embedding and reranking models are ready in the shared Hugging Face cache."
