//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{ConcurrencyGuard, MultiTokenManager};
use crate::model::config::TlsBackend;
use parking_lot::Mutex;

/// API 调用结果
pub struct ApiCallResult {
    pub response: reqwest::Response,
    pub credential_id: u64,
    /// 本次实际成功连接上游时使用的代理主机（host:port）。
    pub proxy_host: Option<String>,
    /// 是否命中会话亲和（仅供调用日志展示）
    pub session_affinity_hit: bool,
    /// 并发槽位守卫：必须随响应（流式/非流式）一路持有到 body 读完，
    /// drop 时自动释放该凭据的在途计数。详见 token_manager::ConcurrencyGuard。
    pub slot_guard: ConcurrencyGuard,
}

/// 每个凭据的最大重试次数
const MAX_RETRIES_PER_CREDENTIAL: usize = 3;

/// 总重试次数硬上限（避免无限重试）
const MAX_TOTAL_RETRIES: usize = 9;

/// 动态模型列表缓存有效期
const MODEL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// 企业版账号在无法解析出 CodeWhisperer modelId 时使用的默认模型。
const CODEWHISPERER_DEFAULT_MODEL_ID: &str = "CLAUDE_SONNET_4_20250514_V1_0";

/// Kiro 官方模型信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroAvailableModel {
    pub model_id: String,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub token_limits: Option<KiroModelTokenLimits>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroModelTokenLimits {
    pub max_input_tokens: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAvailableModelsResponse {
    #[serde(default)]
    models: Vec<KiroAvailableModel>,
    next_token: Option<String>,
}

#[derive(Clone)]
struct ModelCache {
    models: Vec<KiroAvailableModel>,
    fetched_at: Instant,
}

/// ListAvailableProfiles 返回的单个 profile（仅取 arn）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroProfile {
    arn: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAvailableProfilesResponse {
    #[serde(default)]
    profiles: Vec<KiroProfile>,
}

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// Client 缓存：key = effective proxy config, value = reqwest::Client
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<HashMap<Option<ProxyConfig>, Client>>,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
    /// ListAvailableModels 缓存
    model_cache: Mutex<Option<ModelCache>>,
}

impl KiroProvider {
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client =
            build_client(proxy.as_ref(), 720, tls_backend).expect("创建 HTTP 客户端失败");
        let mut cache = HashMap::new();
        cache.insert(proxy.clone(), initial_client);

