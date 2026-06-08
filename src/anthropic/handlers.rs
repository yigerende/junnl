//! Anthropic API Handler 函数

use std::convert::Infallible;

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use serde_json::json;
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

use super::converter::{ConversionError, SessionHint, convert_request};
use super::middleware::AppState;
use super::stream::{BufferedStreamContext, CacheUsageBreakdown, SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, ErrorResponse, MessagesRequest, Model, ModelsResponse,
    OutputConfig, Thinking,
};
use super::websearch;

/// 生成一次请求的追踪 ID，形如 `r_ab12cd34ef`。
///
/// 仅用于把同一请求横跨多个函数的日志串联起来，不参与任何业务逻辑。
fn new_request_id() -> String {
    let s = Uuid::new_v4().simple().to_string();
    format!("r_{}", &s[..10])
}

/// 应用模型映射（内部逻辑不变，仅替换 payload.model）。
///
/// 返回 (下游原始模型, 上游实际模型, 是否命中映射)，供调用方在请求完成后记录日志。
/// 注意：此函数**不再**记录调用日志——日志改为在请求完成、拿到凭据后记录。
fn apply_model_mapping(state: &AppState, payload: &mut MessagesRequest) -> (String, String, bool) {
    let downstream = payload.model.clone();
    let upstream = state.model_mapping.read().resolve_alias(&downstream);
    let mapped = upstream != downstream;
    if mapped {
        tracing::info!(from = %downstream, to = %upstream, "模型映射生效");
        payload.model = upstream.clone();
    }
    (downstream, upstream, mapped)
}

/// 调用日志上下文：入口提取后传给各出口，在请求完成、拿到 credential_id 后记录。
#[derive(Clone)]
struct CallLogContext {
    call_log: super::call_log::CallLog,
    downstream_model: String,
    upstream_model: String,
    mapped: bool,
    stream: bool,
    endpoint: String,
    client_ip: Option<String>,
    client_host: Option<String>,
    conversation_id: Option<String>,
    conversation_id_source: Option<String>,
}

impl CallLogContext {
    /// 在请求完成后写入一条调用日志。
    /// `provider` 用于查询该凭据的累计请求次数；`credential_id` 为本次实际使用的凭据；
    /// `affinity_hit` 为是否命中会话亲和。
    fn record(
        &self,
        provider: Option<&crate::kiro::provider::KiroProvider>,
        credential_id: Option<u64>,
        affinity_hit: bool,
        success: bool,
    ) {
        let credential_request_count = match (credential_id, provider) {
            (Some(id), Some(p)) => Some(p.get_request_count(id)),
            _ => None,
        };
        self.call_log.record(super::call_log::CallLogEntry {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            downstream_model: self.downstream_model.clone(),
            upstream_model: self.upstream_model.clone(),
            stream: self.stream,
            endpoint: self.endpoint.clone(),
            mapped: self.mapped,
            client_ip: self.client_ip.clone(),
            client_host: self.client_host.clone(),
            credential_id,
            credential_request_count,
            conversation_id: self.conversation_id.clone(),
            conversation_id_source: self.conversation_id_source.clone(),
            session_affinity_hit: affinity_hit,
            success,
        });
    }
}

/// 规范化 IP：去掉端口（`1.2.3.4:5678` → `1.2.3.4`，`[::1]:80` → `::1`）。
fn normalize_ip(raw: &str) -> String {
    let s = raw.trim();
    // 先尝试按 SocketAddr 解析（能正确处理 IPv4:port 和 [IPv6]:port）
    if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
        return sa.ip().to_string();
    }
    // 形如 "1.2.3.4:5678" 但解析失败时，手动按最后一个冒号切（仅 IPv4 形态）
    if s.matches(':').count() == 1 {
        if let Some((host, _port)) = s.rsplit_once(':') {
            if host.parse::<std::net::Ipv4Addr>().is_ok() {
                return host.to_string();
            }
        }
    }
    s.to_string()
}

/// 是否私有/回环地址（对齐 sub2：10/8、172.16/12、192.168/16、127/8、::1、fc00::/7）。
fn is_private_ip(s: &str) -> bool {
    match s.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        Ok(std::net::IpAddr::V6(v6)) => {
            v6.is_loopback()
                // fc00::/7 唯一本地地址（ULA）
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
        Err(_) => false,
    }
}

/// 从请求头提取来源 IP（对齐 sub2 的 GetClientIP 优先级）：
/// 1. CF-Connecting-IP（Cloudflare）
/// 2. X-Real-IP（Nginx）
/// 3. X-Forwarded-For（取第一个公网 IP；全为私有则取第一个）
/// 均去端口；都没有则返回 None。
fn extract_client_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    // 1. Cloudflare
    if let Some(ip) = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| normalize_ip(s))
        .filter(|s| !s.is_empty())
    {
        return Some(ip);
    }
    // 2. Nginx X-Real-IP
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| normalize_ip(s))
        .filter(|s| !s.is_empty())
    {
        return Some(ip);
    }
    // 3. X-Forwarded-For：优先取第一个公网 IP，全私有则取第一个
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let parts: Vec<String> = xff
            .split(',')
            .map(|s| normalize_ip(s))
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(public) = parts.iter().find(|ip| !is_private_ip(ip)) {
            return Some(public.clone());
        }
        if let Some(first) = parts.first() {
            return Some(first.clone());
        }
    }
    None
}

