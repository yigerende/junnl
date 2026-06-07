//! Admin API 业务逻辑服务

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::{CacheOptimizerConfig, Config, ModelMappingConfig};

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, CredentialStatusItem,
    CredentialsStatusResponse, LoadBalancingModeResponse, SetLoadBalancingModeRequest,
};

/// 余额缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;

/// 缓存的余额条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    cache_path: Option<PathBuf>,
    /// 已注册的端点名称集合（用于 add_credential 校验）
    known_endpoints: HashSet<String>,
    /// 运行时模拟缓存配置（与 Anthropic AppState 共享同一个 Arc）
    cache_optimizer_live: Option<Arc<parking_lot::RwLock<CacheOptimizerConfig>>>,
    /// 运行时模型映射配置（与 Anthropic AppState 共享同一个 Arc）
    model_mapping_live: Option<Arc<parking_lot::RwLock<ModelMappingConfig>>>,
    /// Kiro Provider（用于拉取可用模型列表）
    provider: Option<Arc<KiroProvider>>,
    /// 调用日志（与 Anthropic AppState 共享同一个环形缓冲）
    call_log: Option<crate::anthropic::CallLog>,
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        known_endpoints: impl IntoIterator<Item = String>,
    ) -> Self {
        let cache_path = token_manager
            .cache_dir()
            .map(|d| d.join("kiro_balance_cache.json"));

        let balance_cache = Self::load_balance_cache_from(&cache_path);

        Self {
            token_manager,
            balance_cache: Mutex::new(balance_cache),
            cache_path,
            known_endpoints: known_endpoints.into_iter().collect(),
            cache_optimizer_live: None,
            model_mapping_live: None,
            provider: None,
            call_log: None,
        }
    }

    pub fn with_cache_optimizer(mut self, optimizer: Arc<parking_lot::RwLock<CacheOptimizerConfig>>) -> Self {
        self.cache_optimizer_live = Some(optimizer);
        self
    }

    pub fn with_model_mapping(mut self, mapping: Arc<parking_lot::RwLock<ModelMappingConfig>>) -> Self {
        self.model_mapping_live = Some(mapping);
        self
    }

    pub fn with_provider(mut self, provider: Arc<KiroProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_call_log(mut self, call_log: crate::anthropic::CallLog) -> Self {
        self.call_log = Some(call_log);
        self
    }

    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let snapshot = self.token_manager.snapshot();
        let default_endpoint = self.token_manager.config().default_endpoint.clone();

        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| CredentialStatusItem {
                id: entry.id,
                priority: entry.priority,
                disabled: entry.disabled,
                failure_count: entry.failure_count,
                is_current: entry.id == snapshot.current_id,
                expires_at: entry.expires_at,
                auth_method: entry.auth_method,
                provider: entry.provider,
                has_profile_arn: entry.has_profile_arn,
                refresh_token_hash: entry.refresh_token_hash,
                api_key_hash: entry.api_key_hash,
                masked_api_key: entry.masked_api_key,
                email: entry.email,
                success_count: entry.success_count,
                last_used_at: entry.last_used_at.clone(),
                has_proxy: entry.has_proxy,
                proxy_url: entry.proxy_url,
                refresh_failure_count: entry.refresh_failure_count,
                disabled_reason: entry.disabled_reason,
                endpoint: entry.endpoint.unwrap_or_else(|| default_endpoint.clone()),
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        CredentialsStatusResponse {
            total: snapshot.total,
            available: snapshot.available,
            current_id: snapshot.current_id,
            credentials,
        }
    }

    /// 设置凭据禁用状态
    pub fn set_disabled(&self, id: u64, disabled: bool) -> Result<(), AdminServiceError> {
        // 先获取当前凭据 ID，用于判断是否需要切换
        let snapshot = self.token_manager.snapshot();
        let current_id = snapshot.current_id;

        self.token_manager
            .set_disabled(id, disabled)
            .map_err(|e| self.classify_error(e, id))?;

        // 只有禁用的是当前凭据时才尝试切换到下一个
        if disabled && id == current_id {
            let _ = self.token_manager.switch_to_next();
        }
        Ok(())
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 获取凭据余额（带缓存）
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        // 先查缓存
        {
            let cache = self.balance_cache.lock();
            if let Some(cached) = cache.get(&id) {
                let now = Utc::now().timestamp() as f64;
                if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    tracing::debug!("凭据 #{} 余额命中缓存", id);
                    return Ok(cached.data.clone());
                }
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = self.fetch_balance(id).await?;

        // 更新缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();

        Ok(balance)
    }

    /// 强制从上游获取余额（跳过缓存），用于「单独测活」。成功即写回缓存。
    pub async fn get_balance_fresh(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let balance = self.fetch_balance(id).await?;
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();
        Ok(balance)
    }

    /// 从上游获取余额（无缓存）
    async fn fetch_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let usage = self
            .token_manager
            .get_usage_limits_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;
        Ok(Self::build_balance_response(id, &usage))
    }

    /// 从上游额度信息构建余额响应（含超额字段）。
    fn build_balance_response(
        id: u64,
        usage: &crate::kiro::model::usage_limits::UsageLimitsResponse,
    ) -> BalanceResponse {
        let current_usage = usage.current_usage();
        let usage_limit = usage.usage_limit();
        let remaining = (usage_limit - current_usage).max(0.0);
        let usage_percentage = if usage_limit > 0.0 {
            (current_usage / usage_limit * 100.0).min(100.0)
        } else {
            0.0
        };

        BalanceResponse {
            id,
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
            overage_status: usage.overage_status().to_string(),
            overage_capability: usage.overage_capability().map(|s| s.to_string()),
            base_limit: usage.base_limit(),
            overage_cap: usage.overage_cap(),
            total_limit: usage.total_limit_with_overage(),
            overage_usage: usage.overage_usage(),
        }
    }

    /// 返回所有内存缓存中的余额（只读，不请求上游）。
    /// 供前端进页面时立即展示「上次查询到的余额」，可能不是最新。
    pub fn get_cached_balances(&self) -> Vec<BalanceResponse> {
        let cache = self.balance_cache.lock();
        cache.values().map(|c| c.data.clone()).collect()
    }

    /// 设置超额开关（Admin API）。成功后清缓存并返回最新余额。
    pub async fn set_overage(
        &self,
        id: u64,
        enabled: bool,
    ) -> Result<BalanceResponse, AdminServiceError> {
        let usage = self
            .token_manager
            .set_overage_for(id, enabled)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        let mut balance = Self::build_balance_response(id, &usage);

        // 乐观更新：上游 setUserPreference 成功后，getUsageLimits 可能因最终一致性
        // 延迟仍返回旧的 overageStatus。这里以本次操作的目标值为准覆盖，
        // 并据此重算总额度（开启=基础+超额，关闭=基础），保证前端立即正确。
        // 下次用户主动刷新余额时，上游已反映，数据自然一致。
        balance.overage_status = if enabled { "ENABLED".to_string() } else { "DISABLED".to_string() };
        balance.total_limit = if enabled {
            balance.base_limit + balance.overage_cap
        } else {
            balance.base_limit
        };
        if !enabled {
            // 关闭后不再展示已用超额
            balance.overage_usage = 0.0;
        }

        // 状态已变，刷新缓存。
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();

        Ok(balance)
    }

    /// 添加新凭据
    pub async fn add_credential(
        &self,
        req: AddCredentialRequest,
    ) -> Result<AddCredentialResponse, AdminServiceError> {
        // 校验端点名：未指定则默认合法，指定则必须已注册
        if let Some(ref name) = req.endpoint {
            if !self.known_endpoints.contains(name) {
                let mut known: Vec<&str> =
                    self.known_endpoints.iter().map(|s| s.as_str()).collect();
                known.sort();
                return Err(AdminServiceError::InvalidCredential(format!(
                    "未知端点 \"{}\"，已注册端点: {:?}",
                    name, known
                )));
            }
        }

        // 构建凭据对象
        let email = req.email.clone();
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: req.refresh_token,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(req.auth_method),
            provider: req.provider,
            client_id: req.client_id,
            client_secret: req.client_secret,
            priority: req.priority,
            region: req.region,
            auth_region: req.auth_region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            email: req.email,
            subscription_title: None, // 将在首次获取使用额度时自动更新
            proxy_url: req.proxy_url,
            proxy_username: req.proxy_username,
            proxy_password: req.proxy_password,
            disabled: false, // 新添加的凭据默认启用
            kiro_api_key: req.kiro_api_key,
            endpoint: req.endpoint,
        };

        // 调用 token_manager 添加凭据
        let credential_id = self
            .token_manager
            .add_credential(new_cred)
            .await
            .map_err(|e| self.classify_add_error(e))?;

        // 主动获取订阅等级，避免首次请求时 Free 账号绕过 Opus 模型过滤
        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }

        Ok(AddCredentialResponse {
            success: true,
            message: format!("凭据添加成功，ID: {}", credential_id),
            credential_id,
            email,
        })
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;

        // 清理已删除凭据的余额缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.remove(&id);
        }
        self.save_balance_cache();

        Ok(())
    }

    /// 获取负载均衡模式
    pub fn get_load_balancing_mode(&self) -> LoadBalancingModeResponse {
        LoadBalancingModeResponse {
            mode: self.token_manager.get_load_balancing_mode(),
        }
    }

    /// 设置负载均衡模式
    pub fn set_load_balancing_mode(
        &self,
        req: SetLoadBalancingModeRequest,
    ) -> Result<LoadBalancingModeResponse, AdminServiceError> {
        // 验证模式值
        if req.mode != "priority" && req.mode != "balanced" {
            return Err(AdminServiceError::InvalidCredential(
                "mode 必须是 'priority' 或 'balanced'".to_string(),
            ));
        }

        self.token_manager
            .set_load_balancing_mode(req.mode.clone())
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        Ok(LoadBalancingModeResponse { mode: req.mode })
    }

    /// 获取模拟缓存配置
    pub fn get_cache_optimizer(&self) -> CacheOptimizerConfig {
        if let Some(live) = &self.cache_optimizer_live {
            live.read().clone()
        } else {
            self.token_manager.config().cache_optimizer.clone()
        }
    }

    /// 更新模拟缓存配置
    pub fn set_cache_optimizer(
        &self,
        new_config: CacheOptimizerConfig,
    ) -> Result<CacheOptimizerConfig, AdminServiceError> {
        let valid_modes = ["passthrough", "zero", "cap", "random", "weighted"];
        if !valid_modes.contains(&new_config.mode.as_str()) {
            return Err(AdminServiceError::InvalidCredential(
                "mode 必须是 passthrough / zero / cap / random / weighted 之一".to_string(),
            ));
        }

        let config_path = self
            .token_manager
            .config()
            .config_path()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| {
                AdminServiceError::InternalError("配置文件路径未知".to_string())
            })?;

        let mut config = Config::load(&config_path)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        config.cache_optimizer = new_config.clone();
        config
            .save()
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        // 热更新运行时配置
        if let Some(live) = &self.cache_optimizer_live {
            *live.write() = new_config.clone();
        }

        Ok(new_config)
    }

    /// 获取模型映射配置
    pub fn get_model_mapping(&self) -> ModelMappingConfig {
        if let Some(live) = &self.model_mapping_live {
            live.read().clone()
        } else {
            self.token_manager.config().model_mapping.clone()
        }
    }

    /// 更新模型映射配置
    pub fn set_model_mapping(
        &self,
        mut new_config: ModelMappingConfig,
    ) -> Result<ModelMappingConfig, AdminServiceError> {
        // 规范化：去除空行（alias/target 任一为空），按 alias 去重（后写覆盖），上限 200 条
        let mut seen: HashSet<String> = HashSet::new();
        let mut cleaned: Vec<_> = Vec::new();
        for m in new_config.mappings.into_iter().rev() {
            let alias = m.alias.trim().to_string();
            let target = m.target.trim().to_string();
            if alias.is_empty() || target.is_empty() {
                continue;
            }
            let key = alias.to_lowercase();
            if seen.contains(&key) {
                continue; // 已有更靠后的同名条目（rev 遍历，后写优先）
            }
            seen.insert(key);
            cleaned.push(crate::model::config::ModelMapping {
                alias,
                target,
                enabled: m.enabled,
            });
        }
        cleaned.reverse();
        if cleaned.len() > 200 {
            cleaned.truncate(200);
        }
        new_config.mappings = cleaned;

        let config_path = self
            .token_manager
            .config()
            .config_path()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| {
                AdminServiceError::InternalError("配置文件路径未知".to_string())
            })?;

        let mut config = Config::load(&config_path)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        config.model_mapping = new_config.clone();
        config
            .save()
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        // 热更新运行时配置
        if let Some(live) = &self.model_mapping_live {
            *live.write() = new_config.clone();
        }

        Ok(new_config)
    }

    /// 拉取上游可用模型 ID 列表（供前端选择映射目标）
    pub async fn list_available_models(&self) -> Result<Vec<String>, AdminServiceError> {
        let provider = self.provider.as_ref().ok_or_else(|| {
            AdminServiceError::InternalError("Provider 未配置".to_string())
        })?;
        let models = provider
            .list_available_models()
            .await
            .map_err(|e| AdminServiceError::UpstreamError(e.to_string()))?;
        let ids: Vec<String> = models
            .into_iter()
            .map(|m| m.model_id)
            .filter(|id| !id.trim().is_empty())
            .collect();
        Ok(ids)
    }

    /// 获取调用日志（最新在前），最多 limit 条
    pub fn get_call_logs(&self, limit: usize) -> Vec<crate::anthropic::call_log::CallLogEntry> {
        match &self.call_log {
            Some(log) => log.recent(limit),
            None => Vec::new(),
        }
    }

    /// 清空调用日志
    pub fn clear_call_logs(&self) {
        if let Some(log) = &self.call_log {
            log.clear();
        }
    }

    /// 获取调用日志容量上限
    pub fn get_call_log_capacity(&self) -> usize {
        match &self.call_log {
            Some(log) => log.capacity(),
            None => 0,
        }
    }

    /// 设置调用日志容量上限，返回实际生效值
    pub fn set_call_log_capacity(&self, capacity: usize) -> usize {
        match &self.call_log {
            Some(log) => log.set_capacity(capacity),
            None => 0,
        }
    }

    /// 强制刷新指定凭据的 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))
    }

    // ============ 余额缓存持久化 ============

    fn load_balance_cache_from(cache_path: &Option<PathBuf>) -> HashMap<u64, CachedBalance> {
        let path = match cache_path {
            Some(p) => p,
            None => return HashMap::new(),
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };

        // 文件中使用字符串 key 以兼容 JSON 格式
        let map: HashMap<String, CachedBalance> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解析余额缓存失败，将忽略: {}", e);
                return HashMap::new();
            }
        };

        // 加载时不按 TTL 过滤：进页面要能展示「上次查询到的余额」（哪怕已过期）。
        // TTL 仅用于 get_balance 决定是否重新请求上游（见 get_balance）。
        map.into_iter()
            .filter_map(|(k, v)| {
                let id = k.parse::<u64>().ok()?;
                Some((id, v))
            })
            .collect()
    }

    fn save_balance_cache(&self) {
        let path = match &self.cache_path {
            Some(p) => p,
            None => return,
        };

        // 持有锁期间完成序列化和写入，防止并发损坏
        let cache = self.balance_cache.lock();
        let map: HashMap<String, &CachedBalance> =
            cache.iter().map(|(k, v)| (k.to_string(), v)).collect();

        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::warn!("保存余额缓存失败: {}", e);
                }
            }
            Err(e) => tracing::warn!("序列化余额缓存失败: {}", e),
        }
    }

    // ============ 错误分类 ============

    /// 分类简单操作错误（set_disabled, set_priority, reset_and_enable）
    fn classify_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类余额查询错误（可能涉及上游 API 调用）
    fn classify_balance_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();

        // 1. 凭据不存在
        if msg.contains("不存在") {
            return AdminServiceError::NotFound { id };
        }

        // 2. API Key 凭据不支持刷新：客户端请求错误，映射为 400
        if msg.contains("API Key 凭据不支持刷新") {
            return AdminServiceError::InvalidCredential(msg);
        }

        // 3. 上游服务错误特征：HTTP 响应错误或网络错误
        let is_upstream_error =
            // HTTP 响应错误（来自 refresh_*_token 的错误消息）
            msg.contains("凭证已过期或无效") ||
            msg.contains("权限不足") ||
            msg.contains("已被限流") ||
            msg.contains("服务器错误") ||
            msg.contains("Token 刷新失败") ||
            msg.contains("暂时不可用") ||
            // 网络错误（reqwest 错误）
            msg.contains("error trying to connect") ||
            msg.contains("connection") ||
            msg.contains("timeout") ||
            msg.contains("timed out");

        if is_upstream_error {
            AdminServiceError::UpstreamError(msg)
        } else {
            // 4. 默认归类为内部错误（本地验证失败、配置错误等）
            // 包括：缺少 refreshToken、refreshToken 已被截断、无法生成 machineId 等
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类添加凭据错误
    fn classify_add_error(&self, e: anyhow::Error) -> AdminServiceError {
        let msg = e.to_string();

        // 凭据验证失败（refreshToken 无效、格式错误等）
        let is_invalid_credential = msg.contains("缺少 refreshToken")
            || msg.contains("refreshToken 为空")
            || msg.contains("refreshToken 已被截断")
            || msg.contains("凭据已存在")
            || msg.contains("refreshToken 重复")
            || msg.contains("kiroApiKey 重复")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("kiroApiKey 为空")
            || msg.contains("凭证已过期或无效")
            || msg.contains("权限不足")
            || msg.contains("已被限流");

        if is_invalid_credential {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类删除凭据错误
    fn classify_delete_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("只能删除已禁用的凭据") || msg.contains("请先禁用凭据")
        {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }
}
