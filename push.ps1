# push.ps1 - one-command commit and push to GitHub
# Usage:
#   .\push.ps1 "commit message"   # with message
#   .\push.ps1                    # no message, auto timestamp

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$MessageParts
)

$ErrorActionPreference = "Stop"

# move to script dir (project root)
Set-Location -Path $PSScriptRoot

# build commit message
if ($MessageParts -and $MessageParts.Count -gt 0) {
    $message = $MessageParts -join " "
} else {
    $message = "chore: sync at " + (Get-Date -Format "yyyy-MM-dd HH:mm:ss")
}

# ensure git repo
if (-not (Test-Path ".git")) {
    Write-Host "[ERROR] Not a git repo root." -ForegroundColor Red
    exit 1
}

# check for changes
$changes = git status --porcelain
if ([string]::IsNullOrWhiteSpace($changes)) {
    Write-Host "[SKIP] Working tree clean, nothing to commit." -ForegroundColor Yellow
    exit 0
}

Write-Host "[1/3] git add ..." -ForegroundColor Cyan
git add .

Write-Host "[2/3] commit: $message" -ForegroundColor Cyan
git commit -m "$message"

Write-Host "[3/3] push to remote ..." -ForegroundColor Cyan
git push

if ($LASTEXITCODE -eq 0) {
    Write-Host "[DONE] Pushed to GitHub." -ForegroundColor Green
} else {
    Write-Host "[FAIL] Push failed, check output above." -ForegroundColor Red
    exit 1
}