/// 从请求头提取来源域名（Host）。
fn extract_client_host(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 从上游请求体(kiro 格式)提取 conversationId（与 provider 的会话亲和提取逻辑一致）。
fn extract_conversation_id(request_body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(request_body).ok()?;
    json.get("conversationState")?
        .get("conversationId")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// 从请求头提取会话标识候选，构造 SessionHint。
/// 按优先级收集 X-Session-Id / X-Conversation-Id / X-Claude-Code-Session-Id，
/// 供 convert_request 在 metadata.user_id 之外补充会话来源。
fn build_session_hint(headers: &axum::http::HeaderMap) -> SessionHint {
    let mut candidates = Vec::new();
    for name in [
        "x-session-id",
        "x-conversation-id",
        "x-claude-code-session-id",
    ] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let v = v.trim();
            if !v.is_empty() {
                candidates.push(super::converter::SessionCandidate {
                    value: v.to_string(),
                    source: name,
                });
            }
        }
    }
    SessionHint { candidates }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CacheUsageContext {
    cache_creation_input_tokens: i32,
    cache_read_input_tokens: i32,
    cache_creation_5m_input_tokens: i32,
    cache_creation_1h_input_tokens: i32,
    cache_creation_ttl_known: bool,
    prefix_hit_input_jitter: i32,
}

fn build_cache_profile(
    cache_tracker: &crate::anthropic::cache_tracker::CacheTracker,
    payload: &MessagesRequest,
    total_input_tokens: i32,
) -> crate::anthropic::cache_tracker::CacheProfile {
    cache_tracker.build_profile(payload, total_input_tokens)
}

fn compute_cache_usage(
    cache_tracker: &crate::anthropic::cache_tracker::CacheTracker,
    credential_id: u64,
    profile: &crate::anthropic::cache_tracker::CacheProfile,
) -> CacheUsageContext {
    let result = cache_tracker.compute(credential_id, profile);
    CacheUsageContext {
        cache_creation_input_tokens: result.cache_creation_input_tokens,
        cache_read_input_tokens: result.cache_read_input_tokens,
        cache_creation_5m_input_tokens: result.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: result.cache_creation_1h_input_tokens,
        cache_creation_ttl_known: true,
        prefix_hit_input_jitter: result.prefix_hit_input_jitter,
    }
}

fn inject_cache_usage_fields(usage: &mut serde_json::Value, cache_context: CacheUsageContext) {
    usage["cache_creation_input_tokens"] = json!(cache_context.cache_creation_input_tokens);
    usage["cache_read_input_tokens"] = json!(cache_context.cache_read_input_tokens);
    if cache_context.cache_creation_ttl_known {
        usage["cache_creation"] = json!({
            "ephemeral_5m_input_tokens": cache_context.cache_creation_5m_input_tokens,
            "ephemeral_1h_input_tokens": cache_context.cache_creation_1h_input_tokens
        });
    }
}

fn upstream_cache_context_from_token_usage(
    token_usage: &crate::kiro::model::events::TokenUsage,
) -> Option<CacheUsageContext> {
    token_usage.has_input_breakdown().then(|| {
        let cache_write = token_usage.cache_write_tokens();
        CacheUsageContext {
            cache_creation_input_tokens: cache_write,
            cache_read_input_tokens: token_usage.cache_read_tokens(),
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            cache_creation_ttl_known: false,
            prefix_hit_input_jitter: 0,
        }
    })
}

fn billed_input_tokens(
    input_tokens: i32,
    cache_creation_input_tokens: i32,
    cache_read_input_tokens: i32,
) -> i32 {
    input_tokens
        .saturating_sub(cache_creation_input_tokens)
        .saturating_sub(cache_read_input_tokens)
        .max(0)
}

fn scale_cache_context(
    cache_context: CacheUsageContext,
    estimated_input_tokens: i32,
    actual_input_tokens: i32,
) -> CacheUsageContext {
    if estimated_input_tokens <= 0 || actual_input_tokens <= 0 {
        return cache_context;
    }

    if cache_context.prefix_hit_input_jitter > 0
        && cache_context.cache_creation_input_tokens == 0
        && cache_context.cache_read_input_tokens > 0
    {
        let input_tokens =
            prefix_hit_input_tokens(actual_input_tokens, cache_context.prefix_hit_input_jitter);
        return CacheUsageContext {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: actual_input_tokens.saturating_sub(input_tokens),
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            cache_creation_ttl_known: cache_context.cache_creation_ttl_known,
            prefix_hit_input_jitter: cache_context.prefix_hit_input_jitter,
        };
    }

    let mut scaled = CacheUsageContext {
        cache_creation_input_tokens: scale_token_count(
            cache_context.cache_creation_input_tokens,
            estimated_input_tokens,
            actual_input_tokens,
        ),
        cache_read_input_tokens: scale_token_count(
            cache_context.cache_read_input_tokens,
            estimated_input_tokens,
            actual_input_tokens,
        ),
        cache_creation_5m_input_tokens: scale_token_count(
            cache_context.cache_creation_5m_input_tokens,
            estimated_input_tokens,
            actual_input_tokens,
        ),
        cache_creation_1h_input_tokens: scale_token_count(
            cache_context.cache_creation_1h_input_tokens,
            estimated_input_tokens,
            actual_input_tokens,
        ),
        cache_creation_ttl_known: cache_context.cache_creation_ttl_known,
        prefix_hit_input_jitter: cache_context.prefix_hit_input_jitter,
    };

    let cache_total = scaled
        .cache_creation_input_tokens
        .saturating_add(scaled.cache_read_input_tokens);
    if cache_total > actual_input_tokens {
        let overflow = cache_total - actual_input_tokens;
        scaled.cache_creation_input_tokens =
            scaled.cache_creation_input_tokens.saturating_sub(overflow);
    }

    if cache_context.cache_creation_1h_input_tokens > 0
        && cache_context.cache_creation_5m_input_tokens == 0
    {
        scaled.cache_creation_1h_input_tokens = scaled.cache_creation_input_tokens;
        scaled.cache_creation_5m_input_tokens = 0;
    } else if cache_context.cache_creation_5m_input_tokens > 0
        && cache_context.cache_creation_1h_input_tokens == 0
    {
        scaled.cache_creation_5m_input_tokens = scaled.cache_creation_input_tokens;
        scaled.cache_creation_1h_input_tokens = 0;
    }

    scaled
}

fn prefix_hit_input_tokens(input_tokens: i32, jitter: i32) -> i32 {
    if input_tokens <= 0 {
        return 0;
    }

    input_tokens
        .saturating_div(10)
        .saturating_add(jitter)
        .clamp(0, input_tokens)
}

fn scale_token_count(value: i32, estimated_input_tokens: i32, actual_input_tokens: i32) -> i32 {
    if value <= 0 {
        return 0;
    }

    let value = value as i64;
    let estimated = estimated_input_tokens.max(1) as i64;
    let actual = actual_input_tokens.max(0) as i64;
    ((value * actual + estimated / 2) / estimated)
        .min(actual)
        .max(0) as i32
}

/// 将 KiroProvider 错误映射为 HTTP 响应
///
/// `estimated_input_tokens` 为请求进来时的估算输入 token 数（system + messages + tools）。
/// 上下文/输入过长类错误会把它写入日志和返回给用户的错误消息，便于定位
/// 「输入估算多少 token 超了」。错误发生在上游拒绝时，拿不到上游真实输入，故为估算值。
fn map_provider_error(err: Error, estimated_input_tokens: i32) -> Response {
    let err_str = err.to_string();

    // 上下文窗口满了（对话历史累积超出模型上下文窗口限制）
    if err_str.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        tracing::warn!(
            error = %err,
            estimated_input_tokens,
            "上游拒绝请求：上下文窗口已满，输入估算 ~{} tokens（支持最大 1M，不应重试）",
            estimated_input_tokens
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                format!(
                    "Context window is full. Reduce conversation history, system prompt, or tools. \
                     (estimated input ~{estimated_input_tokens} tokens, max supported 1M)"
                ),
            )),
        )
            .into_response();
    }

    // 单次输入太长（请求体本身超出上游限制）
    if err_str.contains("Input is too long") {
        tracing::warn!(
            error = %err,
            estimated_input_tokens,
            "上游拒绝请求：输入过长，输入估算 ~{} tokens（支持最大 1M，不应重试）",
            estimated_input_tokens
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                format!(
                    "Input is too long. Reduce the size of your messages. \
                     (estimated input ~{estimated_input_tokens} tokens, max supported 1M)"
                ),
            )),
        )
            .into_response();
    }
    // 并发繁忙：所有可用凭据在途已满（第二层硬上限兜底），映射为 429 让客户端稍后重试
    if err_str.contains("CONCURRENCY_BUSY") {
        tracing::warn!(error = %err, "并发繁忙：所有可用凭据在途已满");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse::new(
                "rate_limit_error",
                "All credentials are at maximum concurrency. Please retry shortly.".to_string(),
            )),
        )
            .into_response();
    }
    tracing::error!("Kiro API 调用失败: {}", err);
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorResponse::new(
            "api_error",
            format!("上游 API 调用失败: {}", err),
        )),
    )
        .into_response()
}

