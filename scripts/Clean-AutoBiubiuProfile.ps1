#requires -Version 7.2
<#
.SYNOPSIS
预览并归档 AutoBiubiu 旧 Electron 配置；始终保留 data 目录。
.DESCRIPTION
默认只读预览。-Apply 才把白名单项目移到独立备份目录，不永久删除。
默认归档可重建的浏览器缓存；-IncludeLegacyProfile 额外归档旧网络、存储和偏好配置。
执行前须完全退出 AutoBiubiu，包括托盘。不会停止任何进程，不会清理当前 Tauri 的 EBWebView。
跳过符号链接、目录联接及含重解析点的目录；未知项目一律保留。
.EXAMPLE
pwsh -NoProfile -File .\scripts\Clean-AutoBiubiuProfile.ps1
.EXAMPLE
pwsh -NoProfile -File .\scripts\Clean-AutoBiubiuProfile.ps1 -IncludeLegacyProfile -Apply
.EXAMPLE
pwsh -NoProfile -File .\scripts\Clean-AutoBiubiuProfile.ps1 -RestoreFrom 'C:\备份目录\某次备份' -Apply
#>
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'Medium', DefaultParameterSetName = 'Clean')]
param(
    [string]$TargetPath = (Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'AutoBiubiu'),
    [Parameter(ParameterSetName = 'Clean')]
    [string]$BackupRoot = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'AutoBiubiu-CleanupBackups'),
    [Parameter(ParameterSetName = 'Clean')]
    [switch]$IncludeLegacyProfile,
    [Parameter(Mandatory, ParameterSetName = 'Restore')]
    [string]$RestoreFrom,
    [switch]$Apply
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '此脚本仅用于 Windows 上的 AutoBiubiu 配置目录。' }

$cacheNames = @(
    'Cache', 'Code Cache', 'DawnGraphiteCache', 'DawnWebGPUCache', 'GPUCache',
    'GPUPersistentCache', 'GrShaderCache', 'ShaderCache', 'Shared Dictionary'
)
$legacyNames = @(
    'blob_storage', 'Extension State', 'Local Storage', 'Network', 'Session Storage',
    'declarative_performance_observer.db', 'declarative_performance_observer.db-journal',
    'DIPS', 'Local State', 'Preferences'
)
$allowedNames = $cacheNames + $legacyNames

function Get-NormalPath([string]$Path) {
    $expanded = [Environment]::ExpandEnvironmentVariables($Path)
    if (-not [IO.Path]::IsPathFullyQualified($expanded)) { throw "请使用绝对路径：$Path" }
    return [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($expanded))
}

function Test-Inside([string]$Child, [string]$Parent) {
    return $Child.Equals($Parent, [StringComparison]::OrdinalIgnoreCase) -or
        $Child.StartsWith($Parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)
}

function Assert-NoLinkedAncestor([string]$Path) {
    $cursor = $Path
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "路径经过符号链接或目录联接，已停止：$cursor"
            }
        }
        $cursor = Split-Path -Path $cursor -Parent
    }
}

function Get-TreeInfo([string]$Path) {
    $stack = [Collections.Generic.Stack[IO.FileSystemInfo]]::new()
    $stack.Push((Get-Item -LiteralPath $Path -Force))
    $bytes = 0L
    $files = 0
    $links = 0
    $newest = [datetime]::MinValue
    while ($stack.Count -gt 0) {
        $item = $stack.Pop()
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { $links++; continue }
        if ($item.LastWriteTime -gt $newest) { $newest = $item.LastWriteTime }
        if ($item -is [IO.DirectoryInfo]) {
            foreach ($child in Get-ChildItem -LiteralPath $item.FullName -Force) { $stack.Push($child) }
        } else {
            $bytes += $item.Length
            $files++
        }
    }
    return [pscustomobject]@{ Bytes = $bytes; Files = $files; Links = $links; Newest = $newest }
}

