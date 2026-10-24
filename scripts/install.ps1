# Installs the latest hyperag release for Windows x64 and checks it against SHA256SUMS.
#   irm https://raw.githubusercontent.com/ogarciarevett/hyperag/main/scripts/install.ps1 | iex
# $env:HYPERAG_VERSION pins a tag; $env:HYPERAG_INSTALL_DIR picks the directory.
# While the repository is private, set $env:GH_TOKEN to a token that can read it.
# The body runs in its own scope, so these settings do not stay in the caller's session.
& {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  $tmp = $null
  try {
    $repo = 'ogarciarevett/hyperag'
    $dir = if ($env:HYPERAG_INSTALL_DIR) { $env:HYPERAG_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'hyperag\bin' }
    $headers = @{ Accept = 'application/vnd.github+json' }
    if ($env:GH_TOKEN) { $headers.Authorization = "Bearer $env:GH_TOKEN" }
    $release = if ($env:HYPERAG_VERSION) {
      Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$repo/releases/tags/$env:HYPERAG_VERSION"
    } else {
      Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$repo/releases/latest"
    }
    $tag = $release.tag_name
    if ($tag -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$') { throw "hyperag: invalid or missing release tag" }
    $name = "hyperag-$tag-x86_64-pc-windows-msvc.zip"
    $tmp = New-Item -ItemType Directory (Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid()))
    function Get-Asset($file) {
      $asset = $release.assets | Where-Object name -eq $file
      if (-not $asset) { throw "hyperag: $file is not in release $tag" }
      $h = $headers.Clone(); $h.Accept = 'application/octet-stream'
      Invoke-WebRequest -UseBasicParsing -Headers $h -Uri $asset.url -OutFile (Join-Path $tmp $file)
    }
    Get-Asset $name
    Get-Asset 'SHA256SUMS'
    $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([regex]::Escape($name))$" }) -split ' ' | Select-Object -First 1
    $actual = (Get-FileHash (Join-Path $tmp $name) -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "hyperag: checksum mismatch for $name" }
    Expand-Archive (Join-Path $tmp $name) -DestinationPath $tmp
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $tmp "hyperag-$tag-x86_64-pc-windows-msvc\hyperag.exe") $dir -Force
    Write-Output "hyperag $tag installed to $dir\hyperag.exe"
    if (-not ($env:PATH -split ';' -contains $dir)) { Write-Output "add $dir to PATH" }
  } finally {
    if ($tmp) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
  }
}