fn static_models() -> Vec<Model> {
    vec![
        Model {
            id: "claude-opus-4-8".to_string(),
            object: "model".to_string(),
            created: 1780012800, // May 29, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.8".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 128000,
        },
        Model {
            id: "claude-opus-4-8-thinking".to_string(),
            object: "model".to_string(),
            created: 1780012800, // May 29, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.8 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 128000,
        },
        Model {
            id: "claude-opus-4-7".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-7-thinking".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-6".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-6".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-5-20251101".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-5-20251101-thinking".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-5-20250929".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-5-20250929-thinking".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-haiku-4-5-20251001".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-haiku-4-5-20251001-thinking".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
    ]
}

fn model_display_name(model_id: &str, model_name: Option<&str>) -> String {
    if let Some(name) = model_name {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }

    model_id
        .replace('-', " ")
        .replace('.', " ")
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn model_from_kiro(model: crate::kiro::provider::KiroAvailableModel) -> Option<Model> {
    let id = model.model_id.trim();
    if id.is_empty() {
        return None;
    }

    Some(Model {
        id: id.to_string(),
        object: "model".to_string(),
        created: 0,
        owned_by: model
            .model_provider
            .filter(|provider| !provider.trim().is_empty())
            .unwrap_or_else(|| "kiro".to_string()),
        display_name: model_display_name(
            id,
            model.model_name.as_deref().or(model.description.as_deref()),
        ),
        model_type: "chat".to_string(),
        max_tokens: model
            .token_limits
            .and_then(|limits| limits.max_input_tokens.or(limits.max_output_tokens))
            .unwrap_or(200_000),
    })
}

/// GET /v1/models
///
/// 返回可用的模型列表
pub async fn get_models(State(state): State<AppState>) -> impl IntoResponse {
    tracing::info!("Received GET /v1/models request");

    let mut models = if let Some(provider) = &state.kiro_provider {
        match provider.list_available_models().await {
            Ok(dynamic_models) => {
                let models: Vec<Model> = dynamic_models
                    .into_iter()
                    .filter_map(model_from_kiro)
                    .collect();
                if models.is_empty() {
                    tracing::warn!("动态模型列表为空，回退到静态模型列表");
                    static_models()
                } else {
                    models
                }
            }
            Err(err) => {
                tracing::warn!("动态模型列表获取失败，回退到静态模型列表: {}", err);
                static_models()
            }
        }
    } else {
        static_models()
    };

    // 模型映射：把已映射的 target 用 alias 展示给下游
    {
        let mapping = state.model_mapping.read();
        for model in &mut models {
            if let Some(alias) = mapping.alias_for_target(&model.id) {
                model.id = alias;
            }
        }
    }

    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
}

/// POST /v1/messages
///
/// 创建消息（对话）
#[tracing::instrument(
    skip_all,
    fields(request_id = %new_request_id(), route = "/v1/messages")
)]
pub async fn post_messages(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages request"
    );
    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 应用模型映射（内部逻辑不变）。日志改为请求完成后在出口记录。
    let is_stream = payload.stream;
    let (downstream_model, upstream_model, mapped) = apply_model_mapping(&state, &mut payload);
    let mut log_ctx = CallLogContext {
        call_log: state.call_log.clone(),
        downstream_model,
        upstream_model,
        mapped,
        stream: is_stream,
        endpoint: "/v1".to_string(),
        client_ip: extract_client_ip(&headers),
        client_host: extract_client_host(&headers),
        conversation_id: None,        // 在 request_body(kiro格式) 就绪后补充
        conversation_id_source: None, // 在转换后从 ConversionResult 补充
    };

    let prompt_cache = state.prompt_cache_snapshot();

    // 估算输入 tokens，cache 记账需要在 payload 被移动前完成。
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;

    let cache_profile = prompt_cache
        .accounting_enabled
        .then(|| build_cache_profile(prompt_cache.tracker.as_ref(), &payload, input_tokens));
    let provisional_cache_context = cache_profile
        .as_ref()
        .map(|profile| compute_cache_usage(prompt_cache.tracker.as_ref(), 0, profile))
        .unwrap_or_default();
    tracing::info!(
        provisional_cache_creation_input_tokens =
            provisional_cache_context.cache_creation_input_tokens,
        provisional_cache_read_input_tokens = provisional_cache_context.cache_read_input_tokens,
        cache_accounting_enabled = prompt_cache.accounting_enabled,
        prompt_cache_ttl_seconds = prompt_cache.ttl_seconds,
        "Computed provisional cache usage for /v1/messages"
    );

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");
        let resp = websearch::handle_websearch_request(provider, &payload, input_tokens).await;
        log_ctx.record(None, None, false, resp.status().is_success());
        return resp;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload, Some(&build_session_hint(&headers))) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
        }
    };
    // 记录 conversationId 来源（供调用日志展示）
    log_ctx.conversation_id_source = Some(conversion_result.conversation_id_source.to_string());

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // request_body(kiro格式) 就绪后提取 conversationId 补入日志上下文
    log_ctx.conversation_id = extract_conversation_id(&request_body);

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;

    if payload.stream {
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            cache_profile.as_ref(),
            prompt_cache
                .accounting_enabled
                .then_some(&prompt_cache.tracker),
            thinking_enabled,
            tool_name_map,
            state.cache_optimizer.clone(),
            log_ctx,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            cache_profile.as_ref(),
            prompt_cache
                .accounting_enabled
                .then_some(&prompt_cache.tracker),
            extract_thinking,
            tool_name_map,
            state.cache_optimizer.clone(),
            log_ctx,
        )
        .await
    }
}

