# junnl 启动脚本
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root "target\release\kiro-rs.exe"
$pidFile = Join-Path $root ".junnl.pid"

if (Test-Path $pidFile) {
    $oldPid = (Get-Content $pidFile).Trim()
    $proc = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
    if ($proc -and $proc.Name -eq "kiro-rs") {
        Write-Host "[junnl] 已在运行 (PID: $oldPid)" -ForegroundColor Yellow
        return
    }
    Remove-Item $pidFile -Force
}

if (-not (Test-Path $exe)) {
    Write-Host "[junnl] 未找到 release 二进制，正在编译..." -ForegroundColor Cyan
    Push-Location $root
    cargo build --release
    Pop-Location
}

$process = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru -WindowStyle Hidden
$process.Id | Out-File -FilePath $pidFile -Encoding utf8 -NoNewline
Write-Host "[junnl] 已启动 (PID: $($process.Id))" -ForegroundColor Green
