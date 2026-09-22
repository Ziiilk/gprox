param([switch]$Push, [switch]$DryRun)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'version-common.ps1')
$root = Split-Path $PSScriptRoot -Parent
$state = Get-GproxVersionState -Root $root
$tag = 'v' + $state.Version
$changes = @(git -C $root status --porcelain)
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect Git worktree' }
if ($changes.Count) { throw 'Commit all release changes before tagging' }
$null = git -C $root rev-parse --verify HEAD
if ($LASTEXITCODE -ne 0) { throw 'Repository has no commit to tag' }
git -C $root show-ref --verify --quiet "refs/tags/$tag"
if ($LASTEXITCODE -eq 0) { throw "Tag $tag already exists; bump the version instead of replacing it" }
Write-Output "Create lightweight tag $tag at HEAD"
if ($Push) { Write-Output "Push refs/tags/$tag to origin to trigger the Release workflow" }
if ($DryRun) { return }
git -C $root tag $tag
if ($LASTEXITCODE -ne 0) { throw "Cannot create tag $tag" }
if ($Push) {
    git -C $root push origin "refs/tags/$tag"
    if ($LASTEXITCODE -ne 0) { throw "Tag exists locally, but push failed. Retry: git push origin refs/tags/$tag" }
}