function Assert-AppStopped {
    # 查询失败时终止执行；进程路径可能因权限不足而为空，因此也按进程名识别。
    $processes = @(Get-CimInstance -ClassName Win32_Process -ErrorAction Stop | Where-Object {
        $_.Name -ieq 'autobiubiu.exe' -or
        ($_.Name -in @('electron.exe', 'msedgewebview2.exe') -and
            [string]$_.CommandLine -match '(?i)autobiubiu')
    })
    if ($processes.Count) {
        $names = ($processes | ForEach-Object { "$($_.Name) (PID $($_.ProcessId))" }) -join '、'
        throw "请先通过「设置 → 退出应用」或托盘菜单完全退出 AutoBiubiu；仅关闭窗口会留在托盘。检测到：$names"
    }
}

function Assert-Movable([string]$Source, [string]$Destination) {
    Assert-NoLinkedAncestor $Source
    Assert-NoLinkedAncestor (Split-Path -Path $Destination -Parent)
    if (Test-Path -LiteralPath $Destination) { throw "目标已存在，已停止以避免覆盖：$Destination" }
    $info = Get-TreeInfo $Source
    if ($info.Links) { throw "项目内出现重解析点，已停止：$Source" }
}

$target = Get-NormalPath $TargetPath
if ((Split-Path -Path $target -Leaf) -ine 'AutoBiubiu') {
    throw '目标目录的名称必须是 AutoBiubiu，避免误选其他应用目录或磁盘根目录。'
}
if (-not (Test-Path -LiteralPath $target -PathType Container)) { throw "目标目录不存在：$target" }
Assert-NoLinkedAncestor $target
Write-Host "目标目录：$target"
Write-Host '固定保留：data 及其全部内容；未知文件也不会移动。'

if ($PSCmdlet.ParameterSetName -eq 'Restore') {
    $backup = Get-NormalPath $RestoreFrom
    if (Test-Inside $backup $target) { throw '还原来源必须位于目标目录之外。' }
    Assert-NoLinkedAncestor $backup
    $manifestPath = Join-Path $backup 'manifest.json'
    Assert-NoLinkedAncestor $manifestPath
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.Schema -ne 1 -or (Get-NormalPath $manifest.TargetPath) -ine $target) {
        throw '备份清单版本或目标路径不匹配，已停止还原。'
    }
    # 只接受固定白名单里的单层文件名，不执行清单里的代码或任意路径。
    $names = @($manifest.Items | ForEach-Object {
        if ($_.Name -notin $allowedNames) { throw "备份清单含未认可的项目：$($_.Name)" }
        [string]$_.Name
    } | Select-Object -Unique)
    $restoreItems = @($names | Where-Object { Test-Path -LiteralPath (Join-Path $backup $_) })
    foreach ($name in $restoreItems) { Assert-Movable (Join-Path $backup $name) (Join-Path $target $name) }
    Write-Host "可还原项目：$($restoreItems.Count)"
    $restoreItems | ForEach-Object { Write-Host "  $_" }
    if (-not $Apply) { Write-Host '以上仅为预览；加 -Apply 执行还原，不覆盖现有同名项目。'; return }
    if (-not $restoreItems.Count) { return }
    if (-not $PSCmdlet.ShouldProcess($target, "从 $backup 还原 $($restoreItems.Count) 项")) { return }
    Assert-AppStopped
    foreach ($name in $restoreItems) {
        Assert-AppStopped
        $source = Join-Path $backup $name
        $destination = Join-Path $target $name
        Assert-Movable $source $destination
        Move-Item -LiteralPath $source -Destination $destination -ErrorAction Stop
        Write-Host "已还原：$name"
    }
    Write-Host '还原完成；备份清单留存，未删除任何现有数据。'
    return
}