/// 处理流式请求
async fn handle_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    cache_profile: Option<&crate::anthropic::cache_tracker::CacheProfile>,
    cache_tracker: Option<&std::sync::Arc<crate::anthropic::cache_tracker::CacheTracker>>,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
    cache_optimizer: std::sync::Arc<
        parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>,
    >,
    log_ctx: CallLogContext,
) -> Response {
    let req_started = std::time::Instant::now();
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(
                event = "stream_end",
                reason = "upstream_connect_error",
                elapsed_ms = req_started.elapsed().as_millis() as u64,
                "流式请求上游连接失败"
            );
            // 记录失败的调用日志（未选到可用凭据）
            log_ctx.record(Some(provider.as_ref()), None, false, false);
            return map_provider_error(e, input_tokens);
        }
    };
    // 记录调用日志：已选中凭据并连上上游
    log_ctx.record(
        Some(provider.as_ref()),
        Some(api_result.credential_id),
        api_result.session_affinity_hit,
        true,
    );
    tracing::info!(
        event = "stream_upstream_connected",
        credential_id = api_result.credential_id,
        upstream_connect_ms = req_started.elapsed().as_millis() as u64,
        "流式请求已连接上游"
    );

    let final_cache_context = match (cache_tracker, cache_profile) {
        (Some(tracker), Some(profile)) => {
            let resolved = compute_cache_usage(tracker.as_ref(), api_result.credential_id, profile);
            tracing::info!(
                credential_id = api_result.credential_id,
                final_cache_creation_input_tokens = resolved.cache_creation_input_tokens,
                final_cache_read_input_tokens = resolved.cache_read_input_tokens,
                "Resolved cache usage for stream request"
            );
            tracker.update(api_result.credential_id, profile);
            Some(resolved)
        }
        _ => None,
    };
    let final_cache_usage = final_cache_context.map(|ctx| CacheUsageBreakdown {
        cache_creation_input_tokens: ctx.cache_creation_input_tokens,
        cache_read_input_tokens: ctx.cache_read_input_tokens,
        cache_creation_5m_input_tokens: ctx.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: ctx.cache_creation_1h_input_tokens,
        cache_creation_ttl_known: ctx.cache_creation_ttl_known,
        prefix_hit_input_jitter: ctx.prefix_hit_input_jitter,
    });

    // 创建流处理上下文
    let mut ctx = StreamContext::new_with_cache_usage(
        model,
        input_tokens,
        final_cache_usage,
        thinking_enabled,
        tool_name_map,
    );
    ctx.cache_optimizer = Some(cache_optimizer);
    // 并发槽位守卫随 StreamContext 持有到 stream_end 后 drop
    ctx.slot_guard = Some(api_result.slot_guard);

    // 生成初始事件
    let initial_events = ctx.generate_initial_events();

    // 创建 SSE 流
    let stream = create_sse_stream(api_result.response, ctx, initial_events);

    // 返回 SSE 响应
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Ping 事件间隔（25秒）
const PING_INTERVAL_SECS: u64 = 25;

/// 创建 ping 事件的 SSE 字符串
fn create_ping_sse() -> Bytes {
    Bytes::from("event: ping\ndata: {\"type\": \"ping\"}\n\n")
}

/// 创建 SSE 事件流
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    // 先发送初始事件
    let initial_stream = stream::iter(
        initial_events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    );

    // 然后处理 Kiro 响应流，同时每25秒发送 ping 保活
    let body_stream = response.bytes_stream();

    // 捕获当前请求 span（含 request_id）。SSE 流是在 handler 返回后才被轮询的，
    // 那时 instrument 的 span 已不活跃，所以这里捕获下来，打点时用 in_scope 临时进入。
    let span = tracing::Span::current();
    let stream_started = std::time::Instant::now();

    let processing_stream = stream::unfold(
        (body_stream, ctx, EventStreamDecoder::new(), false, interval(Duration::from_secs(PING_INTERVAL_SECS)), false, 0u64),
        move |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval, mut first_byte_logged, mut ping_count)| {
            let span = span.clone();
            async move {
            if finished {
                return None;
            }

            // 使用 select! 同时等待数据和 ping 定时器
            tokio::select! {
                // 处理数据流
                chunk_result = body_stream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            if !first_byte_logged {
                                first_byte_logged = true;
                                span.in_scope(|| tracing::info!(
                                    event = "stream_first_byte",
                                    upstream_first_byte_ms = stream_started.elapsed().as_millis() as u64,
                                    "流式上游首字节到达"
                                ));
                            }
                            // 解码事件
                            if let Err(e) = decoder.feed(&chunk) {
                                tracing::warn!("缓冲区溢出: {}", e);
                            }

                            let mut events = Vec::new();
                            for result in decoder.decode_iter() {
                                match result {
                                    Ok(frame) => {
                                        if let Ok(event) = Event::from_frame(frame) {
                                            let sse_events = ctx.process_kiro_event(&event);
                                            events.extend(sse_events);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("解码事件失败: {}", e);
                                    }
                                }
                            }

                            // 转换为 SSE 字节流
                            let bytes: Vec<Result<Bytes, Infallible>> = events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval, first_byte_logged, ping_count)))
                        }
                        Some(Err(e)) => {
                            span.in_scope(|| tracing::error!(
                                event = "stream_end",
                                reason = "upstream_error",
                                elapsed_ms = stream_started.elapsed().as_millis() as u64,
                                ping_count = ping_count,
                                error = %e,
                                "流式读取上游失败"
                            ));
                            // 发送最终事件并结束
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval, first_byte_logged, ping_count)))
                        }
                        None => {
                            span.in_scope(|| tracing::info!(
                                event = "stream_end",
                                reason = "upstream_done",
                                elapsed_ms = stream_started.elapsed().as_millis() as u64,
                                ping_count = ping_count,
                                "流式正常结束"
                            ));
                            // 流结束，发送最终事件
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval, first_byte_logged, ping_count)))
                        }
                    }
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    ping_count += 1;
                    tracing::trace!("发送 ping 保活事件");
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval, first_byte_logged, ping_count)))
                }
            }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing_stream)
}

use super::converter::get_context_window_size;

