param([string]$Tag)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'version-common.ps1')
$root = Split-Path $PSScriptRoot -Parent
$state = Get-GproxVersionState -Root $root
$expected = 'v' + $state.Version
if ($Tag -and $Tag -cne $expected) { throw "Tag '$Tag' does not match package version '$expected'" }
Write-Output "Version verified: $($state.Version) ($expected)"
