<#
.SYNOPSIS
  Build the innerrag Docker image from Windows (PowerShell 5.1+ or 7), with Docker Desktop (Linux engine)
  or Podman (Podman Desktop / podman machine).

.EXAMPLE
  .\scripts\build.ps1                                   # image for this machine's architecture
  .\scripts\build.ps1 -Platform amd64                   # linux/amd64
  .\scripts\build.ps1 -Platform all -Push -Tag registry.example.com/innerrag:0.1.0
  .\scripts\build.ps1 -Save innerrag.tar                # export the image to a file
  .\scripts\build.ps1 -Gpu                             # NVIDIA GPU image innerrag:cuda (linux/amd64)
  .\scripts\build.ps1 -Engine podman                   # force Podman (default: Docker, else Podman)
  .\scripts\build.ps1 -TrustWindowsCa "Zscaler"         # behind a TLS-inspecting proxy (see certs/README.md)
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
  [string]$EmbedFile = "",
  [ValidateSet("auto", "docker", "podman")]
  [string]$Engine = "auto",
  [string]$TrustWindowsCa = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if ($Engine -eq "auto") {
  if (Get-Command docker -ErrorAction SilentlyContinue) { $Engine = "docker" }
  elseif (Get-Command podman -ErrorAction SilentlyContinue) { $Engine = "podman" }
  else { throw "Neither docker nor podman is in PATH (install Docker Desktop or Podman Desktop)." }
} elseif (-not (Get-Command $Engine -ErrorAction SilentlyContinue)) {
  throw "$Engine is not installed or not in PATH."
}

if ($Engine -eq "docker") {
  docker buildx version *> $null
  if ($LASTEXITCODE -ne 0) { throw "docker buildx is required (Docker Desktop 4.x)." }
  $osType = docker info --format "{{.OSType}}"
  if ($osType -ne "linux") {
    throw "Docker runs Windows containers. innerrag is a Linux image: switch Docker Desktop to Linux containers (tray icon > Switch to Linux containers)."
  }
} else {
  $osType = podman info --format "{{.Host.OS}}" 2>$null
  if ($LASTEXITCODE -ne 0) { throw "podman cannot reach its machine: run 'podman machine init' once, then 'podman machine start'." }
  if ($osType -ne "linux") { throw "podman runs $osType containers; innerrag is a Linux image." }
}

# Behind a proxy that re-signs TLS (Zscaler...), the build trusts the PEM files of certs/. Export the
# matching, still valid authorities of the Windows store there; they stay on this machine (git-ignored).
if ($TrustWindowsCa) {
  $certs = Get-ChildItem Cert:\LocalMachine\Root, Cert:\LocalMachine\CA, Cert:\CurrentUser\Root, Cert:\CurrentUser\CA |
    Where-Object { $_.Subject -match $TrustWindowsCa -and $_.NotAfter -gt (Get-Date) } |
    Sort-Object Thumbprint -Unique
  if (-not $certs) { throw "no valid certificate of the Windows store has a subject matching '$TrustWindowsCa'." }
  $pem = ($certs | ForEach-Object {
      "-----BEGIN CERTIFICATE-----`n" + [Convert]::ToBase64String($_.RawData, "InsertLineBreaks").Replace("`r", "") + "`n-----END CERTIFICATE-----`n"
    }) -join ""
  [IO.File]::WriteAllText((Join-Path (Get-Location) "certs\windows-ca.crt"), $pem)
  Write-Host "==> trusting $($certs.Count) certificate(s) in certs\windows-ca.crt:"
  $certs | ForEach-Object { Write-Host "    $($_.Subject)" }
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
if ($Platform -eq "all" -and $Engine -eq "podman") {
  throw "-Platform all needs docker buildx; with Podman, build and push each architecture with -Platform amd64 / arm64."
}

$target = $(if ($Gpu) { "runtime-cuda" } else { "cpu" })
if ($Engine -eq "docker") {
  $buildArgs = @("buildx", "build", "--tag", $Tag, "--target", $target)
} else {
  $buildArgs = @("build", "--tag", $Tag, "--target", $target)
}
if ($platforms) { $buildArgs += @("--platform", $platforms) }
if ($NoCache) { $buildArgs += "--no-cache" }
if ($NerFile) { $buildArgs += @("--build-arg", "NER_FILE=$NerFile") }
if ($EmbedFile) { $buildArgs += @("--build-arg", "EMBED_FILE=$EmbedFile") }
if ($Engine -eq "docker") {
  if ($Push) { $buildArgs += "--push" } else { $buildArgs += "--load" }
  if ($Platform -eq "all") {
    docker buildx inspect innerrag-builder *> $null
    if ($LASTEXITCODE -ne 0) { docker buildx create --name innerrag-builder --driver docker-container | Out-Null }
    $buildArgs += @("--builder", "innerrag-builder")
  }
}
$buildArgs += "."

Write-Host "==> $Engine $($buildArgs -join ' ')"
$sw = [Diagnostics.Stopwatch]::StartNew()
& $Engine @buildArgs
if ($LASTEXITCODE -ne 0) { throw "$Engine build failed (exit $LASTEXITCODE)." }
Write-Host ("==> built {0} in {1:N0} s" -f $Tag, $sw.Elapsed.TotalSeconds)

# Podman builds into its local store; pushing is a separate step.
if ($Push -and $Engine -eq "podman") {
  podman push $Tag
  if ($LASTEXITCODE -ne 0) { throw "podman push failed." }
}

if (-not $Push) { & $Engine image ls $Tag --format "    {{.Repository}}:{{.Tag}}  {{.Size}}" }
if ($Save) {
  Write-Host "==> saving to $Save"
  & $Engine save $Tag -o $Save
  if ($LASTEXITCODE -ne 0) { throw "$Engine save failed." }
  Write-Host "    load it elsewhere with: $Engine load -i $Save"
}
Write-Host "==> run it: $Engine run -d -p 127.0.0.1:8080:8080 -v `"`${PWD}\data:/data`" $Tag   (or: $Engine compose up -d)"