/// 处理非流式请求
async fn handle_non_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    cache_profile: Option<&crate::anthropic::cache_tracker::CacheProfile>,
    cache_tracker: Option<&std::sync::Arc<crate::anthropic::cache_tracker::CacheTracker>>,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
    cache_optimizer: std::sync::Arc<
        parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>,
    >,
    log_ctx: CallLogContext,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            // 记录失败的调用日志（未选到可用凭据）
            log_ctx.record(Some(provider.as_ref()), None, false, false);
            return map_provider_error(e, input_tokens);
        }
    };
    // 记录调用日志：已选中凭据并连上上游
    log_ctx.record(
        Some(provider.as_ref()),
        Some(api_result.credential_id),
        api_result.session_affinity_hit,
        true,
    );
    // 并发槽位守卫：显式持有到本函数返回（body 读完、响应构建完毕）后再 drop。
    // 非流式无 stream_end，靠此 binding 的作用域保证槽位不被提前释放。
    let _slot_guard = api_result.slot_guard;

    let final_cache_context = match (cache_tracker, cache_profile) {
        (Some(tracker), Some(profile)) => {
            let resolved = compute_cache_usage(tracker.as_ref(), api_result.credential_id, profile);
            tracing::info!(
                credential_id = api_result.credential_id,
                final_cache_creation_input_tokens = resolved.cache_creation_input_tokens,
                final_cache_read_input_tokens = resolved.cache_read_input_tokens,
                "Resolved cache usage for non-stream request"
            );
            tracker.update(api_result.credential_id, profile);
            Some(resolved)
        }
        _ => None,
    };

    // 读取响应体
    let body_bytes = match api_result.response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("读取响应体失败: {}", e);
            return (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse::new(
                    "api_error",
                    format!("读取响应失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    // 解析事件流
    let mut decoder = EventStreamDecoder::new();
    if let Err(e) = decoder.feed(&body_bytes) {
        tracing::warn!("缓冲区溢出: {}", e);
    }

    let mut text_content = String::new();
    let mut tool_uses: Vec<serde_json::Value> = Vec::new();
    let mut has_tool_use = false;
    let mut stop_reason = "end_turn".to_string();
    // 从 contextUsageEvent 计算的实际输入 tokens
    let mut context_input_tokens: Option<i32> = None;
    let mut upstream_token_usage: Option<crate::kiro::model::events::TokenUsage> = None;

    // 收集工具调用的增量 JSON
    let mut tool_json_buffers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => {
                if let Ok(event) = Event::from_frame(frame) {
                    match event {
                        Event::AssistantResponse(resp) => {
                            text_content.push_str(&resp.content);
                        }
                        Event::ToolUse(tool_use) => {
                            has_tool_use = true;

                            // 累积工具的 JSON 输入
                            let buffer = tool_json_buffers
                                .entry(tool_use.tool_use_id.clone())
                                .or_insert_with(String::new);
                            buffer.push_str(&tool_use.input);

                            // 如果是完整的工具调用，添加到列表
                            if tool_use.stop {
                                let input: serde_json::Value = if buffer.is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(buffer).unwrap_or_else(|e| {
                                        tracing::warn!(
                                            "工具输入 JSON 解析失败: {}, tool_use_id: {}",
                                            e,
                                            tool_use.tool_use_id
                                        );
                                        serde_json::json!({})
                                    })
                                };

                                let original_name = tool_name_map
                                    .get(&tool_use.name)
                                    .cloned()
                                    .unwrap_or_else(|| tool_use.name.clone());

                                tool_uses.push(json!({
                                    "type": "tool_use",
                                    "id": tool_use.tool_use_id,
                                    "name": original_name,
                                    "input": input
                                }));
                            }
                        }
                        Event::ContextUsage(context_usage) => {
                            if upstream_token_usage
                                .as_ref()
                                .and_then(|usage| usage.input_tokens())
                                .is_some()
                            {
                                if context_usage.context_usage_percentage >= 100.0 {
                                    stop_reason = "model_context_window_exceeded".to_string();
                                }
                                tracing::debug!(
                                    "收到 contextUsageEvent: {}%, 已使用 tokenUsage 真实输入统计，跳过反推",
                                    context_usage.context_usage_percentage
                                );
                                continue;
                            }

                            // 从上下文使用百分比计算实际的 input_tokens
                            let window_size = get_context_window_size(model);
                            let actual_input_tokens =
                                (context_usage.context_usage_percentage * (window_size as f64)
                                    / 100.0) as i32;
                            context_input_tokens = Some(actual_input_tokens);
                            // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                            if context_usage.context_usage_percentage >= 100.0 {
                                stop_reason = "model_context_window_exceeded".to_string();
                            }
                            tracing::debug!(
                                "收到 contextUsageEvent: {}%, 计算 input_tokens: {}",
                                context_usage.context_usage_percentage,
                                actual_input_tokens
                            );
                        }
                        Event::MessageMetadata(metadata) => {
                            if let Some(token_usage) = metadata.token_usage {
                                tracing::info!(
                                    upstream_input_tokens = token_usage.input_tokens(),
                                    upstream_output_tokens = token_usage.output_tokens,
                                    upstream_cache_write_input_tokens =
                                        token_usage.cache_write_tokens(),
                                    upstream_cache_read_input_tokens =
                                        token_usage.cache_read_tokens(),
                                    "Received upstream Kiro tokenUsage for non-stream request"
                                );
                                upstream_token_usage = Some(token_usage);
                            }
                        }
                        Event::Exception { exception_type, .. } => {
                            if exception_type == "ContentLengthExceededException" {
                                stop_reason = "max_tokens".to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::warn!("解码事件失败: {}", e);
            }
        }
    }

    // 确定 stop_reason
    if has_tool_use && stop_reason == "end_turn" {
        stop_reason = "tool_use".to_string();
    }

    // 构建响应内容
    let mut content: Vec<serde_json::Value> = Vec::new();

    if thinking_enabled {
        // 从完整文本中提取 thinking 块
        let (thinking, remaining_text) =
            super::stream::extract_thinking_from_complete_text(&text_content);

        if let Some(thinking_text) = thinking {
            content.push(json!({
                "type": "thinking",
                "thinking": thinking_text
            }));
        }

        if !remaining_text.is_empty() {
            content.push(json!({
                "type": "text",
                "text": remaining_text
            }));
        }
    } else if !text_content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": text_content
        }));
    }

    content.extend(tool_uses);

    let upstream_input_tokens = upstream_token_usage
        .as_ref()
        .and_then(|usage| usage.input_tokens());

    if let Some(percentage) = upstream_token_usage
        .as_ref()
        .and_then(|usage| usage.context_usage_percentage)
        && percentage >= 100.0
    {
        stop_reason = "model_context_window_exceeded".to_string();
    }

    // 优先使用 Kiro tokenUsage 返回的真实输出 tokens，否则估算。
    let output_tokens = upstream_token_usage
        .as_ref()
        .and_then(|usage| usage.output_tokens)
        .unwrap_or_else(|| token::estimate_output_tokens(&content));

    // 优先使用 Kiro tokenUsage，其次 contextUsageEvent，最后使用本地估算。
    let final_input_tokens = upstream_input_tokens
        .or(context_input_tokens)
        .unwrap_or(input_tokens);
    let final_cache_context = upstream_token_usage
        .as_ref()
        .and_then(upstream_cache_context_from_token_usage)
        .or_else(|| {
            final_cache_context
                .map(|ctx| scale_cache_context(ctx, input_tokens, final_input_tokens))
        });
    let billed_input_tokens = final_cache_context
        .map(|ctx| {
            billed_input_tokens(
                final_input_tokens,
                ctx.cache_creation_input_tokens,
                ctx.cache_read_input_tokens,
            )
        })
        .unwrap_or(final_input_tokens);

    // 构建 Anthropic 响应
    let response_body = {
        let mut usage = json!({
            "input_tokens": billed_input_tokens,
            "output_tokens": output_tokens
        });
        // 探活豁免：请求输入过小（如渠道探活）时，完全不改写，原样真实返回
        let bypass = super::cache_rewriter::should_bypass_for_probe(
            &cache_optimizer.read(),
            super::cache_rewriter::ResponsePath::NonStream,
            input_tokens,
        );
        if let Some(mut cache_context) = final_cache_context {
            if bypass {
                // 豁免：cache 字段原样真实返回，不改写、不放大
                inject_cache_usage_fields(&mut usage, cache_context);
            } else {
                // 如果模拟缓存开启，改写 cache 字段（含 5m/1h 拆分同步）
                let optimizer_config = cache_optimizer.read();
                let (new_read, new_write, new_5m, new_1h) =
                    super::cache_rewriter::rewrite_cache_usage_with_split(
                        cache_context.cache_read_input_tokens,
                        cache_context.cache_creation_input_tokens,
                        cache_context.cache_creation_5m_input_tokens,
                        cache_context.cache_creation_1h_input_tokens,
                        &optimizer_config,
                        super::cache_rewriter::ResponsePath::NonStream,
                    );
                // 按上游真实输入分档放大读/写缓存
                let (new_read, new_write, new_5m, new_1h) =
                    super::cache_rewriter::apply_input_scale(
                        new_read,
                        new_write,
                        new_5m,
                        new_1h,
                        final_input_tokens,
                        &optimizer_config,
                    );
                cache_context.cache_read_input_tokens = new_read;
                cache_context.cache_creation_input_tokens = new_write;
                cache_context.cache_creation_5m_input_tokens = new_5m;
                cache_context.cache_creation_1h_input_tokens = new_1h;
                inject_cache_usage_fields(&mut usage, cache_context);
            }
        }

        // 如果模拟缓存开启且配置了 input 随机上限，替换 input_tokens（豁免时不替换）
        if !bypass {
            if let Some(new_input) = super::cache_rewriter::rewrite_input_tokens(
                &cache_optimizer.read(),
                super::cache_rewriter::ResponsePath::NonStream,
            ) {
                usage["input_tokens"] = json!(new_input);
            }
        }

        json!({
            "id": format!("msg_{}", Uuid::new_v4().to_string().replace('-', "")),
            "type": "message",
            "role": "assistant",
            "content": content,
            "model": model,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": usage
        })
    };

    (StatusCode::OK, Json(response_body)).into_response()
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
///
/// - Opus 4.6/4.8：覆写为 adaptive 类型
/// - 其他模型：覆写为 enabled 类型
/// - budget_tokens 固定为 20000
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let model_lower = payload.model.to_lowercase();
    if !model_lower.contains("thinking") {
        return;
    }

    let is_opus_adaptive_thinking = model_lower.contains("opus")
        && (model_lower.contains("4-6")
            || model_lower.contains("4.6")
            || model_lower.contains("4-8")
            || model_lower.contains("4.8"));

    let thinking_type = if is_opus_adaptive_thinking {
        "adaptive"
    } else {
        "enabled"
    };

    tracing::info!(
        model = %payload.model,
        thinking_type = thinking_type,
        "模型名包含 thinking 后缀，覆写 thinking 配置"
    );

    payload.thinking = Some(Thinking {
        thinking_type: thinking_type.to_string(),
        budget_tokens: 20000,
    });

    if is_opus_adaptive_thinking {
        payload.output_config = Some(OutputConfig {
            effort: "high".to_string(),
        });
    }
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量
#[tracing::instrument(
    skip_all,
    fields(request_id = %new_request_id(), route = "/v1/messages/count_tokens")
)]
pub async fn count_tokens(
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> impl IntoResponse {
    tracing::info!(
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages/count_tokens request"
    );

    let total_tokens = token::count_all_tokens(
        payload.model,
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
}

/// POST /cc/v1/messages
///
/// Claude Code 兼容端点，与 /v1/messages 的区别在于：
/// - 流式响应会等待 kiro 端返回 contextUsageEvent 后再发送 message_start
/// - message_start 中的 input_tokens 是从 contextUsageEvent 计算的准确值
#[tracing::instrument(
    skip_all,
    fields(request_id = %new_request_id(), route = "/cc/v1/messages")
)]
pub async fn post_messages_cc(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /cc/v1/messages request"
    );

    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 应用模型映射（内部逻辑不变）。日志改为请求完成后在出口记录。
    let is_stream = payload.stream;
    let (downstream_model, upstream_model, mapped) = apply_model_mapping(&state, &mut payload);
    let mut log_ctx = CallLogContext {
        call_log: state.call_log.clone(),
        downstream_model,
        upstream_model,
        mapped,
        stream: is_stream,
        endpoint: "/cc/v1".to_string(),
        client_ip: extract_client_ip(&headers),
        client_host: extract_client_host(&headers),
        conversation_id: None,        // 在 request_body(kiro格式) 就绪后补充
        conversation_id_source: None, // 在转换后从 ConversionResult 补充
    };

    let prompt_cache = state.prompt_cache_snapshot();

    // 估算输入 tokens，cache 记账需要在 payload 被移动前完成。
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;

    let cache_profile = prompt_cache
        .accounting_enabled
        .then(|| build_cache_profile(prompt_cache.tracker.as_ref(), &payload, input_tokens));
    let provisional_cache_context = cache_profile
        .as_ref()
        .map(|profile| compute_cache_usage(prompt_cache.tracker.as_ref(), 0, profile))
        .unwrap_or_default();
    tracing::info!(
        provisional_cache_creation_input_tokens =
            provisional_cache_context.cache_creation_input_tokens,
        provisional_cache_read_input_tokens = provisional_cache_context.cache_read_input_tokens,
        cache_accounting_enabled = prompt_cache.accounting_enabled,
        prompt_cache_ttl_seconds = prompt_cache.ttl_seconds,
        "Computed provisional cache usage for /cc/v1/messages"
    );

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");
        let resp = websearch::handle_websearch_request(provider, &payload, input_tokens).await;
        log_ctx.record(None, None, false, resp.status().is_success());
        return resp;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload, Some(&build_session_hint(&headers))) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
        }
    };
    // 记录 conversationId 来源（供调用日志展示）
    log_ctx.conversation_id_source = Some(conversion_result.conversation_id_source.to_string());

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // request_body(kiro格式) 就绪后提取 conversationId 补入日志上下文
    log_ctx.conversation_id = extract_conversation_id(&request_body);

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;

    if payload.stream {
        // 流式响应（缓冲模式）
        handle_stream_request_buffered(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            cache_profile.as_ref(),
            prompt_cache
                .accounting_enabled
                .then_some(&prompt_cache.tracker),
            thinking_enabled,
            tool_name_map,
            state.cache_optimizer.clone(),
            log_ctx,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            cache_profile.as_ref(),
            prompt_cache
                .accounting_enabled
                .then_some(&prompt_cache.tracker),
            extract_thinking,
            tool_name_map,
            state.cache_optimizer.clone(),
            log_ctx,
        )
        .await
    }
}

