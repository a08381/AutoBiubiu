param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [ValidateRange(1, 65535)][int]$Port = 9222
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$exePath = (Resolve-Path -LiteralPath $Executable).Path
$existing = @(Get-Process -Name autobiubiu -ErrorAction SilentlyContinue)
if ($existing.Count -gt 0) {
    throw '已有 AutoBiubiu 实例，诊断脚本不操作现有实例。请完整退出后再运行。'
}
# 只点击本次启动窗口中的“连接客户端”，不点击加速、领取、重启确认或 UAC。
$process = Start-Process -FilePath $exePath -WorkingDirectory (Split-Path -Parent $exePath) -PassThru
$deadline = [DateTime]::UtcNow.AddSeconds(30)
$button = $null
while ([DateTime]::UtcNow -lt $deadline) {
    $process.Refresh()
    if ($process.HasExited) { throw "AutoBiubiu 已退出：$($process.ExitCode)" }
    if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
        $window = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
        $condition = [System.Windows.Automation.AndCondition]::new(
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, '连接客户端'),
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
        )
        $button = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if ($button -and $button.Current.IsEnabled) { break }
    }
    Start-Sleep -Milliseconds 200
}
if (-not $button -or -not $button.Current.IsEnabled) { throw '未找到可用的连接客户端按钮，未发送任何操作。' }
$invoke = $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
$invoke.Invoke()
Write-Output '已点击连接客户端；如出现 Windows 管理员确认，请在屏幕上处理。'
$deadline = [DateTime]::UtcNow.AddSeconds(130)
$cdpReady = $false
$uiConnected = $false
while ([DateTime]::UtcNow -lt $deadline) {
    try {
        $pages = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/json/list" -TimeoutSec 2
        $cdpReady = @($pages | Where-Object { $_.url -match '/renderer/index\.html' }).Count -gt 0
    } catch { $cdpReady = $false }
    if ($cdpReady) {
        foreach ($name in @('客户端已连接 · 已登录', '客户端已连接 · 请登录')) {
            $status = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $name))
            if ($status) { $uiConnected = $true; break }
        }
        if ($uiConnected) { break }
    }
    Start-Sleep -Milliseconds 300
}
[pscustomobject]@{ processId = $process.Id; debugPort = $Port; cdpReady = $cdpReady; uiConnected = $uiConnected } | ConvertTo-Json
# 验证后保留窗口和客户端，交还用户；不结束任何用户进程。
if (-not $cdpReady -or -not $uiConnected) { throw '实际连接尚未确认，请检查 AutoBiubiu 的连接日志与屏幕提示。' }
