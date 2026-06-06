# Git 同步与提交说明

本项目通过 Git 在「本地开发环境」与「正式环境」之间同步代码。

- 本地：改代码 → 提交 → 推送到 GitHub
- 正式环境：拉取更新 → 重新编译 → 重启服务

仓库地址：`https://github.com/yigerende/junnl.git`（远程名 `yigerende`）

---

## 一、远程仓库说明

本地配置了两个远程：

| 远程名 | 地址 | 用途 |
|--------|------|------|
| `origin` | `https://github.com/junnl/kiro.rs.git` | 上游原仓库（一般不推） |
| `yigerende` | `https://github.com/yigerende/junnl.git` | 你自己的仓库（日常推送） |

本地 `master` 分支已跟踪 `yigerende/master`，所以 `git push` / `git pull` 默认走 `yigerende`。

---

## 二、配置文件不会被覆盖（重要）

以下文件被 `.gitignore` 忽略，**Git 不跟踪、不上传、不覆盖**：

- `config.json` —— 真实配置（apiKey、adminApiKey、代理等）
- `credentials.json` / `credentials.*` —— 真实凭证
- `kiro_stats.json` —— 运行时统计
- `kiro_balance_cache.json` —— 余额缓存

因此：

- `git clone` 下来**不会有** `config.json` 和 `credentials.json`，正式环境需手动创建（见下）。
- 以后 `git pull` 更新代码时，**绝不会覆盖**正式环境已填好的真实配置和凭证。
- 你本地改的 `config.example.json` 等示例文件会被同步，但不影响真实 `config.json`。

> 仅当有人手动取消 `.gitignore` 忽略、或用 `git add -f` 强制提交这些文件时，才可能被覆盖。正常操作不会发生。

---

## 三、本地：提交并推送

在项目根目录执行：

```powershell
.\push.ps1 "提交说明"
```

---

## 四、正式环境：拉取更新

### 首次部署

```bash
git clone https://github.com/yigerende/junnl.git
cd junnl

# 手动创建真实配置（仓库里没有）
cp config.example.json config.json                    # 填入真实 apiKey / adminApiKey
cp credentials.example.multiple.json credentials.json # 填入真实凭证

# 编译前端（如需 admin-ui）
cd admin-ui && pnpm install && pnpm build && cd ..

# 编译后端
cargo build --release
```

建议把真实配置另存一份备份：`cp config.json config.json.bak`。

### 后续更新

```bash
cd junnl
git pull

# 仅当 admin-ui 前端有改动时才需要
cd admin-ui && pnpm install && pnpm build && cd ..

# 重新编译并重启
cargo build --release
# 按你的部署方式重启，例如：
# systemctl restart junnl
```

---

## 五、注意事项

1. **正式环境只拉取、不改文件**：所有改动在本地完成后推送，避免 `git pull` 冲突。运行时生成的 `kiro_stats.json`、`kiro_balance_cache.json` 已被忽略，不影响拉取。
2. **Rust 项目需重新编译**：`git pull` 只更新源码，正式环境跑的是 `cargo build --release` 后的二进制，每次更新后都要重新编译。
3. **前端改动需重新构建**：admin-ui 的 `dist` 通过 `rust-embed` 打进二进制，前端改了要先 `pnpm build` 再编译后端。
4. **提交前确认敏感文件未被跟踪**：`git status` 中不应出现 `config.json` / `credentials.json`。
