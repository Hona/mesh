# Install opencode mesh on Windows (SPEC §10):
#   gh api repos/Hona/mesh/contents/install.ps1 -H "Accept: application/vnd.github.raw" | Out-String | iex
# Optional: $env:MESH_VERSION = "0.1.0" picks a release (default: the latest);
#           $env:MESH_UP_ARGS = "--yes" is passed to `mesh up`.
# Downloads with gh, checks SHA256SUMS, then `mesh up` installs to %LOCALAPPDATA%\Programs\mesh,
# adds that to the user PATH and sets this device up.
& {
    $ErrorActionPreference = 'Stop'
    $gh = (Get-Command gh -ErrorAction SilentlyContinue).Source
    if (-not $gh) { $gh = Join-Path $env:ProgramFiles 'GitHub CLI\gh.exe' }
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $assets = @("mesh-windows-$arch.exe", "meshw-windows-$arch.exe")
    $dir = Join-Path $env:TEMP "mesh-install-$PID"
    New-Item -ItemType Directory -Force $dir | Out-Null
    $tag = @()
    if ($env:MESH_VERSION) { $tag = @('v' + $env:MESH_VERSION.TrimStart('v')) }
    $pattern = @()
    foreach ($a in $assets + 'SHA256SUMS') { $pattern += '-p', $a }
    & $gh release download @tag -R Hona/mesh -D $dir --clobber @pattern
    if ($LASTEXITCODE) { throw 'gh release download failed' }
    $sums = Get-Content (Join-Path $dir 'SHA256SUMS')
    $sha = [Security.Cryptography.SHA256]::Create()
    foreach ($a in $assets) {
        $want = $sums | Where-Object { ($_ -split '\s+')[1] -eq $a } | ForEach-Object { ($_ -split '\s+')[0] }
        $got = -join ($sha.ComputeHash([IO.File]::ReadAllBytes((Join-Path $dir $a))) | ForEach-Object { $_.ToString('x2') })
        if (-not $want -or $want -ne $got) { throw "${a}: SHA256 mismatch" }
    }
    Move-Item -Force (Join-Path $dir $assets[0]) (Join-Path $dir 'mesh.exe')
    Move-Item -Force (Join-Path $dir $assets[1]) (Join-Path $dir 'meshw.exe')
    $env:Path = "$env:LOCALAPPDATA\Programs\mesh;$env:Path"
    $upArgs = @()
    if ($env:MESH_UP_ARGS) { $upArgs = @($env:MESH_UP_ARGS -split '\s+' | Where-Object { $_ }) }
    & (Join-Path $dir 'mesh.exe') up @upArgs
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
