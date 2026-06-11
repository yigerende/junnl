# Changelog

## v2026.6.7 - 2026-06-07

### 发版摘要

本次发版汇总 `v2026.3.1` 之后的 fork 改动，覆盖 Kiro API Key、按凭据路由、模型/ profile 发现、缓存计费、构建链路和 Admin UI 导入流程。

### 重点变更

- 新增 Kiro API Key 凭据支持，覆盖 headless CLI 认证、Admin API/UI 添加、脱敏展示、去重、额度查询和强制刷新错误处理。
- Kiro Provider 改为按当前凭据解析 endpoint、machine id、profile ARN、model id、token type、认证区域和 API 区域。
- 新增模型/profile 发现与会话亲和能力，balanced 模式可以把同一会话稳定分配到兼容凭据。
- 改进 Anthropic 请求转换与流式/非流式响应，包括 thinking 提取、缓存计费、Opus 模型映射、长工具名处理和 token usage 计算。
- 加固构建输入与发版链路，补齐 Cargo/admin UI lock 文件，升级 Node.js 22、pnpm 11，支持 musl 目标和 TLS feature 分流。
- 更新 Admin UI 的批量导入、KAM/enterprise 导入和 API 类型定义。

### 兼容性说明

- `config/`、`config.json`、凭据文件、缓存文件、生成的 `admin-ui/dist` 和 `node_modules` 不属于本次发版提交。
- musl release 构建使用 `--no-default-features`；默认构建仍保留 native TLS。
- Kiro/AWS 真实认证、模型列表和额度查询网络调用仍需要在有真实凭据的环境里验证。

### 验证

- `pnpm build`
- `cargo check`
- `cargo test`，共 `243 passed`

### 逐提交变更

- `551b91f` fix: 修复 Kiro API 工具名称超过 63 字符导致上游拒绝的问题。
- `93ddbc5` sec: 添加 lock 文件，固定构建依赖，降低供应链漂移风险。
- `70b8593` fix: IdC token 刷新时同步更新 `profile_arn`，覆盖 IdC、Builder ID 和 IAM 风格认证。
- `53df562` fix: `profileArn` 改为按当前凭据在 provider 阶段动态注入，避免刷新或切换凭据后携带过期 ARN。
- `298505b` fix: 避免单个 access token 被上游判失效时，把所有本地未过期凭据逐个误禁用。
- `5c41c1d` chore: 清理未使用 import 和死代码 warning，对测试专用 helper 做显式标注。
- `98a12f5` chore: 删除已被 `MultiTokenManager` 替代的旧 TokenManager 和测试专用冗余路径。
- `2805585` fix: `refreshToken` 遇到 `invalid_grant` 时立即禁用永久失效凭据，不再累计重试。
- `5bd6a38` feat: 非流式响应支持 `<thinking>` 提取，并由 `extractThinking` 配置控制。
- `8673096` chore: 删除未使用的 `create_test_provider`，消除编译警告。
- `fbc5f4b` feat: 新增 `KIRO_API_KEY` 和配置文件 API Key 凭据，接入 Kiro CLI headless 认证。
- `daf3aa4` fix: API Key 凭据支持额度查询，并补齐 `tokentype: API_KEY` 请求头。
- `7b5f90a` fix: 加固 API Key 脱敏逻辑，防止短 key 泄露。
- `5270ab3` fix: Admin API `add_credential` 支持添加 API Key 凭据。
- `8b09346` fix: Admin UI 支持添加 API Key 凭据。
- `c93a81b` refactor: 拆分 OAuth refresh token hash、API key hash 和 API key 脱敏展示字段。
- `234b95b` fix: API Key 凭据强制刷新返回 400，避免无实际动作却显示成功。
- `d3c92a9` fix: `machine_id` 派生按凭据类型互斥，并新增随机兜底。
- `7105616` refactor: `machine_id` 直接返回 `String`，同时清理上游死代码。
- `672e361` fix: 空 `KIRO_API_KEY` 会输出 warn，并修正 API Key 过期注释。
- `35a7c93` refactor: 抽象 Kiro endpoint，使不同凭据可选择 IDE 或 CLI endpoint。
- `73baa3f` chore: 切换到 vendored OpenSSL 支持 musl 编译。
- `f0ed27a` feat: 添加 musl 静态构建目标，并用 Cargo feature 控制 native TLS。
- `e4796f5` refactor: 通过 reqwest feature 简化 vendored OpenSSL 配置。
- `8a08bcb` fix: 禁用 reqwest default features 后补回系统代理和 charset 支持。
- `ff514ba` feat: rustls 同时信任本地 CA 和 WebPKI 根证书。
- `19aea59` chore: 添加 README 中声明的缺失 LICENSE 文件。
- `4cca715` feat: 增加 Claude Opus 4.7 模型映射和上下文窗口调整。
- `32a3cfa` fix: 修复 Admin UI 的 Docker 构建输入。
- `a76643f` fix: Docker 构建允许复制 `pnpm-lock.yaml`。
- `a70ab2d` fix: 调整 Admin UI 构建配置，处理 pnpm approved-builds。
- `f1bbe9f` chore: 构建链路升级到 Node.js 22 和 pnpm 11。
- `b3e57e1` feat: 更新 Kiro 认证和 Anthropic 缓存计费，覆盖 provider、stream、config 和 admin 面。
- `f157d16` fix: 修正缓存计费 review 发现的问题，覆盖 converter、stream、handler 和 token manager。
- `fdc049b` release: 在 README 中记录 fork 更新范围。
- `903ff21` release: 从 `jp` 同步最新非配置改动，包含 Kiro auth routing、model discovery、token/session selection、usage fallback 和 admin import 更新。