/// 处理流式请求（缓冲版本）
///
/// 与 `handle_stream_request` 不同，此函数会缓冲所有事件直到流结束，
/// 然后用从 contextUsageEvent 计算的正确 input_tokens 生成 message_start 事件。
async fn handle_stream_request_buffered(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    estimated_input_tokens: i32,
    cache_profile: Option<&crate::anthropic::cache_tracker::CacheProfile>,
    cache_tracker: Option<&std::sync::Arc<crate::anthropic::cache_tracker::CacheTracker>>,
    thinking_enabled: bool,
    tool_name_map: std::collections::HashMap<String, String>,
    cache_optimizer: std::sync::Arc<
        parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>,
    >,
    log_ctx: CallLogContext,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            log_ctx.record(Some(provider.as_ref()), None, false, false);
            return map_provider_error(e, estimated_input_tokens);
        }
    };
    log_ctx.record(
        Some(provider.as_ref()),
        Some(api_result.credential_id),
        api_result.session_affinity_hit,
        true,
    );

    let final_cache_context = match (cache_tracker, cache_profile) {
        (Some(tracker), Some(profile)) => {
            let resolved = compute_cache_usage(tracker.as_ref(), api_result.credential_id, profile);
            tracing::info!(
                credential_id = api_result.credential_id,
                final_cache_creation_input_tokens = resolved.cache_creation_input_tokens,
                final_cache_read_input_tokens = resolved.cache_read_input_tokens,
                "Resolved cache usage for buffered stream request"
            );
            tracker.update(api_result.credential_id, profile);
            Some(resolved)
        }
        _ => None,
    };
    let final_cache_usage = final_cache_context.map(|ctx| CacheUsageBreakdown {
        cache_creation_input_tokens: ctx.cache_creation_input_tokens,
        cache_read_input_tokens: ctx.cache_read_input_tokens,
        cache_creation_5m_input_tokens: ctx.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: ctx.cache_creation_1h_input_tokens,
        cache_creation_ttl_known: ctx.cache_creation_ttl_known,
        prefix_hit_input_jitter: ctx.prefix_hit_input_jitter,
    });

    // 创建缓冲流处理上下文
    let mut ctx = BufferedStreamContext::new(
        model,
        estimated_input_tokens,
        final_cache_usage,
        thinking_enabled,
        tool_name_map,
    );
    ctx.set_cache_optimizer(cache_optimizer);
    // 并发槽位守卫随 BufferedStreamContext 持有到 stream_end 后 drop
    ctx.set_slot_guard(api_result.slot_guard);

    // 创建缓冲 SSE 流
    let stream = create_buffered_sse_stream(api_result.response, ctx);

    // 返回 SSE 响应
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// 创建缓冲 SSE 事件流
///
/// 工作流程：
/// 1. 等待上游流完成，期间只发送 ping 保活信号
/// 2. 使用 StreamContext 的事件处理逻辑处理所有 Kiro 事件，结果缓存
/// 3. 流结束后，用正确的 input_tokens 更正 message_start 事件
/// 4. 一次性发送所有事件
fn create_buffered_sse_stream(
    response: reqwest::Response,
    ctx: BufferedStreamContext,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let body_stream = response.bytes_stream();

    // 捕获当前请求 span（含 request_id），打点时用 in_scope 临时进入。
    let span = tracing::Span::current();
    let stream_started = std::time::Instant::now();

    stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            false,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
            false,
            0u64,
        ),
        move |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval, mut first_byte_logged, mut ping_count)| {
            let span = span.clone();
            async move {
            if finished {
                return None;
            }

            loop {
                tokio::select! {
                    // 使用 biased 模式，优先检查 ping 定时器
                    // 避免在上游 chunk 密集时 ping 被"饿死"
                    biased;

                    // 优先检查 ping 保活（等待期间唯一发送的数据）
                    _ = ping_interval.tick() => {
                        ping_count += 1;
                        tracing::trace!("发送 ping 保活事件（缓冲模式）");
                        let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                        return Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval, first_byte_logged, ping_count)));
                    }

                    // 然后处理数据流
                    chunk_result = body_stream.next() => {
                        match chunk_result {
                            Some(Ok(chunk)) => {
                                if !first_byte_logged {
                                    first_byte_logged = true;
                                    span.in_scope(|| tracing::info!(
                                        event = "stream_first_byte",
                                        upstream_first_byte_ms = stream_started.elapsed().as_millis() as u64,
                                        "流式上游首字节到达（缓冲模式）"
                                    ));
                                }
                                // 解码事件
                                if let Err(e) = decoder.feed(&chunk) {
                                    tracing::warn!("缓冲区溢出: {}", e);
                                }

                                for result in decoder.decode_iter() {
                                    match result {
                                        Ok(frame) => {
                                            if let Ok(event) = Event::from_frame(frame) {
                                                // 缓冲事件（复用 StreamContext 的处理逻辑）
                                                ctx.process_and_buffer(&event);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("解码事件失败: {}", e);
                                        }
                                    }
                                }
                                // 继续读取下一个 chunk，不发送任何数据
                            }
                            Some(Err(e)) => {
                                span.in_scope(|| tracing::error!(
                                    event = "stream_end",
                                    reason = "upstream_error",
                                    elapsed_ms = stream_started.elapsed().as_millis() as u64,
                                    ping_count = ping_count,
                                    error = %e,
                                    "流式读取上游失败（缓冲模式）"
                                ));
                                // 发生错误，完成处理并返回所有事件
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval, first_byte_logged, ping_count)));
                            }
                            None => {
                                span.in_scope(|| tracing::info!(
                                    event = "stream_end",
                                    reason = "upstream_done",
                                    elapsed_ms = stream_started.elapsed().as_millis() as u64,
                                    ping_count = ping_count,
                                    "流式正常结束（缓冲模式）"
                                ));
                                // 流结束，完成处理并返回所有事件（已更正 input_tokens）
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval, first_byte_logged, ping_count)));
                            }
                        }
                    }
                }
            }
            }
        },
    )
    .flatten()
}