        Self {
            token_manager,
            client_cache: Mutex::new(cache),
            tls_backend,
            endpoints,
            default_endpoint,
            model_cache: Mutex::new(None),
        }
    }

    /// 根据代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for_proxy(&self, effective: Option<&ProxyConfig>) -> anyhow::Result<Client> {
        let effective = effective.cloned();
        let mut cache = self.client_cache.lock();
        if let Some(client) = cache.get(&effective) {
            return Ok(client.clone());
        }
        let client = build_client(effective.as_ref(), 720, self.tls_backend)?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 根据凭据的第一优先级代理获取 Client。
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = self.token_manager.effective_proxy_for(credentials);
        self.client_for_proxy(effective.as_ref())
    }

    fn proxy_host_label(proxy: &ProxyConfig) -> String {
        let without_scheme = proxy
            .url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(proxy.url.as_str());
        let without_auth = without_scheme
            .rsplit_once('@')
            .map(|(_, rest)| rest)
            .unwrap_or(without_scheme);
        without_auth
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(without_auth)
            .to_string()
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    /// 获取 Kiro 官方可用模型列表（带 5 分钟缓存）。
    pub async fn list_available_models(&self) -> anyhow::Result<Vec<KiroAvailableModel>> {
        {
            let cache = self.model_cache.lock();
            if let Some(cache) = cache.as_ref() {
                if cache.fetched_at.elapsed() <= MODEL_CACHE_TTL {
                    return Ok(cache.models.clone());
                }
            }
        }

        let models = self.fetch_available_models().await?;
        *self.model_cache.lock() = Some(ModelCache {
            models: models.clone(),
            fetched_at: Instant::now(),
        });
        Ok(models)
    }

    async fn fetch_available_models(&self) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let (ctx, _slot_guard) = self.token_manager.acquire_context(None).await?;
        let config = self.token_manager.config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);
        let endpoint = self.endpoint_for(&ctx.credentials)?;
        self.fetch_available_models_for(&ctx.credentials, &ctx.token, &machine_id, endpoint.as_ref())
            .await
    }

    /// 用显式凭据/token 拉取模型列表（供企业版 modelId 解析复用，不另起调度上下文）。
    async fn fetch_available_models_for(
        &self,
        credentials: &KiroCredentials,
        token: &str,
        machine_id: &str,
        endpoint: &dyn KiroEndpoint,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let config = self.token_manager.config();
        let rctx = RequestContext {
            credentials,
            token,
            machine_id,
            config,
        };

        let url = endpoint
            .models_url(&rctx)
            .ok_or_else(|| anyhow::anyhow!("端点不支持动态模型列表: {}", endpoint.name()))?;

        let mut all_models = Vec::new();
        let mut next_token: Option<String> = None;

        loop {
            let mut params = vec![
                ("origin", "AI_EDITOR".to_string()),
                ("maxResults", "50".to_string()),
            ];

            if let Some(profile_arn) = credentials.resolved_profile_arn() {
                params.push(("profileArn", profile_arn));
            }
            if let Some(token) = next_token.as_ref() {
                params.push(("nextToken", token.clone()));
            }

            let base = self
                .client_for(credentials)?
                .get(&url)
                .query(&params)
                .header("Accept", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_models(base, &rctx);
            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("ListAvailableModels 请求失败: {} {}", status, body);
            }

            let page: ListAvailableModelsResponse = response.json().await?;
            all_models.extend(page.models);

            next_token = page.next_token.filter(|token| !token.trim().is_empty());
            if next_token.is_none() {
                break;
            }
        }

        Ok(all_models)
    }

    /// profileArn 自愈：仅对企业版 / IdC 账号，且 `should_fetch_profile_arn()` 为真时，
    /// 通过 ListAvailableProfiles 拉取真实 ARN 并回写持久化。
    ///
    /// 二开保护：额外加了 `is_enterprise_auth() || is_idc_auth()` 门控（上游仅按
    /// should_fetch 判定）。这样 social 等账号即使缺 ARN 也保持 no-op，绝不触发
    /// 额外网络请求，不影响既有调度路径。返回值对不满足条件者等于入参克隆。
    async fn resolve_and_persist_profile_arn(
        &self,
        credential_id: u64,
        credentials: &KiroCredentials,
        token: &str,
        machine_id: &str,
        endpoint: &dyn KiroEndpoint,
    ) -> KiroCredentials {
        let resolved = credentials.clone();
        // 二开保护门控：非企业/IdC 账号直接 no-op。
        if !(resolved.is_enterprise_auth() || resolved.is_idc_auth()) {
            return resolved;
        }
        if !resolved.should_fetch_profile_arn() {
            return resolved;
        }

        let mut resolved = resolved;
        match self
            .fetch_enterprise_profile_arn(&resolved, token, machine_id, endpoint)
            .await
        {
            Ok(Some(profile_arn)) => {
                tracing::info!(
                    "凭据 #{} 已通过 ListAvailableProfiles 获取 profileArn",
                    credential_id
                );
                resolved.profile_arn = Some(profile_arn.clone());
                if let Err(err) = self.token_manager.update_profile_arn(credential_id, profile_arn) {
                    tracing::warn!(
                        "凭据 #{} profileArn 自愈持久化失败（不影响本次请求）: {}",
                        credential_id,
                        err
                    );
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(
                    "凭据 #{} ListAvailableProfiles 未获取到 profileArn: {}",
                    credential_id,
                    err
                );
            }
        }

        resolved
    }

    async fn fetch_enterprise_profile_arn(
        &self,
        credentials: &KiroCredentials,
        token: &str,
        machine_id: &str,
        endpoint: &dyn KiroEndpoint,
    ) -> anyhow::Result<Option<String>> {
        let config = self.token_manager.config();
        let rctx = RequestContext {
            credentials,
            token,
            machine_id,
            config,
        };

        let Some(url) = endpoint.profiles_url(&rctx) else {
            return Ok(None);
        };

        let base = self
            .client_for(credentials)?
            .post(&url)
            .body("{}")
            .header("content-type", "application/json")
            .header("Connection", "close");
        let request = endpoint.decorate_profiles(base, &rctx);
        let response = request.send().await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::debug!(
                "ListAvailableProfiles 请求失败: {} {}",
                status,
                body.chars().take(200).collect::<String>()
            );
            return Ok(None);
        }

        let data: ListAvailableProfilesResponse = response.json().await?;
        Ok(data
            .profiles
            .into_iter()
            .filter_map(|profile| profile.arn)
            .map(|arn| arn.trim().to_string())
            .find(|arn| !arn.is_empty()))
    }

    /// 获取指定凭据的总请求次数（含失败），供调用日志展示。
    pub fn get_request_count(&self, id: u64) -> u64 {
        self.token_manager.get_request_count(id)
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, false).await
    }

    /// 发送流式 API 请求
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<ApiCallResult> {
        self.call_api_with_retry(request_body, true).await
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    pub async fn call_mcp(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_mcp_with_retry(request_body).await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            // _slot_guard 在本次循环迭代内持有该凭据的并发槽位，迭代结束（continue/return）
            // 时自动释放。MCP 为快速工具调用，body 在 provider 返回后立即读取，槽位略早释放可接受。
            let (ctx, _slot_guard) = match self.token_manager.acquire_context(None).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    // endpoint 解析失败：记为失败，换下一张凭据
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = endpoint.transform_mcp_body(request_body, &rctx);

            let proxy_candidates = self.token_manager.proxy_candidates_for(&ctx.credentials);
            let mut response = None;
            for (proxy_index, proxy) in proxy_candidates.iter().enumerate() {
                let base = self
                    .client_for_proxy(proxy.as_ref())?
                    .post(&url)
                    .body(body.clone())
                    .header("content-type", "application/json")
                    .header("Connection", "close");
                let request = endpoint.decorate_mcp(base, &rctx);

                match request.send().await {
                    Ok(resp) => {
                        response = Some(resp);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "MCP 请求发送失败（尝试 {}/{}，代理 {}/{}）: {}",
                            attempt + 1,
                            max_retries,
                            proxy_index + 1,
                            proxy_candidates.len(),
                            e
                        );
                        last_error = Some(e.into());
                        if proxy_index + 1 < proxy_candidates.len() {
                            continue;
                        }
                    }
                }
            }

            let Some(response) = response else {
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            };

            let status = response.status();

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(response);
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self
                        .token_manager
                        .force_refresh_token_for(ctx.id)
                        .await
                        .is_ok()
                    {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    anyhow::bail!("MCP 请求失败（所有凭据已用尽）: {} {}", status, body);
                }
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                continue;
            }

            // 瞬态错误
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    "MCP 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                anyhow::bail!("MCP 请求失败: {} {}", status, body);
            }

            // 兜底
            last_error = Some(anyhow::anyhow!("MCP 请求失败: {} {}", status, body));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 总重试次数 = min(凭据数量 × 每凭据重试次数, MAX_TOTAL_RETRIES)
    /// - 硬上限 9 次，避免无限重试
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<ApiCallResult> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);
        let session_key = Self::extract_conversation_id_from_request(request_body);

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）+ 并发槽位守卫。
            // slot_guard 是本次循环迭代的局部变量：成功时随 ApiCallResult 透传到 handlers
            // 持有到流读完；任何 continue（429 退避 / 402 切号 / 网络错误）时作为局部变量
            // 自动 drop，旧凭据的在途槽位随之释放，无需任何手动释放代码（方案 §五）。
            let (ctx, slot_guard) = match self
                .token_manager
                .acquire_context_for_session(model.as_deref(), session_key.as_deref())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    // 并发繁忙（所有可用凭据在途已满）：reserve 内部已等待约 2 秒，
                    // 重试只会叠加更多等待且大概率仍然满载，直接返回让 handler 映射 429。
                    if e.to_string().contains("CONCURRENCY_BUSY") {
                        return Err(e);
                    }
                    last_error = Some(e);
                    continue;
                }
            };

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            // profileArn 自愈：仅对企业版/IdC 且缺真实 ARN 的账号生效（内部已门控），
            // 其余账号此调用等价于 ctx.credentials 的克隆，无网络请求、无副作用。
            let credentials = self
                .resolve_and_persist_profile_arn(
                    ctx.id,
                    &ctx.credentials,
                    &ctx.token,
                    &machine_id,
                    endpoint.as_ref(),
                )
                .await;

            let rctx = RequestContext {
                credentials: &credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.api_url(&rctx);
            let mut body = endpoint.transform_api_body(request_body, &rctx);

            // 企业版账号：把模型名解析为 CodeWhisperer 大写 modelId 并写回请求体。
            // 非企业账号 body 原样不变（此分支不执行）。
            if credentials.is_enterprise_auth() {
                let requested_model_id = Self::extract_model_from_request(&body);
                let codewhisperer_model_id = self
                    .resolve_codewhisperer_model_id(
                        &credentials,
                        &ctx.token,
                        &machine_id,
                        endpoint.as_ref(),
                        requested_model_id.as_deref(),
                    )
                    .await;
                body = Self::apply_payload_model_id(&body, &codewhisperer_model_id);
            }

            let proxy_candidates = self.token_manager.proxy_candidates_for(&ctx.credentials);
            let mut response = None;
            let mut proxy_host = None;
            for (proxy_index, proxy) in proxy_candidates.iter().enumerate() {
                let base = self
                    .client_for_proxy(proxy.as_ref())?
                    .post(&url)
                    .body(body.clone())
                    .header("content-type", "application/json")
                    .header("Connection", "close");
                let request = endpoint.decorate_api(base, &rctx);

                match request.send().await {
                    Ok(resp) => {
                        response = Some(resp);
                        proxy_host = proxy.as_ref().map(Self::proxy_host_label);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "API 请求发送失败（尝试 {}/{}，代理 {}/{}）: {}",
                            attempt + 1,
                            max_retries,
                            proxy_index + 1,
                            proxy_candidates.len(),
                            e
                        );
                        // 网络错误通常是上游/链路瞬态问题，不应导致"禁用凭据"或"切换凭据"
                        // （否则一段时间网络抖动会把所有凭据都误禁用，需要重启才能恢复）
                        last_error = Some(e.into());
                        if proxy_index + 1 < proxy_candidates.len() {
                            continue;
                        }
                    }
                }
            }

            let Some(response) = response else {
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            };

            let status = response.status();

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(ApiCallResult {
                    response,
                    credential_id: ctx.id,
                    proxy_host,
                    session_affinity_hit: ctx.session_affinity_hit,
                    slot_guard,
                });
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    "API 请求失败（额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    "API 请求失败（可能为凭据错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self
                        .token_manager
                        .force_refresh_token_for(ctx.id)
                        .await
                        .is_ok()
                    {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    anyhow::bail!(
                        "{} API 请求失败（所有凭据已用尽）: {} {}",
                        api_type,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                continue;
            }

            // 429/408/5xx - 瞬态上游错误：重试但不禁用或切换凭据
            // （避免 429 high traffic / 502 high load 等瞬态错误把所有凭据锁死）
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    "API 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败: {} {}",
                    api_type,
                    status,
                    body
                ));
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                anyhow::bail!("{} API 请求失败: {} {}", api_type, status, body);
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            tracing::warn!(
                "API 请求失败（未知错误，尝试 {}/{}）: {} {}",
                attempt + 1,
                max_retries,
                status,
                body
            );
            last_error = Some(anyhow::anyhow!(
                "{} API 请求失败: {} {}",
                api_type,
                status,
                body
            ));
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
    }

    /// 把请求里的模型名解析成企业版(CodeWhisperer)需要的大写 modelId。
    ///
    /// 仅在主循环的 `is_enterprise_auth()` 分支调用；非企业账号永不进入这里。
    async fn resolve_codewhisperer_model_id(
        &self,
        credentials: &KiroCredentials,
        token: &str,
        machine_id: &str,
        endpoint: &dyn KiroEndpoint,
        requested_model_id: Option<&str>,
    ) -> String {
        let Some(model_id) = requested_model_id
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            return CODEWHISPERER_DEFAULT_MODEL_ID.to_string();
        };

        if Self::is_codewhisperer_model_id(model_id) {
            return model_id.to_string();
        }

        let cached_models = {
            let cache = self.model_cache.lock();
            cache
                .as_ref()
                .filter(|cache| cache.fetched_at.elapsed() <= MODEL_CACHE_TTL)
                .map(|cache| cache.models.clone())
        };

        let models = match cached_models {
            Some(models) => models,
            None => match self
                .fetch_available_models_for(credentials, token, machine_id, endpoint)
                .await
            {
                Ok(models) => {
                    *self.model_cache.lock() = Some(ModelCache {
                        models: models.clone(),
                        fetched_at: Instant::now(),
                    });
                    models
                }
                Err(err) => {
                    tracing::warn!(
                        "解析 CodeWhisperer modelId 时获取模型列表失败，使用默认模型: {}",
                        err
                    );
                    return CODEWHISPERER_DEFAULT_MODEL_ID.to_string();
                }
            },
        };

        models
            .iter()
            .find(|model| Self::matches_requested_model(model, model_id))
            .map(|model| model.model_id.clone())
            .unwrap_or_else(|| CODEWHISPERER_DEFAULT_MODEL_ID.to_string())
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，优先取 currentMessage 的 modelId，缺失时回退到 history。
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        let conversation_state = json.get("conversationState")?;

        if let Some(model_id) = conversation_state
            .get("currentMessage")
            .and_then(|m| m.get("userInputMessage"))
            .and_then(|m| m.get("modelId"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
        {
            return Some(model_id);
        }

        conversation_state
            .get("history")?
            .as_array()?
            .iter()
            .find_map(|message| {
                message
                    .get("userInputMessage")?
                    .get("modelId")?
                    .as_str()
                    .map(|s| s.to_string())
            })
    }

    /// 判断字符串是否已是 CodeWhisperer 形态的 modelId（全大写 + 下划线 + 数字）。
    fn is_codewhisperer_model_id(model_id: &str) -> bool {
        model_id.contains('_')
            && model_id
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    }

    fn normalize_model_key(value: &str) -> String {
        value
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    fn model_tokens(value: &str) -> Vec<String> {
        value
            .to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    fn matches_requested_model(model: &KiroAvailableModel, requested_model_id: &str) -> bool {
        let requested_key = Self::normalize_model_key(requested_model_id);
        let model_id_key = Self::normalize_model_key(&model.model_id);
        if model_id_key == requested_key || model_id_key.contains(&requested_key) {
            return true;
        }

        if model
            .model_name
            .as_deref()
            .is_some_and(|name| Self::normalize_model_key(name).contains(&requested_key))
        {
            return true;
        }

        let tokens: Vec<String> = Self::model_tokens(requested_model_id)
            .into_iter()
            .filter(|token| token != "latest" && token != "model")
            .collect();
        if tokens.is_empty() {
            return false;
        }

        let candidate_tokens: HashSet<String> = Self::model_tokens(&format!(
            "{} {}",
            model.model_id,
            model.model_name.as_deref().unwrap_or("")
        ))
        .into_iter()
        .collect();

        if !tokens
            .iter()
            .all(|token| candidate_tokens.contains(token.as_str()))
        {
            return false;
        }

        for family in ["opus", "sonnet", "haiku"] {
            let requested_has_family = tokens.iter().any(|token| token == family);
            let candidate_has_family = candidate_tokens.contains(family);
            if requested_has_family != candidate_has_family {
                return false;
            }
        }

        true
    }

    /// 把解析出的 CodeWhisperer modelId 写回请求体（currentMessage + 整个 history）。
    fn apply_payload_model_id(request_body: &str, model_id: &str) -> String {
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) else {
            return request_body.to_string();
        };

        if let Some(current_model_id) =
            json.pointer_mut("/conversationState/currentMessage/userInputMessage/modelId")
        {
            *current_model_id = serde_json::Value::String(model_id.to_string());
        }

        if let Some(history) = json
            .pointer_mut("/conversationState/history")
            .and_then(|value| value.as_array_mut())
        {
            for message in history {
                if let Some(history_model_id) = message.pointer_mut("/userInputMessage/modelId") {
                    *history_model_id = serde_json::Value::String(model_id.to_string());
                }
            }
        }

        serde_json::to_string(&json).unwrap_or_else(|_| request_body.to_string())
    }

    /// 从请求体中提取 conversationId，用于 balanced 模式下的会话凭据绑定。
    fn extract_conversation_id_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("conversationId")?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }
}
