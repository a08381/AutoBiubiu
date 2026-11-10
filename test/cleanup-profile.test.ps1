#requires -Version 7.2
# 所有写入仅发生在本测试创建的临时目录。进程查询只在本测试进程中替换。
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$cleaner = Join-Path $PSScriptRoot '..\scripts\Clean-AutoBiubiuProfile.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('AutoBiubiu-cleanup-test-' + [guid]::NewGuid().ToString('N'))
$target = Join-Path $root 'AutoBiubiu'
$backupRoot = Join-Path $root 'backups'
$global:AutoBiubiuCleanupTestProcesses = @()
$script:checks = 0

function Get-CimInstance {
    [CmdletBinding()]
    param([string]$ClassName)
    if ($ClassName -ne 'Win32_Process') { throw '测试查询类型错误。' }
    return $global:AutoBiubiuCleanupTestProcesses
}
function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "断言失败：$Message" }
    $script:checks++
}
function Expect-Error([scriptblock]$Action, [string]$Pattern) {
    $caught = $null
    try { & $Action } catch { $caught = $_ }
    Assert ($null -ne $caught) '操作应报错并保持数据不变'
    Assert ($caught.Exception.Message -match $Pattern) "错误应匹配 $Pattern"
}
function Put-File([string]$Relative, [string]$Value) {
    $path = Join-Path $target $Relative
    $null = [IO.Directory]::CreateDirectory((Split-Path -Path $path -Parent))
    [IO.File]::WriteAllText($path, $Value)
}
function Run-Cleaner([hashtable]$Arguments) {
    & $cleaner -TargetPath $target @Arguments 6>$null
}
function Assert-Kept {
    Assert ([IO.File]::ReadAllText((Join-Path $target 'data\run.json')) -ceq '{"pending":"保留"}') '运行状态原样保留'
    Assert ([IO.File]::ReadAllText((Join-Path $target 'data\custom\note.txt')) -ceq '嵌套业务文件') 'data 内未知文件保留'
    Assert (Test-Path -LiteralPath (Join-Path $target 'data\.storage.lock')) '零字节存储锁保留'
    Assert ([IO.File]::ReadAllText((Join-Path $target 'my-notes.txt')) -ceq '未识别项目') '根目录未知文件保留'
}