$entries = @(foreach ($entry in Get-ChildItem -LiteralPath $target -Force) {
    $candidate = $entry.Name -in $cacheNames -or ($IncludeLegacyProfile -and $entry.Name -in $legacyNames)
    $classification = if ($entry.Name -ieq 'data') { '保留：当前业务数据' }
        elseif ($entry.Name -in $cacheNames) { '可归档：旧浏览器缓存' }
        elseif ($entry.Name -in $legacyNames) {
            if ($IncludeLegacyProfile) { '可归档：旧 Electron 配置' } else { '保留：旧配置（可选）' }
        } else { '保留：未识别项目' }
    try {
        $info = Get-TreeInfo $entry.FullName
        if ($info.Links) { $candidate = $false; $classification = '跳过：含重解析点' }
        [pscustomobject]@{
            Name = $entry.Name; Bytes = $info.Bytes; Files = $info.Files; Newest = $info.Newest
            Classification = $classification; Candidate = [bool]$candidate
        }
    } catch {
        Write-Warning "跳过 $($entry.Name)：$($_.Exception.Message)"
        [pscustomobject]@{
            Name = $entry.Name; Bytes = 0L; Files = 0; Newest = $null
            Classification = '跳过：读取失败'; Candidate = $false
        }
    }
})
$entries | Select-Object Name, @{n='MiB'; e={ [math]::Round($_.Bytes / 1MB, 3) }},
    Files, Newest, Classification | Format-Table -AutoSize | Out-Host
$candidates = @($entries | Where-Object Candidate)
$total = 0L
foreach ($item in $candidates) { $total += $item.Bytes }
Write-Host ("可归档 {0} 项，合计 {1:N2} MiB。" -f $candidates.Count, ($total / 1MB))
if (-not $IncludeLegacyProfile) { Write-Host '加 -IncludeLegacyProfile 可同时归档旧网络、Local Storage 和偏好配置。' }
if (-not $Apply) { Write-Host '以上仅为预览；未创建备份、未移动或删除任何文件。加 -Apply 执行。'; return }
if (-not $candidates.Count) { Write-Host '没有需要归档的项目。'; return }

$backupBase = Get-NormalPath $BackupRoot
if ((Test-Inside $backupBase $target) -or (Test-Inside $target $backupBase)) {
    throw '备份目录须与目标目录相互独立，二者不得包含对方。'
}
if ([IO.Path]::GetPathRoot($backupBase) -ine [IO.Path]::GetPathRoot($target)) {
    throw '备份目录须位于同一磁盘，确保移动不变成跨盘复制。'
}
Assert-NoLinkedAncestor $backupBase
$backup = Join-Path $backupBase ("{0}-{1}" -f (Get-Date -Format 'yyyyMMdd-HHmmss'), [guid]::NewGuid().ToString('N').Substring(0, 8))
foreach ($item in $candidates) { Assert-Movable (Join-Path $target $item.Name) (Join-Path $backup $item.Name) }
if (-not $PSCmdlet.ShouldProcess($target, "将 $($candidates.Count) 项旧配置移入 $backup；保留 data")) { return }
Assert-AppStopped

$null = [IO.Directory]::CreateDirectory($backup)
$manifestPath = Join-Path $backup 'manifest.json'
$manifest = [ordered]@{
    Schema = 1
    TargetPath = $target
    CreatedAt = [datetime]::UtcNow.ToString('o')
    IncludeLegacyProfile = [bool]$IncludeLegacyProfile
    Items = @($candidates | ForEach-Object { [pscustomobject]@{ Name = $_.Name; Bytes = $_.Bytes; Status = 'planned' } })
}
# 先记录计划；即使移动后写回日志失败，备份目录中的文件仍可由 -RestoreFrom 找回。
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding utf8
Write-Host "备份目录：$backup"
try {
    foreach ($item in $manifest.Items) {
        Assert-AppStopped
        $source = Join-Path $target $item.Name
        $destination = Join-Path $backup $item.Name
        Assert-Movable $source $destination
        Move-Item -LiteralPath $source -Destination $destination -ErrorAction Stop
        $item.Status = 'moved'
        $manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding utf8
        Write-Host "已归档：$($item.Name)"
    }
} catch {
    Write-Warning "归档中止；已移动的内容保存在：$backup"
    Write-Warning '未自动删除或覆盖任何目标；修复原因后可使用 -RestoreFrom 还原。'
    throw
}
Write-Host '清理完成：旧项目已移出配置目录，data 完整保留。备份仍占磁盘空间，确认无需回退后可自行删除。'
$quotedBackup = $backup.Replace("'", "''")
$quotedTarget = $target.Replace("'", "''")
Write-Host "还原参数：-TargetPath '$quotedTarget' -RestoreFrom '$quotedBackup' -Apply"
