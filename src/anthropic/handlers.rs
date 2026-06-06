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

use super::converter::{ConversionError, convert_request};
use super::middleware::AppState;
use super::stream::{BufferedStreamContext, CacheUsageBreakdown, SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, ErrorResponse, MessagesRequest, Model, ModelsResponse,
    OutputConfig, Thinking,
};
use super::websearch;

/// 应用模型映射并记录调用日志。
///
/// 把下游请求的模型名替换为上游实际模型名（内部逻辑不变），
/// 同时向调用日志写入一条记录（下游模型、上游模型、是否流式、端点、是否命中映射）。
fn apply_model_mapping_and_log(
    state: &AppState,
    payload: &mut MessagesRequest,
    endpoint: &str,
    stream: bool,
) {
    let downstream = payload.model.clone();
    let upstream = state.model_mapping.read().resolve_alias(&downstream);
    let mapped = upstream != downstream;
    if mapped {
        tracing::info!(from = %downstream, to = %upstream, "模型映射生效");
        payload.model = upstream.clone();
    }
    state.call_log.record(super::call_log::CallLogEntry {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        downstream_model: downstream,
        upstream_model: upstream,
        stream,
        endpoint: endpoint.to_string(),
        mapped,
    });
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
fn map_provider_error(err: Error) -> Response {
    let err_str = err.to_string();

    // 上下文窗口满了（对话历史累积超出模型上下文窗口限制）
    if err_str.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        tracing::warn!(error = %err, "上游拒绝请求：上下文窗口已满（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Context window is full. Reduce conversation history, system prompt, or tools.",
            )),
        )
            .into_response();
    }

    // 单次输入太长（请求体本身超出上游限制）
    if err_str.contains("Input is too long") {
        tracing::warn!(error = %err, "上游拒绝请求：输入过长（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Input is too long. Reduce the size of your messages.",
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
pub async fn post_messages(
    State(state): State<AppState>,
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

    // 应用模型映射 + 记录调用日志（内部逻辑不变）
    let is_stream = payload.stream;
    apply_model_mapping_and_log(&state, &mut payload, "/v1", is_stream);

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

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload) {
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
    cache_optimizer: std::sync::Arc<parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

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

    let processing_stream = stream::unfold(
        (body_stream, ctx, EventStreamDecoder::new(), false, interval(Duration::from_secs(PING_INTERVAL_SECS))),
        |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval)| async move {
            if finished {
                return None;
            }

            // 使用 select! 同时等待数据和 ping 定时器
            tokio::select! {
                // 处理数据流
                chunk_result = body_stream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
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

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)))
                        }
                        Some(Err(e)) => {
                            tracing::error!("读取响应流失败: {}", e);
                            // 发送最终事件并结束
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)))
                        }
                        None => {
                            // 流结束，发送最终事件
                            let final_events = ctx.generate_final_events();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)))
                        }
                    }
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    tracing::trace!("发送 ping 保活事件");
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)))
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
    cache_optimizer: std::sync::Arc<parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

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
        if let Some(mut cache_context) = final_cache_context {
            // 如果模拟缓存开启，改写 cache 字段
            let optimizer_config = cache_optimizer.read();
            let (new_read, new_write) = super::cache_rewriter::rewrite_cache_usage(
                cache_context.cache_read_input_tokens,
                cache_context.cache_creation_input_tokens,
                &optimizer_config,
                super::cache_rewriter::ResponsePath::NonStream,
            );
            cache_context.cache_read_input_tokens = new_read;
            cache_context.cache_creation_input_tokens = new_write;
            inject_cache_usage_fields(&mut usage, cache_context);
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
pub async fn post_messages_cc(
    State(state): State<AppState>,
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

    // 应用模型映射 + 记录调用日志（内部逻辑不变）
    let is_stream = payload.stream;
    apply_model_mapping_and_log(&state, &mut payload, "/cc/v1", is_stream);

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

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request(&payload) {
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
    cache_optimizer: std::sync::Arc<parking_lot::RwLock<crate::model::config::CacheOptimizerConfig>>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_result = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

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

    stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            false,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
        ),
        |(mut body_stream, mut ctx, mut decoder, finished, mut ping_interval)| async move {
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
                        tracing::trace!("发送 ping 保活事件（缓冲模式）");
                        let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                        return Some((stream::iter(bytes), (body_stream, ctx, decoder, false, ping_interval)));
                    }

                    // 然后处理数据流
                    chunk_result = body_stream.next() => {
                        match chunk_result {
                            Some(Ok(chunk)) => {
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
                                tracing::error!("读取响应流失败: {}", e);
                                // 发生错误，完成处理并返回所有事件
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)));
                            }
                            None => {
                                // 流结束，完成处理并返回所有事件（已更正 input_tokens）
                                let all_events = ctx.finish_and_get_all_events();
                                let bytes: Vec<Result<Bytes, Infallible>> = all_events
                                    .into_iter()
                                    .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                    .collect();
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, true, ping_interval)));
                            }
                        }
                    }
                }
            }
        },
    )
    .flatten()
}
