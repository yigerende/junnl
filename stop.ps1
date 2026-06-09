# junnl 停止脚本
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$pidFile = Join-Path $root ".junnl.pid"

if (-not (Test-Path $pidFile)) {
    $proc = Get-Process -Name "kiro-rs" -ErrorAction SilentlyContinue
    if ($proc) {
        $proc | Stop-Process -Force -Confirm:$false
        Write-Host "[junnl] 已停止 (通过进程名)" -ForegroundColor Green
    } else {
        Write-Host "[junnl] 未在运行" -ForegroundColor Yellow
    }
    return
}

$targetPid = (Get-Content $pidFile).Trim()
$proc = Get-Process -Id $targetPid -ErrorAction SilentlyContinue

if ($proc -and $proc.Name -eq "kiro-rs") {
    Stop-Process -Id $targetPid -Force -Confirm:$false
    Remove-Item $pidFile -Force
    Write-Host "[junnl] 已停止 (PID: $targetPid)" -ForegroundColor Green
} else {
    Remove-Item $pidFile -Force
    Write-Host "[junnl] 进程已不存在，已清理 PID 文件" -ForegroundColor Yellow
}
