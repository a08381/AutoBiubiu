$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root 'src-tauri\target\release\autobiubiu.exe'
$debug = Join-Path $root 'src-tauri\target\debug\autobiubiu.exe'
# 本地开发构建可能比发行版新，避免双击入口悄悄运行旧逻辑；时间相同优先发行版。
$packaged = if (Test-Path -LiteralPath $release -PathType Leaf) { Get-Item -LiteralPath $release }
$development = if (Test-Path -LiteralPath $debug -PathType Leaf) { Get-Item -LiteralPath $debug }
if ($development -and (-not $packaged -or $development.LastWriteTimeUtc -gt $packaged.LastWriteTimeUtc)) {
    $packaged = $development
}
$env:AUTOBIUBIU_LEGACY_DATA_DIR = Join-Path $root 'data'
if ($packaged) {
    Start-Process -FilePath $packaged.FullName -WorkingDirectory $root
} elseif (Test-Path -LiteralPath (Join-Path $root 'node_modules\@tauri-apps\cli\tauri.js')) {
    # 源码启动保留终端以显示 Vite 和 Rust 构建错误。
    Push-Location $root
    try {
        & npm.cmd start
        if ($LASTEXITCODE -ne 0) { throw "Tauri 启动失败，退出码：$LASTEXITCODE" }
    } finally { Pop-Location }
} else {
    throw '请先安装 Node.js、Rust 和 Visual Studio C++ 工具，再执行 npm install 与 npm run pack；也可使用已生成的安装包。'
}