#[cfg(test)]
mod ops_cache_tests {
    use super::*;
    use crate::model::config::CacheOptimizerConfig;

    // 复现非流式出口(handle_non_stream_request 构建 usage 的那段)的改写逻辑：
    // rewrite_cache_usage_with_split + inject_cache_usage_fields。
    // 验证开启时改写生效、5m/1h 同步；关闭时原样透传。
    fn build_nonstream_usage(
        mut cache_context: CacheUsageContext,
        config: &CacheOptimizerConfig,
    ) -> serde_json::Value {
        let mut usage = serde_json::json!({ "input_tokens": 123, "output_tokens": 5 });
        let (new_read, new_write, new_5m, new_1h) =
            crate::anthropic::cache_rewriter::rewrite_cache_usage_with_split(
                cache_context.cache_read_input_tokens,
                cache_context.cache_creation_input_tokens,
                cache_context.cache_creation_5m_input_tokens,
                cache_context.cache_creation_1h_input_tokens,
                config,
                crate::anthropic::cache_rewriter::ResponsePath::NonStream,
            );
        cache_context.cache_read_input_tokens = new_read;
        cache_context.cache_creation_input_tokens = new_write;
        cache_context.cache_creation_5m_input_tokens = new_5m;
        cache_context.cache_creation_1h_input_tokens = new_1h;
        inject_cache_usage_fields(&mut usage, cache_context);
        usage
    }

