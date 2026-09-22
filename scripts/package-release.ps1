param([string]$Tag, [string]$OutputDir)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'version-common.ps1')
$root = Split-Path $PSScriptRoot -Parent
$state = Get-GproxVersionState -Root $root
& (Join-Path $PSScriptRoot 'check-version.ps1') -Tag $Tag
if (-not $IsWindows) { throw 'Release packaging currently supports Windows x64 only' }
if (-not $OutputDir) { $OutputDir = Join-Path $root 'dist' }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$target = 'x86_64-pc-windows-msvc'
$originalRustflags = $env:CARGO_ENCODED_RUSTFLAGS
$buildFlags = @()
if ($null -ne $originalRustflags) {
    $buildFlags += $originalRustflags -split [char]31
} elseif ($env:RUSTFLAGS) {
    $buildFlags += $env:RUSTFLAGS -split '\s+' | Where-Object { $_ }
}
# Panic locations can embed absolute dependency paths even when symbols are stripped.
$profilePath = [Environment]::GetFolderPath('UserProfile')
foreach ($mapping in @(@($profilePath, '/user'), @($root, '/workspace/gprox'))) {
    if ($mapping[0]) {
        $buildFlags += "--remap-path-prefix=$($mapping[0])=$($mapping[1])"
        $buildFlags += "--remap-path-prefix=$($mapping[0].Replace('\', '/'))=$($mapping[1])"
    }
}
$env:CARGO_ENCODED_RUSTFLAGS = $buildFlags -join [char]31
Push-Location $root
try {
    cargo build --release --locked --target $target
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    $binary = Join-Path $root "target/$target/release/gprox.exe"
    $reported = & $binary --version
    if ($LASTEXITCODE -ne 0 -or $reported -cne "gprox $($state.Version)") {
        throw 'Built executable version does not match Cargo.toml'
    }
    $null = New-Item -ItemType Directory -Path $OutputDir -Force
    $archive = Join-Path $OutputDir "gprox-$target.zip"
    Compress-Archive -LiteralPath @($binary, (Join-Path $root 'LICENSE'), (Join-Path $root 'README.md')) -DestinationPath $archive -Force
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksum = "$hash  $([IO.Path]::GetFileName($archive))`n"
    [IO.File]::WriteAllText((Join-Path $OutputDir 'SHA256SUMS'), $checksum, [Text.UTF8Encoding]::new($false))
    Write-Output "Packaged: $archive"
} finally {
    Pop-Location
    $env:CARGO_ENCODED_RUSTFLAGS = $originalRustflags
}
