//! Anthropic API 中间件

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};

use crate::common::auth;
use crate::kiro::provider::KiroProvider;
use crate::model::config::{CacheOptimizerConfig, ModelMappingConfig};

use super::cache_tracker::CacheTracker;
use super::call_log::CallLog;
use super::types::ErrorResponse;

#[derive(Clone)]
pub(crate) struct PromptCacheSnapshot {
    pub accounting_enabled: bool,
    pub ttl_seconds: u64,
    pub tracker: Arc<CacheTracker>,
}

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// API 密钥
    pub api_key: String,
    /// Kiro Provider（可选，用于实际 API 调用）
    /// 内部使用 MultiTokenManager，已支持线程安全的多凭据管理
    pub kiro_provider: Option<Arc<KiroProvider>>,
    /// 是否开启非流式响应的 thinking 块提取
    pub extract_thinking: bool,
    /// 本地 Prompt Cache usage 记账快照
    pub prompt_cache: PromptCacheSnapshot,
    /// 模拟缓存优化器配置（运行时可热更新）
    pub cache_optimizer: Arc<parking_lot::RwLock<CacheOptimizerConfig>>,
    /// 模型映射配置（运行时可热更新）
    pub model_mapping: Arc<parking_lot::RwLock<ModelMappingConfig>>,
    /// 调用日志（内存环形缓冲）
    pub call_log: CallLog,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(
        api_key: impl Into<String>,
        extract_thinking: bool,
        prompt_cache_ttl_seconds: u64,
        prompt_cache_accounting_enabled: bool,
        cache_optimizer: Arc<parking_lot::RwLock<CacheOptimizerConfig>>,
        model_mapping: Arc<parking_lot::RwLock<ModelMappingConfig>>,
        call_log: CallLog,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            kiro_provider: None,
            extract_thinking,
            prompt_cache: PromptCacheSnapshot {
                accounting_enabled: prompt_cache_accounting_enabled,
                ttl_seconds: prompt_cache_ttl_seconds,
                tracker: Arc::new(CacheTracker::new(Duration::from_secs(
                    prompt_cache_ttl_seconds,
                ))),
            },
            cache_optimizer,
            model_mapping,
            call_log,
        }
    }

    /// 设置已包装的 KiroProvider（与 Admin 共享同一实例）
    pub fn with_kiro_provider_arc(mut self, provider: Arc<KiroProvider>) -> Self {
        self.kiro_provider = Some(provider);
        self
    }

    pub fn prompt_cache_snapshot(&self) -> PromptCacheSnapshot {
        self.prompt_cache.clone()
    }
}

/// API Key 认证中间件
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match auth::extract_api_key(&request) {
        Some(key) if auth::constant_time_eq(&key, &state.api_key) => next.run(request).await,
        _ => {
            let error = ErrorResponse::authentication_error();
            (StatusCode::UNAUTHORIZED, Json(error)).into_response()
        }
    }
}

/// CORS 中间件层
///
/// **安全说明**：当前配置允许所有来源（Any），这是为了支持公开 API 服务。
/// 如果需要更严格的安全控制，请根据实际需求配置具体的允许来源、方法和头信息。
///
/// # 配置说明
/// - `allow_origin(Any)`: 允许任何来源的请求
/// - `allow_methods(Any)`: 允许任何 HTTP 方法
/// - `allow_headers(Any)`: 允许任何请求头
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};

    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}
