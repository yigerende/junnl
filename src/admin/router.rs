//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};

use super::{
    handlers::{
        add_credential, batch_set_concurrency, clear_call_logs, delete_credential,
        download_log_file, force_refresh_token, get_all_credentials, get_available_models,
        get_cache_optimizer, get_cached_balances, get_call_logs, get_credential_balance,
        get_load_balancing_mode, get_log_info, get_model_mapping, get_runtime_logs,
        reset_failure_count, set_cache_optimizer, set_call_log_capacity,
        set_credential_concurrency, set_credential_disabled, set_credential_overage,
        set_credential_priority, set_load_balancing_mode, set_model_mapping, stream_runtime_logs,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `POST /credentials/:id/refresh` - 强制刷新 Token
/// - `GET /credentials/:id/balance` - 获取凭据余额
/// - `GET /config/load-balancing` - 获取负载均衡模式
/// - `PUT /config/load-balancing` - 设置负载均衡模式
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/{id}", delete(delete_credential))
        .route(
            "/credentials/concurrency/batch",
            post(batch_set_concurrency),
        )
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route(
            "/credentials/{id}/concurrency",
            post(set_credential_concurrency),
        )
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/overage", post(set_credential_overage))
        .route("/balances/cached", get(get_cached_balances))
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
        )
        .route(
            "/cache-optimizer",
            get(get_cache_optimizer).put(set_cache_optimizer),
        )
        .route(
            "/model-mapping",
            get(get_model_mapping).put(set_model_mapping),
        )
        .route("/available-models", get(get_available_models))
        .route("/call-logs", get(get_call_logs).delete(clear_call_logs))
        .route("/call-logs/capacity", put(set_call_log_capacity))
        .route("/logs", get(get_runtime_logs))
        .route("/logs/stream", get(stream_runtime_logs))
        .route("/logs/info", get(get_log_info))
        .route("/logs/download", get(download_log_file))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
