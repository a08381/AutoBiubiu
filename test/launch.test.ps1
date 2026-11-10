$ErrorActionPreference = 'Stop'
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('autobiubiu-launch-test-' + [guid]::NewGuid().ToString('N'))
$originalLegacy = $env:AUTOBIUBIU_LEGACY_DATA_DIR
$script:SelectedExecutable = $null
$script:SelectedDirectory = $null

function Start-Process {
    param([string]$FilePath, [string]$WorkingDirectory)
    $script:SelectedExecutable = $FilePath
    $script:SelectedDirectory = $WorkingDirectory
}
function Invoke-Fixture {
    $script:SelectedExecutable = $null
    . (Join-Path $fixtureRoot 'scripts\launch.ps1')
}
function Assert-Selected([string]$expected) {
    if ($script:SelectedExecutable -ne $expected) { throw "Unexpected executable: $script:SelectedExecutable" }
    if ($script:SelectedDirectory -ne $fixtureRoot) { throw 'Wrong working directory' }
}

try {
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'scripts') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'src-tauri\target\release') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'src-tauri\target\debug') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\launch.ps1') -Destination (Join-Path $fixtureRoot 'scripts\launch.ps1')
    $releaseFile = Join-Path $fixtureRoot 'src-tauri\target\release\autobiubiu.exe'
    $debugFile = Join-Path $fixtureRoot 'src-tauri\target\debug\autobiubiu.exe'
    [IO.File]::WriteAllText($releaseFile, 'fixture - never executed')
    [IO.File]::WriteAllText($debugFile, 'fixture - never executed')
    $older = [datetime]'2020-01-01T00:00:00Z'
    $newer = $older.AddMinutes(1)

    (Get-Item -LiteralPath $releaseFile).LastWriteTimeUtc = $older
    (Get-Item -LiteralPath $debugFile).LastWriteTimeUtc = $newer
    Invoke-Fixture
    Assert-Selected $debugFile

    (Get-Item -LiteralPath $releaseFile).LastWriteTimeUtc = $newer.AddMinutes(1)
    Invoke-Fixture
    Assert-Selected $releaseFile

    (Get-Item -LiteralPath $debugFile).LastWriteTimeUtc = $newer.AddMinutes(1)
    Invoke-Fixture
    Assert-Selected $releaseFile

    Remove-Item -LiteralPath $debugFile
    Invoke-Fixture
    Assert-Selected $releaseFile

    [IO.File]::WriteAllText($debugFile, 'fixture - never executed')
    Remove-Item -LiteralPath $releaseFile
    Invoke-Fixture
    Assert-Selected $debugFile
    Write-Output 'Launcher tests passed: 5 cases; no application processes started.'
} finally {
    $env:AUTOBIUBIU_LEGACY_DATA_DIR = $originalLegacy
    if (Test-Path -LiteralPath $fixtureRoot) { Remove-Item -LiteralPath $fixtureRoot -Recurse -Force }
}
