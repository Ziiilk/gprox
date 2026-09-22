[CmdletBinding(DefaultParameterSetName = 'Part')]
param(
    [Parameter(ParameterSetName = 'Part')]
    [ValidateSet('patch', 'minor', 'major')][string]$Part = 'patch',
    [Parameter(Mandatory, ParameterSetName = 'Version')][string]$Version,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'version-common.ps1')
$root = Split-Path $PSScriptRoot -Parent
$state = Get-GproxVersionState -Root $root
if ($PSCmdlet.ParameterSetName -eq 'Part') {
    $parts = @($state.Version.Split('.') | ForEach-Object { [int]$_ })
    switch ($Part) {
        'patch' { $parts[2]++ }
        'minor' { $parts[1]++; $parts[2] = 0 }
        'major' { $parts[0]++; $parts[1] = 0; $parts[2] = 0 }
    }
    $Version = $parts -join '.'
}
if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'Version must be stable MAJOR.MINOR.PATCH'
}
if ([version]$Version -le [version]$state.Version) { throw 'New version must be greater than the current version' }
Write-Output "$($state.Version) -> $Version (tag v$Version)"
if ($DryRun) { return }
$utf8 = [Text.UTF8Encoding]::new($false)
try {
    foreach ($kind in @('Manifest', 'Lock')) {
        $record = $state.($kind + 'Record')
        $text = $state.($kind + 'Text')
        $text = $text.Substring(0, $record.Index) + $Version + $text.Substring($record.Index + $record.Length)
        [IO.File]::WriteAllText($state.($kind + 'Path'), $text, $utf8)
    }
    $null = Get-GproxVersionState -Root $root
} catch {
    [IO.File]::WriteAllText($state.ManifestPath, $state.ManifestText, $utf8)
    [IO.File]::WriteAllText($state.LockPath, $state.LockText, $utf8)
    throw
}
