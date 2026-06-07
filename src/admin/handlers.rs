//! Admin API HTTP 处理器

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::Stream;
use std::convert::Infallible;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, SetDisabledRequest, SetLoadBalancingModeRequest, SetPriorityRequest,
        SuccessResponse,
    },
};
use crate::model::config::{CacheOptimizerConfig, ModelMappingConfig};

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// 余额查询参数
#[derive(serde::Deserialize)]
pub struct BalanceQuery {
    /// fresh=true 时跳过缓存强制拉上游（用于单独测活）
    #[serde(default)]
    pub fresh: bool,
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额。?fresh=true 强制跳过缓存（单独测活）。
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Query(q): Query<BalanceQuery>,
) -> impl IntoResponse {
    let result = if q.fresh {
        state.service.get_balance_fresh(id).await
    } else {
        state.service.get_balance(id).await
    };
    match result {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/balances/cached
/// 返回所有缓存的余额（只读，不请求上游），供前端进页面立即展示。
pub async fn get_cached_balances(State(state): State<AdminState>) -> impl IntoResponse {
    let balances = state.service.get_cached_balances();
    Json(serde_json::json!({ "balances": balances }))
}

/// 设置超额开关请求体
#[derive(serde::Deserialize)]
pub struct SetOverageRequest {
    pub enabled: bool,
}

/// POST /api/admin/credentials/:id/overage
/// 开启/关闭该凭据对应账号的超额计费（调上游 setUserPreference）。
/// 成功返回该凭据最新余额（含改写后的超额状态）。
pub async fn set_credential_overage(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetOverageRequest>,
) -> impl IntoResponse {
    match state.service.set_overage(id, payload.enabled).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/load-balancing
/// 获取负载均衡模式
pub async fn get_load_balancing_mode(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_load_balancing_mode();
    Json(response)
}

/// PUT /api/admin/config/load-balancing
/// 设置负载均衡模式
pub async fn set_load_balancing_mode(
    State(state): State<AdminState>,
    Json(payload): Json<SetLoadBalancingModeRequest>,
) -> impl IntoResponse {
    match state.service.set_load_balancing_mode(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/cache-optimizer
/// 获取模拟缓存配置
pub async fn get_cache_optimizer(State(state): State<AdminState>) -> impl IntoResponse {
    let config = state.service.get_cache_optimizer();
    Json(serde_json::json!({ "config": config }))
}

/// PUT /api/admin/cache-optimizer
/// 更新模拟缓存配置
pub async fn set_cache_optimizer(
    State(state): State<AdminState>,
    Json(payload): Json<CacheOptimizerConfig>,
) -> impl IntoResponse {
    match state.service.set_cache_optimizer(payload) {
        Ok(config) => Json(serde_json::json!({ "config": config })).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/model-mapping
/// 获取模型映射配置
pub async fn get_model_mapping(State(state): State<AdminState>) -> impl IntoResponse {
    let config = state.service.get_model_mapping();
    Json(serde_json::json!({ "config": config }))
}

/// PUT /api/admin/model-mapping
/// 更新模型映射配置
pub async fn set_model_mapping(
    State(state): State<AdminState>,
    Json(payload): Json<ModelMappingConfig>,
) -> impl IntoResponse {
    match state.service.set_model_mapping(payload) {
        Ok(config) => Json(serde_json::json!({ "config": config })).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/available-models
/// 拉取上游可用模型 ID 列表（供前端选择映射目标）
pub async fn get_available_models(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.list_available_models().await {
        Ok(models) => Json(serde_json::json!({ "models": models })).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// 调用日志查询参数
#[derive(serde::Deserialize)]
pub struct CallLogQuery {
    /// 返回条数上限（默认 1000）
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /api/admin/call-logs?limit=N
/// 获取调用日志（最新在前）
pub async fn get_call_logs(
    State(state): State<AdminState>,
    Query(q): Query<CallLogQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(1000).clamp(1, 100_000);
    let logs = state.service.get_call_logs(limit);
    let capacity = state.service.get_call_log_capacity();
    Json(serde_json::json!({ "logs": logs, "capacity": capacity }))
}

/// DELETE /api/admin/call-logs
/// 清空调用日志
pub async fn clear_call_logs(State(state): State<AdminState>) -> impl IntoResponse {
    state.service.clear_call_logs();
    Json(SuccessResponse::new("调用日志已清空".to_string()))
}

/// 设置容量请求体
#[derive(serde::Deserialize)]
pub struct SetCallLogCapacityRequest {
    pub capacity: usize,
}

/// PUT /api/admin/call-logs/capacity
/// 设置调用日志保留条数
pub async fn set_call_log_capacity(
    State(state): State<AdminState>,
    Json(payload): Json<SetCallLogCapacityRequest>,
) -> impl IntoResponse {
    let applied = state.service.set_call_log_capacity(payload.capacity);
    Json(serde_json::json!({ "capacity": applied }))
}

/// 运行日志拉取参数
#[derive(serde::Deserialize)]
pub struct RuntimeLogQuery {
    pub limit: Option<usize>,
}

/// GET /api/admin/logs?limit=N
/// 拉取最近的运行日志（时间正序）。供前端首次加载/补全历史。
pub async fn get_runtime_logs(Query(q): Query<RuntimeLogQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(5000).clamp(1, 5000);
    let logs = crate::log_buffer::global().recent(limit);
    Json(serde_json::json!({ "logs": logs }))
}

/// GET /api/admin/logs/stream
/// 通过 SSE 实时推送运行日志。前端用 EventSource 订阅，新日志实时滚动。
pub async fn stream_runtime_logs() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let rx = crate::log_buffer::global().subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(record) => {
            // 序列化失败的极端情况直接跳过该条，不中断流。
            serde_json::to_string(&record)
                .ok()
                .map(|json| Ok(Event::default().data(json)))
        }
        // 订阅者落后（Lagged）时丢弃，不中断流。
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// GET /api/admin/logs/info
/// 返回日志落盘信息：目录绝对路径、当天文件名、已有日志文件列表（含大小）。
/// 用于前端展示「日志到底落在哪」，并辅助排查「正式机看不到日志文件」。
pub async fn get_log_info() -> impl IntoResponse {
    let dir = crate::logging::log_dir_absolute();
    let today = crate::logging::today_log_filename();

    // 枚举目录下合法的滚动日志文件（app.YYYY-MM-DD），附带字节大小。
    let mut files: Vec<serde_json::Value> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !crate::logging::is_valid_log_filename(&name) {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(serde_json::json!({ "name": name, "size": size }));
        }
    }
    // 按文件名（即日期）倒序，最新的在前。
    files.sort_by(|a, b| {
        b["name"]
            .as_str()
            .unwrap_or("")
            .cmp(a["name"].as_str().unwrap_or(""))
    });
    let today_exists = files.iter().any(|f| f["name"].as_str() == Some(&today));

    Json(serde_json::json!({
        "dir": dir.to_string_lossy(),
        "today": today,
        "todayExists": today_exists,
        "files": files,
    }))
}

#[derive(serde::Deserialize)]
pub struct LogDownloadQuery {
    /// 指定要下载的日志文件名（app.YYYY-MM-DD）；缺省下载当天。
    pub file: Option<String>,
}

/// GET /api/admin/logs/download?file=app.YYYY-MM-DD
/// 下载指定（默认当天）的落盘日志文件。文件名经严格校验，杜绝路径穿越。
pub async fn download_log_file(Query(q): Query<LogDownloadQuery>) -> impl IntoResponse {
    let name = q
        .file
        .unwrap_or_else(crate::logging::today_log_filename);

    // 严格校验文件名：仅允许 app.YYYY-MM-DD，拒绝任何路径分隔符 / 穿越。
    if !crate::logging::is_valid_log_filename(&name) {
        return (StatusCode::BAD_REQUEST, "非法日志文件名").into_response();
    }

    let dir = crate::logging::log_dir_absolute();
    let path = dir.join(&name);

    let content = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                format!("日志文件不存在：{name}（当天可能尚无日志产生）"),
            )
                .into_response();
        }
    };

    // 落盘为 JSON-lines，下载时用 .log 后缀，附带 Content-Disposition 触发浏览器下载。
    let download_name = format!("{name}.log");
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{download_name}\""),
            ),
        ],
        Body::from(content),
    )
        .into_response()
}
