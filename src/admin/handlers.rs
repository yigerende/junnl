//! Admin API HTTP 处理器

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};

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

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
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
