<#
.SYNOPSIS
  Build the innerrag Docker image from Windows (PowerShell 5.1+ or 7, Docker Desktop with the Linux engine).

.EXAMPLE
  .\scripts\build.ps1                                   # image for this machine's architecture
  .\scripts\build.ps1 -Platform amd64                   # linux/amd64
  .\scripts\build.ps1 -Platform all -Push -Tag registry.example.com/innerrag:0.1.0
  .\scripts\build.ps1 -Save innerrag.tar                # export the image to a file
  .\scripts\build.ps1 -Gpu                             # NVIDIA GPU image innerrag:cuda (linux/amd64)
#>
[CmdletBinding()]
param(
  [string]$Tag = "",
  [switch]$Gpu,
  [ValidateSet("native", "amd64", "arm64", "all")]
  [string]$Platform = "native",
  [switch]$Push,
  [string]$Save = "",
  [switch]$NoCache,
  [string]$NerFile = "",
  [string]$EmbedFile = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
  throw "docker is not installed or not in PATH (install Docker Desktop)."
}
docker buildx version *> $null
if ($LASTEXITCODE -ne 0) { throw "docker buildx is required (Docker Desktop 4.x)." }
$osType = docker info --format "{{.OSType}}"
if ($osType -ne "linux") {
  throw "Docker runs Windows containers. innerrag is a Linux image: switch Docker Desktop to Linux containers (tray icon > Switch to Linux containers)."
}

if ($Gpu) {
  if ($Platform -notin @("native", "amd64")) { throw "-Gpu builds linux/amd64 only." }
  $Platform = "amd64"
  if (-not $Tag) { $Tag = "innerrag:cuda" }
} elseif (-not $Tag) {
  $Tag = "innerrag:latest"
}

$platforms = switch ($Platform) {
  "native" { "" }
  "amd64" { "linux/amd64" }
  "arm64" { "linux/arm64" }
  "all" { "linux/amd64,linux/arm64" }
}
if ($Platform -eq "all" -and -not $Push) {
  throw "-Platform all builds a multi-architecture image, which must be pushed: add -Push and a registry -Tag."
}

$dockerArgs = @("buildx", "build", "--tag", $Tag, "--target", $(if ($Gpu) { "runtime-cuda" } else { "cpu" }))
if ($platforms) { $dockerArgs += @("--platform", $platforms) }
if ($NoCache) { $dockerArgs += "--no-cache" }
if ($NerFile) { $dockerArgs += @("--build-arg", "NER_FILE=$NerFile") }
if ($EmbedFile) { $dockerArgs += @("--build-arg", "EMBED_FILE=$EmbedFile") }
if ($Push) { $dockerArgs += "--push" } else { $dockerArgs += "--load" }
if ($Platform -eq "all") {
  docker buildx inspect innerrag-builder *> $null
  if ($LASTEXITCODE -ne 0) { docker buildx create --name innerrag-builder --driver docker-container | Out-Null }
  $dockerArgs += @("--builder", "innerrag-builder")
}
$dockerArgs += "."

Write-Host "==> docker $($dockerArgs -join ' ')"
$sw = [Diagnostics.Stopwatch]::StartNew()
& docker @dockerArgs
if ($LASTEXITCODE -ne 0) { throw "docker build failed (exit $LASTEXITCODE)." }
Write-Host ("==> built {0} in {1:N0} s" -f $Tag, $sw.Elapsed.TotalSeconds)

if (-not $Push) { docker image ls $Tag --format "    {{.Repository}}:{{.Tag}}  {{.Size}}" }
if ($Save) {
  Write-Host "==> saving to $Save"
  docker save $Tag -o $Save
  if ($LASTEXITCODE -ne 0) { throw "docker save failed." }
  Write-Host "    load it elsewhere with: docker load -i $Save"
}
Write-Host "==> run it: docker run -d -p 8080:8080 -v `"`${PWD}\data:/data`" $Tag   (or: docker compose up -d)"