try {
    Put-File 'Cache\index.bin' 'cache'
    Put-File 'Code Cache\compiled.bin' 'code'
    Put-File 'Network\Cookies' '旧网络配置'
    Put-File 'Local Storage\value.log' '旧存储'
    Put-File 'Preferences' '旧偏好'
    Put-File 'data\run.json' '{"pending":"保留"}'
    Put-File 'data\custom\note.txt' '嵌套业务文件'
    Put-File 'data\.storage.lock' ''
    Put-File 'my-notes.txt' '未识别项目'

    Run-Cleaner @{ BackupRoot = $backupRoot }
    Assert (-not (Test-Path -LiteralPath $backupRoot)) '默认预览不创建备份'
    Assert (Test-Path -LiteralPath (Join-Path $target 'Cache\index.bin')) '默认预览不移动缓存'
    Run-Cleaner @{ BackupRoot = $backupRoot; IncludeLegacyProfile = $true; Apply = $true; WhatIf = $true }
    Assert (-not (Test-Path -LiteralPath $backupRoot)) 'WhatIf 不创建备份'
    Assert-Kept

    $global:AutoBiubiuCleanupTestProcesses = @([pscustomobject]@{ Name='autobiubiu.exe'; CommandLine=$null; ProcessId=123 })
    Expect-Error { Run-Cleaner @{ BackupRoot=$backupRoot; Apply=$true } } '完全退出'
    Assert (-not (Test-Path -LiteralPath $backupRoot)) '应用运行时不创建备份或移动文件'
    $global:AutoBiubiuCleanupTestProcesses = @()
    Expect-Error { Run-Cleaner @{ BackupRoot=(Join-Path $target 'data\backup'); Apply=$true } } '相互独立'
    Assert-Kept

    Run-Cleaner @{ BackupRoot=$backupRoot; Apply=$true }
    $backups = @(Get-ChildItem -LiteralPath $backupRoot -Directory)
    Assert ($backups.Count -eq 1) '首次执行建立独立备份'
    $backup = $backups[0].FullName
    Assert (-not (Test-Path -LiteralPath (Join-Path $target 'Cache'))) '缓存移出原目录'
    Assert ([IO.File]::ReadAllText((Join-Path $backup 'Cache\index.bin')) -ceq 'cache') '备份内容完整'
    Assert (Test-Path -LiteralPath (Join-Path $target 'Network\Cookies')) '默认保留旧网络配置'
    $manifest = Get-Content -LiteralPath (Join-Path $backup 'manifest.json') -Raw | ConvertFrom-Json
    Assert (@($manifest.Items).Count -eq 2) '清单只含两项测试缓存'
    Assert (@($manifest.Items | Where-Object Status -ne 'moved').Count -eq 0) '移动状态记录完整'
    Assert-Kept

    Run-Cleaner @{ RestoreFrom=$backup }
    Run-Cleaner @{ RestoreFrom=$backup; Apply=$true; WhatIf=$true }
    Assert (-not (Test-Path -LiteralPath (Join-Path $target 'Cache'))) '还原预览及 WhatIf 不移动文件'
    $null = [IO.Directory]::CreateDirectory((Join-Path $target 'Cache'))
    Expect-Error { Run-Cleaner @{ RestoreFrom=$backup; Apply=$true } } '目标已存在'
    Assert (Test-Path -LiteralPath (Join-Path $backup 'Cache\index.bin')) '还原冲突不覆盖或移动备份'
    [IO.Directory]::Delete((Join-Path $target 'Cache'))
    Run-Cleaner @{ RestoreFrom=$backup; Apply=$true }
    Assert (Test-Path -LiteralPath (Join-Path $target 'Cache\index.bin')) '缓存成功还原'
    Assert-Kept

    Run-Cleaner @{ BackupRoot=$backupRoot; IncludeLegacyProfile=$true; Apply=$true }
    $full = @(Get-ChildItem -LiteralPath $backupRoot -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'Network') })
    Assert ($full.Count -eq 1) '完整清理包含旧网络配置'
    Assert (-not (Test-Path -LiteralPath (Join-Path $target 'Preferences'))) '显式选项才移出旧偏好'
    Assert-Kept
    Run-Cleaner @{ RestoreFrom=$full[0].FullName; Apply=$true }
    Assert ([IO.File]::ReadAllText((Join-Path $target 'Network\Cookies')) -ceq '旧网络配置') '完整清理可还原'

    # 不信任备份清单中的任意路径，即使用户误改清单也不得触碰 data。
    $manifestPath = Join-Path $full[0].FullName 'manifest.json'
    $bad = @{ Schema=1; TargetPath=$target; Items=@(@{ Name='..\data'; Status='moved' }) }
    $bad | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $manifestPath -Encoding utf8
    Expect-Error { Run-Cleaner @{ RestoreFrom=$full[0].FullName; Apply=$true } } '未认可'
    Assert-Kept

    $outside = Join-Path $root 'outside'
    $null = [IO.Directory]::CreateDirectory($outside)
    [IO.File]::WriteAllText((Join-Path $outside 'sentinel.txt'), '外部内容')
    $junction = Join-Path $target 'Cache\junction'
    $null = New-Item -ItemType Junction -Path $junction -Target $outside
    try {
        Run-Cleaner @{ BackupRoot=$backupRoot; Apply=$true }
        Assert (Test-Path -LiteralPath (Join-Path $target 'Cache\index.bin')) '含目录联接的整个缓存项目被跳过'
        Assert ([IO.File]::ReadAllText((Join-Path $outside 'sentinel.txt')) -ceq '外部内容') '不跟随或修改目录联接目标'
        Assert-Kept
    } finally {
        # 只解除本测试创建的联接，随后才清理临时测试目录。
        Remove-Item -LiteralPath $junction -Force
    }
    Write-Host "清理脚本隔离测试通过：$script:checks 项断言。真实 AutoBiubiu 目录未参与写入。"
} finally {
    Remove-Variable -Name AutoBiubiuCleanupTestProcesses -Scope Global -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