    #[test]
    fn test_nonstream_rewrites_cache_usage() {
        let cache_context = CacheUsageContext {
            cache_creation_input_tokens: 480_000,
            cache_read_input_tokens: 150_000,
            cache_creation_5m_input_tokens: 480_000,
            cache_creation_1h_input_tokens: 0,
            cache_creation_ttl_known: true,
            prefix_hit_input_jitter: 0,
        };
        let config = CacheOptimizerConfig {
            enabled: true,
            enabled_stream: true,
            enabled_non_stream: true,
            enabled_buffered: true,
            mode: "cap".to_string(),
            read_max: 165_000,
            write_max: 22_000,
            ..Default::default()
        };
        let usage = build_nonstream_usage(cache_context, &config);
        assert_eq!(
            usage["cache_creation_input_tokens"], 22_000,
            "写 cap 到 22000"
        );
        assert_eq!(
            usage["cache_read_input_tokens"], 150_000,
            "读 150000 < 165000 不变"
        );
        assert_eq!(
            usage["cache_creation"]["ephemeral_5m_input_tokens"], 22_000,
            "5m 同步"
        );
        assert_eq!(usage["cache_creation"]["ephemeral_1h_input_tokens"], 0);
    }

    #[test]
    fn test_nonstream_disabled_passes_through() {
        let cache_context = CacheUsageContext {
            cache_creation_input_tokens: 480_000,
            cache_read_input_tokens: 150_000,
            cache_creation_5m_input_tokens: 300_000,
            cache_creation_1h_input_tokens: 180_000,
            cache_creation_ttl_known: true,
            prefix_hit_input_jitter: 0,
        };
        // 关闭：即便配了极小上限也不应改写。
        let config = CacheOptimizerConfig {
            enabled: false,
            mode: "cap".to_string(),
            read_max: 1,
            write_max: 1,
            ..Default::default()
        };
        let usage = build_nonstream_usage(cache_context, &config);
        assert_eq!(usage["cache_creation_input_tokens"], 480_000);
        assert_eq!(usage["cache_read_input_tokens"], 150_000);
        assert_eq!(
            usage["cache_creation"]["ephemeral_5m_input_tokens"],
            300_000
        );
        assert_eq!(
            usage["cache_creation"]["ephemeral_1h_input_tokens"],
            180_000
        );
    }

    #[test]
    fn test_nonstream_weighted_in_range() {
        use crate::model::config::CacheSegment;
        // 非流式 + weighted（贴近正式配置）：跑多次，断言输出 usage 落在范围且 5m/1h 同步。
        let config = CacheOptimizerConfig {
            enabled: true,
            enabled_stream: true,
            enabled_non_stream: true,
            enabled_buffered: true,
            mode: "weighted".to_string(),
            read_min: 15_000,
            read_max: 165_000,
            write_min: 5,
            write_max: 22_000,
            weight_read_only: 12,
            weight_write_only: 8,
            weight_read_write: 90,
            weight_none: 0,
            rewrite_only_when_present: true,
            use_segment_weights: true,
            read_segments: vec![
                CacheSegment {
                    min: 15_000,
                    max: 70_000,
                    weight: 18,
                },
                CacheSegment {
                    min: 70_001,
                    max: 110_000,
                    weight: 52,
                },
                CacheSegment {
                    min: 110_001,
                    max: 165_000,
                    weight: 30,
                },
            ],
            write_segments: vec![
                CacheSegment {
                    min: 5,
                    max: 800,
                    weight: 72,
                },
                CacheSegment {
                    min: 801,
                    max: 6500,
                    weight: 24,
                },
                CacheSegment {
                    min: 6501,
                    max: 22_000,
                    weight: 4,
                },
            ],
            ..Default::default()
        };
        for _ in 0..200 {
            let cache_context = CacheUsageContext {
                cache_creation_input_tokens: 480_000,
                cache_read_input_tokens: 150_000,
                cache_creation_5m_input_tokens: 480_000,
                cache_creation_1h_input_tokens: 0,
                cache_creation_ttl_known: true,
                prefix_hit_input_jitter: 0,
            };
            let usage = build_nonstream_usage(cache_context, &config);
            let creation = usage["cache_creation_input_tokens"].as_i64().unwrap();
            let read = usage["cache_read_input_tokens"].as_i64().unwrap();
            assert!(
                creation == 0 || (5..=22_000).contains(&creation),
                "creation {creation} 越界"
            );
            assert!(
                read == 0 || (15_000..=165_000).contains(&read),
                "read {read} 越界"
            );
            assert_ne!(creation, 480_000, "写未被改写");
            let m5 = usage["cache_creation"]["ephemeral_5m_input_tokens"]
                .as_i64()
                .unwrap_or(0);
            let h1 = usage["cache_creation"]["ephemeral_1h_input_tokens"]
                .as_i64()
                .unwrap_or(0);
            assert_eq!(m5 + h1, creation, "5m+1h 必须等于写总值");
        }
    }

    #[test]
    fn test_extract_client_ip_priority_and_filtering() {
        use axum::http::HeaderMap;
        // CF-Connecting-IP 最高优先
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        h.insert("x-real-ip", "5.6.7.8".parse().unwrap());
        h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
        assert_eq!(extract_client_ip(&h), Some("1.2.3.4".to_string()));

        // 无 CF 时用 X-Real-IP
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", "5.6.7.8".parse().unwrap());
        h.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
        assert_eq!(extract_client_ip(&h), Some("5.6.7.8".to_string()));

        // XFF 跳过私有，取第一个公网
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            "10.0.0.1, 192.168.1.1, 203.0.113.7".parse().unwrap(),
        );
        assert_eq!(extract_client_ip(&h), Some("203.0.113.7".to_string()));

        // XFF 去端口
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9:54321".parse().unwrap());
        assert_eq!(extract_client_ip(&h), Some("203.0.113.9".to_string()));

        // XFF 全私有 → 取第一个
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());
        assert_eq!(extract_client_ip(&h), Some("10.0.0.1".to_string()));

        // 都没有 → None
        assert_eq!(extract_client_ip(&HeaderMap::new()), None);
    }

    #[test]
    fn test_normalize_and_private_ip() {
        assert_eq!(normalize_ip("1.2.3.4:8080"), "1.2.3.4");
        assert_eq!(normalize_ip("  5.6.7.8  "), "5.6.7.8");
        assert!(is_private_ip("10.0.0.1"));
        assert!(is_private_ip("192.168.1.1"));
        assert!(is_private_ip("172.16.5.5"));
        assert!(is_private_ip("127.0.0.1"));
        assert!(!is_private_ip("203.0.113.7"));
        assert!(!is_private_ip("8.8.8.8"));
    }
}
