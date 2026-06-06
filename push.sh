#!/usr/bin/env bash
# push.sh — 一条命令提交并推送到 GitHub (Linux / Mac)
# 用法:
#   ./push.sh "提交说明"     # 带提交说明
#   ./push.sh                # 不带说明，自动用时间戳

set -euo pipefail

# 切到脚本所在目录（项目根目录）
cd "$(dirname "$0")"

# 组装提交信息
if [ "$#" -gt 0 ]; then
    message="$*"
else
    message="chore: sync at $(date '+%Y-%m-%d %H:%M:%S')"
fi

# 确认是 git 仓库
if [ ! -d ".git" ]; then
    echo "[错误] 当前目录不是 git 仓库根目录。"
    exit 1
fi

# 检查是否有改动
if [ -z "$(git status --porcelain)" ]; then
    echo "[跳过] 工作树干净，没有需要提交的改动。"
    exit 0
fi

echo "[1/3] 暂存改动 (git add .)..."
git add .

echo "[2/3] 提交: $message"
git commit -m "$message"

echo "[3/3] 推送到远程..."
git push

echo "[完成] 已推送到 GitHub。"
