Set-StrictMode -Version Latest

function Get-GproxVersionRecord {
    param([string]$Text, [switch]$LockFile)
    $sectionPattern = if ($LockFile) {
        '(?ms)^\[\[package\]\]\r?\n(?<body>.*?)(?=^\[\[|\z)'
    } else {
        '(?ms)^\[package\][^\S\r\n]*\r?\n(?<body>.*?)(?=^\[|\z)'
    }
    $sections = @([regex]::Matches($Text, $sectionPattern) | Where-Object {
        -not $LockFile -or $_.Groups['body'].Value -match '(?m)^name\s*=\s*"gprox"\s*$'
    })
    if ($sections.Count -ne 1) { throw 'Expected exactly one gprox package section' }
    $body = $sections[0].Groups['body']
    $versions = [regex]::Matches($body.Value, '(?m)^version\s*=\s*"(?<version>[^"]+)"[^\S\r\n]*$')
    if ($versions.Count -ne 1) { throw 'Expected exactly one package version' }
    $value = $versions[0].Groups['version']
    if ($value.Value -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw 'Version must use stable MAJOR.MINOR.PATCH without a v prefix'
    }
    [pscustomobject]@{ Value = $value.Value; Index = $body.Index + $value.Index; Length = $value.Length }
}

function Get-GproxVersionState {
    param([string]$Root)
    $manifestPath = Join-Path $Root 'Cargo.toml'
    $lockPath = Join-Path $Root 'Cargo.lock'
    $manifestText = [IO.File]::ReadAllText($manifestPath)
    $lockText = [IO.File]::ReadAllText($lockPath)
    $manifestRecord = Get-GproxVersionRecord -Text $manifestText
    $lockRecord = Get-GproxVersionRecord -Text $lockText -LockFile
    if ($manifestRecord.Value -ne $lockRecord.Value) { throw 'Cargo.toml and Cargo.lock versions disagree' }
    [pscustomobject]@{
        Version = $manifestRecord.Value
        ManifestPath = $manifestPath; ManifestText = $manifestText; ManifestRecord = $manifestRecord
        LockPath = $lockPath; LockText = $lockText; LockRecord = $lockRecord
    }
}
