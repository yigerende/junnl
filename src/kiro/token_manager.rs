//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持多凭据 (MultiTokenManager) 管理

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as TokioMutex;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    IdcRefreshRequest, IdcRefreshResponse, RefreshRequest, RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::model::config::Config;

/// 检查 Token 是否在指定时间内过期
pub(crate) fn is_token_expiring_within(
    credentials: &KiroCredentials,
    minutes: i64,
) -> Option<bool> {
    credentials
        .expires_at
        .as_ref()
        .and_then(|expires_at| DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires| expires <= Utc::now() + Duration::minutes(minutes))
}

/// 检查 Token 是否已过期（提前 5 分钟判断）
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 检查 Token 是否即将过期（10分钟内）
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// 生成 API Key 脱敏展示(前 4 + ... + 后 4,长度不足或非 ASCII 回退 ***)
fn mask_api_key(key: &str) -> String {
    if key.is_ascii() && key.len() > 16 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    }
}

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;

    if refresh_token.is_empty() {
        bail!("refreshToken 为空");
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        bail!(
            "refreshToken 已被截断（长度: {} 字符）。\n\
             这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。",
            refresh_token.len()
        );
    }

    Ok(())
}

/// Refresh Token 永久失效错误
///
/// 当服务端返回 400 + `invalid_grant` 时，表示 refreshToken 已被撤销或过期，
/// 不应重试，需立即禁用对应凭据。
#[derive(Debug)]
pub(crate) struct RefreshTokenInvalidError {
    pub message: String,
}

impl fmt::Display for RefreshTokenInvalidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RefreshTokenInvalidError {}

/// 刷新 Token
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    // API Key 凭据不支持 Token 刷新：底层契约级拦截
    // 其他调用点（try_ensure_token / 活跃路径 / add_credential）在调用前已显式分流 API Key；
    // 仅 force_refresh_token_for 未分流，此处 bail 让错误自然传播为 400 BAD_REQUEST。
    if credentials.is_api_key_credential() {
        bail!("API Key 凭据不支持刷新 Token");
    }

    validate_refresh_token(credentials)?;

    // 根据 auth_method 选择刷新方式
    // 如果未指定 auth_method，根据是否有 clientId/clientSecret 自动判断
    let auth_method = credentials.auth_method.as_deref().unwrap_or_else(|| {
        if credentials.client_id.is_some() && credentials.client_secret.is_some() {
            "idc"
        } else {
            "social"
        }
    });

    if auth_method.eq_ignore_ascii_case("idc")
        || auth_method.eq_ignore_ascii_case("builder-id")
        || auth_method.eq_ignore_ascii_case("iam")
    {
        refresh_idc_token(credentials, config, proxy).await
    } else {
        refresh_social_token(credentials, config, proxy).await
    }
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            format!("KiroIDE-{}-{}", kiro_version, machine_id),
        )
        .header("Accept-Encoding", "gzip, compress, deflate, br")
        .header("host", &refresh_domain)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant + Invalid refresh token provided → refreshToken 永久失效
        if status.as_u16() == 400
            && body_text.contains("\"invalid_grant\"")
            && body_text.contains("Invalid refresh token provided")
        {
            return Err(RefreshTokenInvalidError {
                message: format!("Social refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "OAuth 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OAuth 服务暂时不可用",
            _ => "Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: RefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
    }

    Ok(new_credentials)
}

/// 刷新 IdC Token (AWS SSO OIDC)
async fn refresh_idc_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientId"))?;
    let client_secret = credentials
        .client_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientSecret"))?;

    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);
    let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    let x_amz_user_agent = "aws-sdk-js/3.980.0 KiroIDE";
    let user_agent = format!(
        "aws-sdk-js/3.980.0 ua/2.1 os/{} lang/js md/nodejs#{} api/sso-oidc#3.980.0 m/E KiroIDE",
        os_name, node_version
    );

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant + Invalid refresh token provided → refreshToken 永久失效
        if status.as_u16() == 400
            && body_text.contains("\"invalid_grant\"")
            && body_text.contains("Invalid refresh token provided")
        {
            return Err(RefreshTokenInvalidError {
                message: format!("IdC refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "IdC 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OIDC 服务暂时不可用",
            _ => "IdC Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: IdcRefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
    }

    // 同步更新 profile_arn（如果 IdC 响应中包含）
    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    Ok(new_credentials)
}

/// 获取使用额度信息
pub(crate) async fn get_usage_limits(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UsageLimitsResponse> {
    tracing::debug!("正在获取使用额度信息...");

    // 优先级：凭据.api_region > config.api_region > config.region
    let region = credentials.effective_api_region(config);
    let host = format!("q.{}.amazonaws.com", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    // 构建 URL
    let mut url = format!(
        "https://{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true",
        host
    );

    // 最新 Kiro 客户端在缺失 profileArn 时也会按认证方式补默认 ARN。
    if let Some(profile_arn) = credentials.resolved_profile_arn() {
        url.push_str(&format!(
            "&profileArn={}",
            urlencoding::encode(&profile_arn)
        ));
    }

    // 构建 User-Agent headers
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        os_name, node_version, kiro_version, machine_id
    );
    let amz_user_agent = format!("aws-sdk-js/1.0.0 KiroIDE-{}-{}", kiro_version, machine_id);

    let client = build_client(proxy, 60, config.tls_backend)?;

    let mut request = client
        .get(&url)
        .header("x-amz-user-agent", &amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", &host)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Authorization", format!("Bearer {}", token))
        .header("Connection", "close");

    if credentials.is_api_key_credential() {
        request = request.header("tokentype", "API_KEY");
    }

    let response = request.send().await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: UsageLimitsResponse = response.json().await?;
    Ok(data)
}

/// 设置用户偏好（超额开关）。
///
/// 调用上游 `setUserPreference` 接口，改写该 AWS 账号的超额计费开关。
/// `overage_status` 取 "ENABLED" 或 "DISABLED"。
///
/// 复用与 `get_usage_limits` 完全一致的 host / 认证头 / profileArn 处理，
/// 保证操作的是同一个账号。注意：这是改账号真实计费设置的写操作。
pub(crate) async fn set_user_preference(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
    overage_status: &str,
) -> anyhow::Result<()> {
    let region = credentials.effective_api_region(config);
    let host = format!("q.{}.amazonaws.com", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    let url = format!("https://{}/setUserPreference", host);

    let mut body = serde_json::json!({
        "overageConfiguration": { "overageStatus": overage_status },
    });
    if let Some(profile_arn) = credentials.resolved_profile_arn() {
        body["profileArn"] = serde_json::json!(profile_arn);
    }

    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        os_name, node_version, kiro_version, machine_id
    );
    let amz_user_agent = format!("aws-sdk-js/1.0.0 KiroIDE-{}-{}", kiro_version, machine_id);

    let client = build_client(proxy, 60, config.tls_backend)?;

    let mut request = client
        .post(&url)
        .header("x-amz-user-agent", &amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", &host)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Authorization", format!("Bearer {}", token))
        .header("content-type", "application/json")
        .header("Connection", "close")
        .json(&body);

    if credentials.is_api_key_credential() {
        request = request.header("tokentype", "API_KEY");
    }

    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法修改超额设置",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "设置超额开关失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    Ok(())
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

/// 单个凭据条目的状态
struct CredentialEntry {
    /// 凭据唯一 ID
    id: u64,
    /// 凭据信息
    credentials: KiroCredentials,
    /// API 调用连续失败次数
    failure_count: u32,
    /// Token 刷新连续失败次数
    refresh_failure_count: u32,
    /// 是否已禁用
    disabled: bool,
    /// 禁用原因（用于区分手动禁用 vs 自动禁用，便于自愈）
    disabled_reason: Option<DisabledReason>,
    /// API 调用成功次数
    success_count: u64,
    /// API 调用总请求次数（含失败，每次选中即 +1）
    request_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    last_used_at: Option<String>,
    /// 当前在途请求数（选中即 +1，ConcurrencyGuard drop 时 -1）
    ///
    /// 运行时态，不持久化。least-active 选号的主键（balanced）/同档内次键（priority）。
    /// 与 success_count（长期均衡裁决）、request_count（累计统计）完全独立。
    active: u32,
    /// 当前等待该凭据释放槽位的请求数（仅前端显示）
    ///
    /// 运行时态，不持久化。仅在第二层硬上限触发等待时变化。
    waiting: u32,
}

/// 禁用原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisabledReason {
    /// Admin API 手动禁用
    Manual,
    /// 连续失败达到阈值后自动禁用
    TooManyFailures,
    /// Token 刷新连续失败达到阈值后自动禁用
    TooManyRefreshFailures,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
    QuotaExceeded,
    /// Refresh Token 永久失效（服务端返回 invalid_grant）
    InvalidRefreshToken,
    /// 凭据配置无效（如 authMethod=api_key 但缺少 kiroApiKey）
    InvalidConfig,
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    #[serde(default)]
    request_count: u64,
    last_used_at: Option<String>,
}

// ============================================================================
// Admin API 公开结构
// ============================================================================

/// 凭据条目快照（用于 Admin API 读取）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialEntrySnapshot {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 登录 Provider（Google / Github / BuilderId / Enterprise 等）
    pub provider: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// Token 过期时间
    pub expires_at: Option<String>,
    /// refreshToken 的 SHA-256 哈希（仅 OAuth 凭据，用于前端去重）
    pub refresh_token_hash: Option<String>,
    /// kiroApiKey 的 SHA-256 哈希（仅 API Key 凭据，用于前端去重）
    pub api_key_hash: Option<String>,
    /// kiroApiKey 的脱敏展示（仅 API Key 凭据，用于前端显示）
    pub masked_api_key: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// API 调用总请求次数（含失败）
    pub request_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 是否配置了凭据级代理
    pub has_proxy: bool,
    /// 代理 URL（用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// 端点名称（未显式配置时返回 None，由 Admin 层回退到默认值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 并发硬上限（0 = 不限制）
    pub max_concurrency: u32,
    /// 当前在途请求数
    pub active_concurrency: u32,
    /// 当前等待该凭据释放槽位的请求数
    pub waiting_concurrency: u32,
}

/// 凭据管理器状态快照
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSnapshot {
    /// 凭据条目列表
    pub entries: Vec<CredentialEntrySnapshot>,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 总凭据数量
    pub total: usize,
    /// 可用凭据数量
    pub available: usize,
}

/// 多凭据 Token 管理器
///
/// 支持多个凭据的管理，实现固定优先级 + 故障转移策略
/// 故障统计基于 API 调用结果，而非 Token 刷新结果
pub struct MultiTokenManager {
    config: Config,
    proxy: Option<ProxyConfig>,
    /// 凭据条目列表
    entries: Mutex<Vec<CredentialEntry>>,
    /// 当前活动凭据 ID
    current_id: Mutex<u64>,
    /// Token 刷新锁，确保同一时间只有一个刷新操作
    refresh_lock: TokioMutex<()>,
    /// 凭据文件路径（用于回写）
    credentials_path: Option<PathBuf>,
    /// 是否为多凭据格式（数组格式才回写）
    is_multiple_format: bool,
    /// 负载均衡模式（运行时可修改）
    load_balancing_mode: Mutex<String>,
    /// balanced 模式下的会话到凭据绑定，避免同一对话在账号之间来回漂移
    session_affinity: Mutex<HashMap<String, SessionAffinityEntry>>,
    /// 最近一次统计持久化时间（用于 debounce）
    last_stats_save_at: Mutex<Option<Instant>>,
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
    /// 指向自身的 Weak 引用，用于 ConcurrencyGuard 在 Drop 时回到管理器释放槽位。
    /// 在 `new` 之后通过 `Arc::new_cyclic` 等价流程填充。
    self_weak: Mutex<Weak<MultiTokenManager>>,
}

#[derive(Clone)]
struct SessionAffinityEntry {
    credential_id: u64,
    last_used_at: Instant,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;
/// 统计数据持久化防抖间隔
const STATS_SAVE_DEBOUNCE: StdDuration = StdDuration::from_secs(30);
/// 会话亲和有效期，避免长期运行进程积累无限会话 key
const SESSION_AFFINITY_TTL: StdDuration = StdDuration::from_secs(2 * 60 * 60);

/// 第二层兜底：满载等待的单次轮询间隔
const CONCURRENCY_WAIT_POLL: StdDuration = StdDuration::from_millis(80);
/// 第二层兜底：满载等待的总时长上限（超时返回繁忙 429）
const CONCURRENCY_WAIT_BUDGET: StdDuration = StdDuration::from_secs(2);
/// 第二层兜底：粘性会话原号等待数阈值，超过即放弃亲和换号（远小于 sub2 的 3）
const STICKY_MAX_WAITING: u32 = 2;

/// API 调用上下文
///
/// 绑定特定凭据的调用上下文，确保 token、credentials 和 id 的一致性
/// 用于解决并发调用时 current_id 竞态问题
#[derive(Clone)]
pub struct CallContext {
    /// 凭据 ID（用于 report_success/report_failure）
    pub id: u64,
    /// 凭据信息（用于构建请求头）
    pub credentials: KiroCredentials,
    /// 访问 Token
    pub token: String,
    /// 本次是否命中会话亲和（balanced 模式下复用了已绑定凭据）。
    /// 仅用于调用日志展示，不参与任何调度决策。
    pub session_affinity_hit: bool,
}

/// 并发槽位 RAII 守卫
///
/// 在 `acquire_context_for_session` 选中凭据、`active += 1` 的同一处生成，
/// 一路透传到请求生命周期结束（非流式读完 body / 流式读到 stream_end /
/// 客户端断开 / 出错 / panic）。Drop 时持 entries 锁把对应凭据的 `active`
/// 做一次 `saturating_sub(1)`，无需任何手动释放，覆盖全部分支。
///
/// 严禁手动 `active -= 1`，释放只能走 Drop（见方案 §3.2 / §6.1）。
pub struct ConcurrencyGuard {
    manager: Weak<MultiTokenManager>,
    credential_id: u64,
    released: bool,
}

impl ConcurrencyGuard {
    fn new(manager: Weak<MultiTokenManager>, credential_id: u64) -> Self {
        Self {
            manager,
            credential_id,
            released: false,
        }
    }

    /// 占位守卫：不绑定任何凭据，Drop 时不做任何事。
    /// 用于无法获得管理器 Weak 引用的边界场景（理论上不会发生）。
    fn noop() -> Self {
        Self {
            manager: Weak::new(),
            credential_id: 0,
            released: true,
        }
    }
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Some(manager) = self.manager.upgrade() {
            manager.release_active_slot(self.credential_id);
        }
    }
}

/// 选号并占用槽位的内部结果
///
/// 各变体大小差异较大（Reserved 携带 KiroCredentials），但都是选号热路径上的
/// 短命栈值，且 KiroCredentials 在本模块本就到处按值传递（见 CallContext），
/// 装箱反而给热路径引入堆分配，故显式放行该 lint。
#[allow(clippy::large_enum_variant)]
enum ReserveOutcome {
    /// 已选中并占用：凭据 id、凭据信息、是否命中会话亲和、槽位守卫
    Reserved(u64, KiroCredentials, bool, ConcurrencyGuard),
    /// 有未禁用凭据但全部满载（仅在配置了 max_concurrency 时可能）
    /// 附带"打算等待的那个凭据 id"，用于前端 waiting 显示
    AllFull(u64),
    /// 无任何未禁用且模型可用的凭据
    Empty,
}

/// 占用指定凭据槽位的结果
#[allow(clippy::large_enum_variant)]
enum ReserveOne {
    /// 已占用，返回凭据信息与守卫
    Ok(KiroCredentials, ConcurrencyGuard),
    /// 该凭据满载（max_concurrency>0 且 active>=max_concurrency）
    Full,
    /// 该凭据不可用（禁用 / 不存在 / 不支持该模型）
    Unavailable,
}

impl MultiTokenManager {
    /// 创建多凭据 Token 管理器
    ///
    /// # Arguments
    /// * `config` - 应用配置
    /// * `credentials` - 凭据列表
    /// * `proxy` - 可选的代理配置
    /// * `credentials_path` - 凭据文件路径（用于回写）
    /// * `is_multiple_format` - 是否为多凭据格式（数组格式才回写）
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
        // 计算当前最大 ID，为没有 ID 的凭据分配新 ID
        let max_existing_id = credentials.iter().filter_map(|c| c.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;
        let mut has_new_machine_ids = false;
        let config_ref = &config;

        let entries: Vec<CredentialEntry> = credentials
            .into_iter()
            .map(|mut cred| {
                cred.canonicalize_auth_method();
                let id = cred.id.unwrap_or_else(|| {
                    let id = next_id;
                    next_id += 1;
                    cred.id = Some(id);
                    has_new_ids = true;
                    id
                });
                if cred.machine_id.is_none() {
                    cred.machine_id =
                        Some(machine_id::generate_from_credentials(&cred, config_ref));
                    has_new_machine_ids = true;
                }
                CredentialEntry {
                    id,
                    credentials: cred.clone(),
                    failure_count: 0,
                    refresh_failure_count: 0,
                    disabled: cred.disabled, // 从配置文件读取 disabled 状态
                    disabled_reason: if cred.disabled {
                        Some(DisabledReason::Manual)
                    } else {
                        None
                    },
                    success_count: 0,
                    request_count: 0,
                    last_used_at: None,
                    active: 0,
                    waiting: 0,
                }
            })
            .collect();

        // 校验 API Key 凭据配置完整性：authMethod=api_key 时必须提供 kiroApiKey
        let mut entries = entries;
        for entry in &mut entries {
            if entry.credentials.kiro_api_key.is_none()
                && entry
                    .credentials
                    .auth_method
                    .as_deref()
                    .map(|m| m.eq_ignore_ascii_case("api_key") || m.eq_ignore_ascii_case("apikey"))
                    .unwrap_or(false)
            {
                tracing::warn!(
                    "凭据 #{} 配置了 authMethod=api_key 但缺少 kiroApiKey 字段，已自动禁用",
                    entry.id
                );
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::InvalidConfig);
            }
        }

        // 检测重复 ID
        let mut seen_ids = std::collections::HashSet::new();
        let mut duplicate_ids = Vec::new();
        for entry in &entries {
            if !seen_ids.insert(entry.id) {
                duplicate_ids.push(entry.id);
            }
        }
        if !duplicate_ids.is_empty() {
            anyhow::bail!("检测到重复的凭据 ID: {:?}", duplicate_ids);
        }

        // 选择初始凭据：优先级最高（priority 最小）的可用凭据，无可用凭据时为 0
        let initial_id = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
            .map(|e| e.id)
            .unwrap_or(0);

        let load_balancing_mode = config.load_balancing_mode.clone();
        let manager = Self {
            config,
            proxy,
            entries: Mutex::new(entries),
            current_id: Mutex::new(initial_id),
            refresh_lock: TokioMutex::new(()),
            credentials_path,
            is_multiple_format,
            load_balancing_mode: Mutex::new(load_balancing_mode),
            session_affinity: Mutex::new(HashMap::new()),
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            self_weak: Mutex::new(Weak::new()),
        };

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到配置文件
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写回配置文件");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();

        Ok(manager)
    }

    /// 获取配置的引用
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 绑定自身的 Arc 弱引用。
    ///
    /// 必须在 `Arc::new(manager)` 之后、对外提供服务之前调用一次，
    /// 否则 ConcurrencyGuard 在 Drop 时无法回到管理器释放槽位（退化为 no-op）。
    pub fn init_weak_self(self: &std::sync::Arc<Self>) {
        *self.self_weak.lock() = std::sync::Arc::downgrade(self);
    }

    /// 构建一个绑定到指定凭据的并发守卫。
    /// 若 self_weak 尚未初始化（如单元测试未走 Arc 流程），返回 no-op 守卫。
    fn make_guard(&self, credential_id: u64) -> ConcurrencyGuard {
        let weak = self.self_weak.lock().clone();
        if weak.strong_count() == 0 {
            ConcurrencyGuard::noop()
        } else {
            ConcurrencyGuard::new(weak, credential_id)
        }
    }

    /// 释放指定凭据的一个在途槽位（仅供 ConcurrencyGuard::drop 调用）。
    /// 持 entries 锁，`saturating_sub` 防下溢；凭据已删除则忽略。
    fn release_active_slot(&self, credential_id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
            entry.active = entry.active.saturating_sub(1);
        }
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    /// 在 entries 锁内按 least-active + 平手随机打散选出一个凭据并占用其槽位。
    ///
    /// 选号策略（方案 §3.3）：
    /// - balanced：主键 `active` 最小 → 平手按 `success_count` 最小 → 再平手在该组内随机。
    /// - priority：先锁定最高优先级（priority 最小）档位 → 档内按 `active` 最小 → 平手随机。
    ///
    /// 第二层兜底：`max_concurrency > 0 && active >= max_concurrency` 的凭据视为满载被跳过。
    ///
    /// **选号与 `active += 1` 在同一把 entries 锁内完成（原子抢槽，方案 §6.4）**，
    /// 消除 TOCTOU 踩踏窗口。返回时锁已释放，再构建 RAII 守卫。
    fn reserve_best(&self, model: Option<&str>) -> ReserveOutcome {
        let is_balanced = self.load_balancing_mode.lock().as_str() == "balanced";

        let mut entries = self.entries.lock();

        // 第一步：未禁用且支持该模型的凭据
        let available_ids: Vec<u64> = entries
            .iter()
            .filter(|e| !e.disabled && Self::credential_supports_model(&e.credentials, model))
            .map(|e| e.id)
            .collect();

        if available_ids.is_empty() {
            return ReserveOutcome::Empty;
        }

        // 第二步：在未满载的候选里选号
        let is_full = |e: &CredentialEntry| -> bool {
            let max = e.credentials.max_concurrency;
            max > 0 && e.active >= max
        };

        let candidates: Vec<&CredentialEntry> = entries
            .iter()
            .filter(|e| available_ids.contains(&e.id) && !is_full(e))
            .collect();

        if candidates.is_empty() {
            // 全部满载：挑一个"最闲"的未禁用凭据作为等待目标（前端 waiting 显示）
            let wait_target = entries
                .iter()
                .filter(|e| available_ids.contains(&e.id))
                .min_by_key(|e| (e.active, e.credentials.priority))
                .map(|e| e.id);
            return match wait_target {
                Some(id) => ReserveOutcome::AllFull(id),
                None => ReserveOutcome::Empty,
            };
        }

        // 按模式确定平手组，组内随机打散
        let chosen_id = if is_balanced {
            // balanced：主键 (active, success_count)
            let min_key = candidates
                .iter()
                .map(|e| (e.active, e.success_count))
                .min()
                .expect("candidates 非空");
            let tied: Vec<u64> = candidates
                .iter()
                .filter(|e| (e.active, e.success_count) == min_key)
                .map(|e| e.id)
                .collect();
            tied[fastrand::usize(0..tied.len())]
        } else {
            // priority：先锁定最高优先级档（priority 最小），仅档内比 active
            let min_priority = candidates
                .iter()
                .map(|e| e.credentials.priority)
                .min()
                .expect("candidates 非空");
            let min_active = candidates
                .iter()
                .filter(|e| e.credentials.priority == min_priority)
                .map(|e| e.active)
                .min()
                .expect("同档非空");
            let tied: Vec<u64> = candidates
                .iter()
                .filter(|e| e.credentials.priority == min_priority && e.active == min_active)
                .map(|e| e.id)
                .collect();
            tied[fastrand::usize(0..tied.len())]
        };

        // 原子占用：同一把锁内 active += 1
        let creds = {
            let entry = entries
                .iter_mut()
                .find(|e| e.id == chosen_id)
                .expect("chosen_id 必然存在");
            entry.active += 1;
            entry.credentials.clone()
        };
        drop(entries);

        ReserveOutcome::Reserved(chosen_id, creds, false, self.make_guard(chosen_id))
    }

    /// 尝试占用指定凭据的槽位（用于会话亲和命中 / priority 模式 current_id 命中）。
    ///
    /// 选号与 `active += 1` 同样在一把 entries 锁内完成。
    fn reserve_one(&self, id: u64, model: Option<&str>) -> ReserveOne {
        let mut entries = self.entries.lock();
        let entry = match entries.iter_mut().find(|e| e.id == id) {
            Some(e) => e,
            None => return ReserveOne::Unavailable,
        };
        if entry.disabled || !Self::credential_supports_model(&entry.credentials, model) {
            return ReserveOne::Unavailable;
        }
        let max = entry.credentials.max_concurrency;
        if max > 0 && entry.active >= max {
            return ReserveOne::Full;
        }
        entry.active += 1;
        let creds = entry.credentials.clone();
        drop(entries);
        ReserveOne::Ok(creds, self.make_guard(id))
    }

    /// 调整指定凭据的等待计数（前端 waiting 显示）。delta 为 +1 / -1。
    fn adjust_waiting(&self, id: u64, increment: bool) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if increment {
                entry.waiting = entry.waiting.saturating_add(1);
            } else {
                entry.waiting = entry.waiting.saturating_sub(1);
            }
        }
    }

    /// 读取指定凭据当前等待数（仅用于粘性过载阈值判断）。
    fn waiting_count(&self, id: u64) -> u32 {
        self.entries
            .lock()
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.waiting)
            .unwrap_or(0)
    }

    fn credential_supports_model(credentials: &KiroCredentials, model: Option<&str>) -> bool {
        let is_opus = model
            .map(|m| m.to_lowercase().contains("opus"))
            .unwrap_or(false);

        !is_opus || credentials.supports_opus()
    }

    fn is_balanced_mode(&self) -> bool {
        self.load_balancing_mode.lock().as_str() == "balanced"
    }

    fn normalize_session_key(session_key: Option<&str>) -> Option<&str> {
        session_key
            .map(str::trim)
            .and_then(|key| (!key.is_empty()).then_some(key))
    }

    fn prune_session_affinity_locked(map: &mut HashMap<String, SessionAffinityEntry>) {
        let now = Instant::now();
        map.retain(|_, entry| now.duration_since(entry.last_used_at) <= SESSION_AFFINITY_TTL);
    }

    fn credential_for_session(
        &self,
        session_key: &str,
        model: Option<&str>,
    ) -> Option<(u64, KiroCredentials)> {
        let credential_id = {
            let mut affinity = self.session_affinity.lock();
            Self::prune_session_affinity_locked(&mut affinity);
            affinity.get(session_key).map(|entry| entry.credential_id)
        }?;

        let hit = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| {
                    e.id == credential_id
                        && !e.disabled
                        && Self::credential_supports_model(&e.credentials, model)
                })
                .map(|e| (e.id, e.credentials.clone()))
        };

        if hit.is_none() {
            self.clear_session_affinity(session_key);
        }

        hit
    }

    fn remember_session_affinity(&self, session_key: &str, credential_id: u64) {
        let mut affinity = self.session_affinity.lock();
        Self::prune_session_affinity_locked(&mut affinity);
        affinity.insert(
            session_key.to_string(),
            SessionAffinityEntry {
                credential_id,
                last_used_at: Instant::now(),
            },
        );
    }

    fn clear_session_affinity(&self, session_key: &str) {
        self.session_affinity.lock().remove(session_key);
    }

    fn clear_session_affinity_for_credential(&self, credential_id: u64) {
        self.session_affinity
            .lock()
            .retain(|_, entry| entry.credential_id != credential_id);
    }

    fn clear_all_session_affinity(&self) {
        self.session_affinity.lock().clear();
    }

    /// 获取 API 调用上下文
    ///
    /// 返回绑定了 id、credentials 和 token 的调用上下文，以及并发槽位守卫。
    /// 确保整个 API 调用过程中使用一致的凭据信息。
    ///
    /// 如果 Token 过期或即将过期，会自动刷新
    /// Token 刷新失败会累计到当前凭据，达到阈值后禁用并切换
    ///
    /// # 返回
    /// `(CallContext, ConcurrencyGuard)` —— **守卫必须由调用方持有到请求生命周期结束**
    /// （流式读到 stream_end / 非流式读完 body），drop 时自动释放在途槽位。
    ///
    /// # 参数
    /// - `model`: 可选的模型名称，用于过滤支持该模型的凭据（如 opus 模型需要付费订阅）
    pub async fn acquire_context(
        &self,
        model: Option<&str>,
    ) -> anyhow::Result<(CallContext, ConcurrencyGuard)> {
        self.acquire_context_for_session(model, None).await
    }

    /// 获取 API 调用上下文，并在 balanced 模式下按会话绑定凭据。
    ///
    /// 同一个 `session_key` 会尽量复用同一凭据，避免同一对话在不同账号之间切换。
    pub async fn acquire_context_for_session(
        &self,
        model: Option<&str>,
        session_key: Option<&str>,
    ) -> anyhow::Result<(CallContext, ConcurrencyGuard)> {
        let session_key = Self::normalize_session_key(session_key);
        let total = self.total_count();
        let max_attempts = (total * MAX_FAILURES_PER_CREDENTIAL as usize).max(1);
        let mut attempt_count = 0;

        loop {
            if attempt_count >= max_attempts {
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }

            // 选号 + 原子占用槽位（含满载等待 / 粘性过载换号 / 自愈），返回 RAII 守卫
            let (id, credentials, affinity_hit, guard) =
                self.reserve_context(model, session_key).await?;

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials).await {
                Ok(mut ctx) => {
                    ctx.session_affinity_hit = affinity_hit;
                    // 该凭据被选中并即将发起请求：总请求次数 +1（含后续可能失败）
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == ctx.id) {
                            entry.request_count += 1;
                        }
                    }
                    self.save_stats_debounced();
                    if self.is_balanced_mode() {
                        if let Some(session_key) = session_key {
                            self.remember_session_affinity(session_key, ctx.id);
                        }
                    }
                    return Ok((ctx, guard));
                }
                Err(e) => {
                    // 显式释放本次预占的槽位（先释放再报告失败，避免误判满载）
                    drop(guard);
                    if let Some(session_key) = session_key {
                        self.clear_session_affinity(session_key);
                    }
                    // refreshToken 永久失效 → 立即禁用，不累计重试
                    let has_available = if e.downcast_ref::<RefreshTokenInvalidError>().is_some() {
                        tracing::warn!("凭据 #{} refreshToken 永久失效: {}", id, e);
                        self.report_refresh_token_invalid(id)
                    } else {
                        tracing::warn!("凭据 #{} Token 刷新失败: {}", id, e);
                        self.report_refresh_failure(id)
                    };
                    attempt_count += 1;
                    if !has_available {
                        anyhow::bail!("所有凭据均已禁用（0/{}）", total);
                    }
                }
            }
        }
    }

    /// 选号并原子占用槽位的完整流程（方案 §3.3 / §3.4 / §3.6）。
    ///
    /// 顺序：
    /// 1. balanced + 命中会话亲和 → 尝试占用绑定号；满载则短等原号，超阈值/超时放弃亲和换号。
    /// 2. 其余情形（balanced 未命中亲和 / priority 模式）→ 一律走 reserve_best：
    ///    least-active 选号（priority 先锁定最高可用档、档内 active + 随机）+ 满载短等待 + 自愈。
    ///    priority 模式不再预先复用 current_id，确保同档摊开、且高档释放后能自动回档。
    ///
    /// **铁律：sleep 期间绝不持 entries 锁**（reserve_* 内部取锁、返回后才 sleep）。
    async fn reserve_context(
        &self,
        model: Option<&str>,
        session_key: Option<&str>,
    ) -> anyhow::Result<(u64, KiroCredentials, bool, ConcurrencyGuard)> {
        let is_balanced = self.load_balancing_mode.lock().as_str() == "balanced";

        if is_balanced {
            // 1. 会话亲和命中（仅 balanced）
            if let Some(key) = session_key {
                if let Some((bound_id, _)) = self.credential_for_session(key, model) {
                    match self.reserve_one(bound_id, model) {
                        ReserveOne::Ok(creds, guard) => {
                            return Ok((bound_id, creds, true, guard));
                        }
                        ReserveOne::Full => {
                            // 粘性过载：优先短等原号保缓存；超阈值/超时放弃亲和换号
                            if let Some(res) = self.wait_for_sticky(bound_id, model).await {
                                return Ok(res);
                            }
                            self.clear_session_affinity(key);
                        }
                        ReserveOne::Unavailable => {
                            self.clear_session_affinity(key);
                        }
                    }
                }
            }
        }
        // priority 模式：不预先复用 current_id，直接走 reserve_best。
        // reserve_best 已实现文档 §3.3 的 priority 语义——先锁定最高「可用（未满载）」
        // 优先级档，再在同档内按 active 最小 + 平手随机打散。
        //   - 同档多号：每次都在档内按 active 摊开，不会一直钉住某一个号（修复同档不打散）。
        //   - 高档满载落到低档后：下次仍重新锁定「最高可用档」，高档一释放即自动回到高档
        //     （current_id 不再参与路由决策，仅用于 UI「当前」标记，故落档不会粘住）。

        // 2. least-active 选号 + 满载短等待 + 自愈
        self.reserve_best_with_wait(model).await
    }

    /// least-active 选号，满载时短等待重试，全灭时自愈。
    async fn reserve_best_with_wait(
        &self,
        model: Option<&str>,
    ) -> anyhow::Result<(u64, KiroCredentials, bool, ConcurrencyGuard)> {
        let total = self.total_count();
        let deadline = Instant::now() + CONCURRENCY_WAIT_BUDGET;
        let mut wait_target: Option<u64> = None;

        loop {
            match self.reserve_best(model) {
                ReserveOutcome::Reserved(id, creds, hit, guard) => {
                    if let Some(w) = wait_target.take() {
                        self.adjust_waiting(w, false);
                    }
                    // priority 模式：更新 current_id 指向新选中的凭据
                    if !self.is_balanced_mode() {
                        *self.current_id.lock() = id;
                    }
                    return Ok((id, creds, hit, guard));
                }
                ReserveOutcome::AllFull(target) => {
                    // 维护 waiting 显示：始终对当前等待目标计数
                    if wait_target != Some(target) {
                        if let Some(w) = wait_target.take() {
                            self.adjust_waiting(w, false);
                        }
                        self.adjust_waiting(target, true);
                        wait_target = Some(target);
                    }
                    if Instant::now() >= deadline {
                        if let Some(w) = wait_target.take() {
                            self.adjust_waiting(w, false);
                        }
                        anyhow::bail!(
                            "CONCURRENCY_BUSY: 所有可用凭据并发已满（{}），请稍后重试",
                            total
                        );
                    }
                    tokio::time::sleep(CONCURRENCY_WAIT_POLL).await;
                }
                ReserveOutcome::Empty => {
                    if let Some(w) = wait_target.take() {
                        self.adjust_waiting(w, false);
                    }
                    // 自愈：仅当存在"自动失败禁用"的凭据时重置并重试一次
                    if self.try_autoheal() {
                        continue;
                    }
                    let available = self.available_count();
                    anyhow::bail!("所有凭据均已禁用（{}/{}）", available, total);
                }
            }
        }
    }

    /// 粘性会话原号满载时的短等待（方案 §3.6.4 第二阶段）。
    ///
    /// 等到原号释放 → 返回原号（保缓存）；等待数已达阈值或超时 → 返回 None（放弃亲和换号）。
    async fn wait_for_sticky(
        &self,
        bound_id: u64,
        model: Option<&str>,
    ) -> Option<(u64, KiroCredentials, bool, ConcurrencyGuard)> {
        // 入口闸：等待者已达阈值则直接放弃，避免无限堆积
        if self.waiting_count(bound_id) >= STICKY_MAX_WAITING {
            return None;
        }
        self.adjust_waiting(bound_id, true);
        let deadline = Instant::now() + CONCURRENCY_WAIT_BUDGET;

        loop {
            match self.reserve_one(bound_id, model) {
                ReserveOne::Ok(creds, guard) => {
                    self.adjust_waiting(bound_id, false);
                    return Some((bound_id, creds, true, guard));
                }
                ReserveOne::Unavailable => {
                    self.adjust_waiting(bound_id, false);
                    return None;
                }
                ReserveOne::Full => {
                    if Instant::now() >= deadline {
                        self.adjust_waiting(bound_id, false);
                        return None;
                    }
                    tokio::time::sleep(CONCURRENCY_WAIT_POLL).await;
                }
            }
        }
    }

    /// 自愈：若存在因连续失败被自动禁用的凭据，重置其失败计数并重新启用。
    /// 返回是否实际重新启用了凭据（用于避免自愈后仍 Empty 时的死循环）。
    fn try_autoheal(&self) -> bool {
        let mut entries = self.entries.lock();
        let has_auto_disabled = entries
            .iter()
            .any(|e| e.disabled && e.disabled_reason == Some(DisabledReason::TooManyFailures));
        if !has_auto_disabled {
            return false;
        }
        tracing::warn!("所有凭据均已被自动禁用，执行自愈：重置失败计数并重新启用（等价于重启）");
        for e in entries.iter_mut() {
            if e.disabled_reason == Some(DisabledReason::TooManyFailures) {
                e.disabled = false;
                e.disabled_reason = None;
                e.failure_count = 0;
            }
        }
        true
    }

    /// 选择优先级最高的未禁用凭据作为当前凭据（内部方法）
    ///
    /// 纯粹按优先级选择，不排除当前凭据，用于优先级变更后立即生效
    fn select_highest_priority(&self) {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（不排除当前凭据）
        if let Some(best) = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
        {
            if best.id != *current_id {
                tracing::info!(
                    "优先级变更后切换凭据: #{} -> #{}（优先级 {}）",
                    *current_id,
                    best.id,
                    best.credentials.priority
                );
                *current_id = best.id;
            }
        }
    }

    /// 尝试使用指定凭据获取有效 Token
    ///
    /// 使用双重检查锁定模式，确保同一时间只有一个刷新操作
    ///
    /// # Arguments
    /// * `id` - 凭据 ID，用于更新正确的条目
    /// * `credentials` - 凭据信息
    async fn try_ensure_token(
        &self,
        id: u64,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<CallContext> {
        // API Key 凭据直接使用 kiro_api_key 作为 Bearer Token，无需刷新
        if credentials.is_api_key_credential() {
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id,
                credentials: credentials.clone(),
                token,
                session_affinity_hit: false,
            });
        }

        // 第一次检查（无锁）：快速判断是否需要刷新
        let needs_refresh = is_token_expired(credentials) || is_token_expiring_soon(credentials);

        let creds = if needs_refresh {
            // 获取刷新锁，确保同一时间只有一个刷新操作
            let _guard = self.refresh_lock.lock().await;

            // 第二次检查：获取锁后重新读取凭据，因为其他请求可能已经完成刷新
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                // 确实需要刷新
                let effective_proxy = current_creds.effective_proxy(self.proxy.as_ref());
                let new_creds =
                    refresh_token(&current_creds, &self.config, effective_proxy.as_ref()).await?;

                if is_token_expired(&new_creds) {
                    anyhow::bail!("刷新后的 Token 仍然无效或已过期");
                }

                // 更新凭据
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                    }
                }

                // 回写凭据到文件，失败只记录警告
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                }

                new_creds
            } else {
                // 其他请求已经完成刷新，直接使用新凭据
                tracing::debug!("Token 已被其他请求刷新，跳过刷新");
                current_creds
            }
        } else {
            credentials.clone()
        };

        let token = creds
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?;

        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.refresh_failure_count = 0;
            }
        }

        Ok(CallContext {
            id,
            credentials: creds,
            token,
            session_affinity_hit: false,
        })
    }

    /// 将凭据列表回写到源文件
    ///
    /// 仅在以下条件满足时回写：
    /// - credentials_path 已设置
    ///
    /// 写回格式：
    /// - 源文件是单凭据格式且当前只有 1 个凭据：写回单对象
    /// - 其他情况：写回数组
    ///
    /// # Returns
    /// - `Ok(true)` - 成功写入文件
    /// - `Ok(false)` - 跳过写入（无路径配置）
    /// - `Err(_)` - 写入失败
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        use anyhow::Context;

        let path = match &self.credentials_path {
            Some(p) => p,
            None => return Ok(false),
        };

        // 收集所有凭据
        let credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    let mut cred = e.credentials.clone();
                    cred.canonicalize_auth_method();
                    // 同步 disabled 状态到凭据对象
                    cred.disabled = e.disabled;
                    cred
                })
                .collect()
        };

        // 序列化为 pretty JSON
        let json = if !self.is_multiple_format && credentials.len() == 1 {
            serde_json::to_string_pretty(&credentials[0]).context("序列化凭据失败")?
        } else {
            serde_json::to_string_pretty(&credentials).context("序列化凭据失败")?
        };

        // 写入文件（在 Tokio runtime 内使用 block_in_place 避免阻塞 worker）
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| std::fs::write(path, &json))
                .with_context(|| format!("回写凭据文件失败: {:?}", path))?;
        } else {
            std::fs::write(path, &json).with_context(|| format!("回写凭据文件失败: {:?}", path))?;
        }

        tracing::debug!("已回写凭据到文件: {:?}", path);
        Ok(true)
    }

    /// 获取缓存目录（凭据文件所在目录）
    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.credentials_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 统计数据文件路径
    fn stats_path(&self) -> Option<PathBuf> {
        self.cache_dir().map(|d| d.join("kiro_stats.json"))
    }

    /// 从磁盘加载统计数据并应用到当前条目
    fn load_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return, // 首次运行时文件不存在
        };

        let stats: HashMap<String, StatsEntry> = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("解析统计缓存失败，将忽略: {}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.request_count = s.request_count;
                entry.last_used_at = s.last_used_at.clone();
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        self.stats_dirty.store(false, Ordering::Relaxed);
        tracing::info!("已从缓存加载 {} 条统计数据", stats.len());
    }

    /// 将当前统计数据持久化到磁盘
    fn save_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let stats: HashMap<String, StatsEntry> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    (
                        e.id.to_string(),
                        StatsEntry {
                            success_count: e.success_count,
                            request_count: e.request_count,
                            last_used_at: e.last_used_at.clone(),
                        },
                    )
                })
                .collect()
        };

        match serde_json::to_string_pretty(&stats) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("保存统计缓存失败: {}", e);
                } else {
                    *self.last_stats_save_at.lock() = Some(Instant::now());
                    self.stats_dirty.store(false, Ordering::Relaxed);
                }
            }
            Err(e) => tracing::warn!("序列化统计数据失败: {}", e),
        }
    }

    /// 标记统计数据已更新，并按 debounce 策略决定是否立即落盘
    fn save_stats_debounced(&self) {
        self.stats_dirty.store(true, Ordering::Relaxed);

        let should_flush = {
            let last = *self.last_stats_save_at.lock();
            match last {
                Some(last_saved_at) => last_saved_at.elapsed() >= STATS_SAVE_DEBOUNCE,
                None => true,
            }
        };

        if should_flush {
            self.save_stats();
        }
    }

    /// 获取指定凭据的总请求次数（含失败）。未找到返回 0。
    pub fn get_request_count(&self, id: u64) -> u64 {
        self.entries
            .lock()
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.request_count)
            .unwrap_or(0)
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_success(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.success_count += 1;
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        self.save_stats_debounced();
    }

    /// 报告指定凭据 API 调用失败
    ///
    /// 增加失败计数，达到阈值时禁用凭据并切换到优先级最高的可用凭据
    /// 返回是否还有可用凭据可以重试
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_failure(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.failure_count += 1;
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            let failure_count = entry.failure_count;

            tracing::warn!(
                "凭据 #{} API 调用失败（{}/{}）",
                id,
                failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::TooManyFailures);
                tracing::error!("凭据 #{} 已连续失败 {} 次，已被禁用", id, failure_count);

                // 切换到优先级最高的可用凭据
                if let Some(next) = entries
                    .iter()
                    .filter(|e| !e.disabled)
                    .min_by_key(|e| e.credentials.priority)
                {
                    *current_id = next.id;
                    tracing::info!(
                        "已切换到凭据 #{}（优先级 {}）",
                        next.id,
                        next.credentials.priority
                    );
                } else {
                    tracing::error!("所有凭据均已禁用！");
                }
            }

            entries.iter().any(|e| !e.disabled)
        };
        self.clear_session_affinity_for_credential(id);
        self.save_stats_debounced();
        result
    }

    /// 报告指定凭据额度已用尽
    ///
    /// 用于处理 402 Payment Required 且 reason 为 `MONTHLY_REQUEST_COUNT` 的场景：
    /// - 立即禁用该凭据（不等待连续失败阈值）
    /// - 切换到下一个可用凭据继续重试
    /// - 返回是否还有可用凭据
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::QuotaExceeded);
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            // 设为阈值，便于在管理面板中直观看到该凭据已不可用
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!("凭据 #{} 额度已用尽（MONTHLY_REQUEST_COUNT），已被禁用", id);

            // 切换到优先级最高的可用凭据
            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.clear_session_affinity_for_credential(id);
        self.save_stats_debounced();
        result
    }

    /// 报告指定凭据刷新 Token 失败。
    ///
    /// 连续刷新失败达到阈值后禁用凭据并切换，阈值内保持当前凭据不切换，
    /// 与 API 401/403 的累计失败策略保持一致。
    pub fn report_refresh_failure(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.last_used_at = Some(Utc::now().to_rfc3339());
            entry.refresh_failure_count += 1;
            let refresh_failure_count = entry.refresh_failure_count;

            tracing::warn!(
                "凭据 #{} Token 刷新失败（{}/{}）",
                id,
                refresh_failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if refresh_failure_count < MAX_FAILURES_PER_CREDENTIAL {
                entries.iter().any(|e| !e.disabled)
            } else {
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::TooManyRefreshFailures);

                tracing::error!(
                    "凭据 #{} Token 已连续刷新失败 {} 次，已被禁用",
                    id,
                    refresh_failure_count
                );

                if let Some(next) = entries
                    .iter()
                    .filter(|e| !e.disabled)
                    .min_by_key(|e| e.credentials.priority)
                {
                    *current_id = next.id;
                    tracing::info!(
                        "已切换到凭据 #{}（优先级 {}）",
                        next.id,
                        next.credentials.priority
                    );
                    true
                } else {
                    tracing::error!("所有凭据均已禁用！");
                    false
                }
            }
        };
        self.clear_session_affinity_for_credential(id);
        self.save_stats_debounced();
        result
    }

    /// 报告指定凭据的 refreshToken 永久失效（invalid_grant）。
    ///
    /// 立即禁用凭据，不累计、不重试。
    /// 返回是否还有可用凭据。
    pub fn report_refresh_token_invalid(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.last_used_at = Some(Utc::now().to_rfc3339());
            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::InvalidRefreshToken);

            tracing::error!(
                "凭据 #{} refreshToken 已失效 (invalid_grant)，已立即禁用",
                id
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.clear_session_affinity_for_credential(id);
        self.save_stats_debounced();
        result
    }

    /// 切换到优先级最高的可用凭据
    ///
    /// 返回是否成功切换
    pub fn switch_to_next(&self) -> bool {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（排除当前凭据）
        if let Some(next) = entries
            .iter()
            .filter(|e| !e.disabled && e.id != *current_id)
            .min_by_key(|e| e.credentials.priority)
        {
            *current_id = next.id;
            tracing::info!(
                "已切换到凭据 #{}（优先级 {}）",
                next.id,
                next.credentials.priority
            );
            true
        } else {
            // 没有其他可用凭据，检查当前凭据是否可用
            entries.iter().any(|e| e.id == *current_id && !e.disabled)
        }
    }

    // ========================================================================
    // Admin API 方法
    // ========================================================================

    /// 获取管理器状态快照（用于 Admin API）
    pub fn snapshot(&self) -> ManagerSnapshot {
        let entries = self.entries.lock();
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| CredentialEntrySnapshot {
                    id: e.id,
                    priority: e.credentials.priority,
                    disabled: e.disabled,
                    failure_count: e.failure_count,
                    auth_method: if e.credentials.is_api_key_credential() {
                        Some("api_key".to_string())
                    } else {
                        e.credentials.auth_method.as_deref().map(|m| {
                            if m.eq_ignore_ascii_case("builder-id") || m.eq_ignore_ascii_case("iam")
                            {
                                "idc".to_string()
                            } else {
                                m.to_string()
                            }
                        })
                    },
                    provider: e.credentials.provider.clone(),
                    has_profile_arn: e
                        .credentials
                        .profile_arn
                        .as_deref()
                        .is_some_and(|arn| !arn.trim().is_empty()),
                    expires_at: if e.credentials.is_api_key_credential() {
                        None // API Key 凭据本地不维护过期时间（服务端策略未知）
                    } else {
                        e.credentials.expires_at.clone()
                    },
                    refresh_token_hash: if e.credentials.is_api_key_credential() {
                        None
                    } else {
                        e.credentials.refresh_token.as_deref().map(sha256_hex)
                    },
                    api_key_hash: if e.credentials.is_api_key_credential() {
                        e.credentials.kiro_api_key.as_deref().map(sha256_hex)
                    } else {
                        None
                    },
                    masked_api_key: if e.credentials.is_api_key_credential() {
                        e.credentials.kiro_api_key.as_deref().map(mask_api_key)
                    } else {
                        None
                    },
                    email: e.credentials.email.clone(),
                    success_count: e.success_count,
                    request_count: e.request_count,
                    last_used_at: e.last_used_at.clone(),
                    has_proxy: e.credentials.proxy_url.is_some(),
                    proxy_url: e.credentials.proxy_url.clone(),
                    refresh_failure_count: e.refresh_failure_count,
                    disabled_reason: e.disabled_reason.map(|r| {
                        match r {
                            DisabledReason::Manual => "Manual",
                            DisabledReason::TooManyFailures => "TooManyFailures",
                            DisabledReason::TooManyRefreshFailures => "TooManyRefreshFailures",
                            DisabledReason::QuotaExceeded => "QuotaExceeded",
                            DisabledReason::InvalidRefreshToken => "InvalidRefreshToken",
                            DisabledReason::InvalidConfig => "InvalidConfig",
                        }
                        .to_string()
                    }),
                    endpoint: e.credentials.endpoint.clone(),
                    max_concurrency: e.credentials.max_concurrency,
                    active_concurrency: e.active,
                    waiting_concurrency: e.waiting,
                })
                .collect(),
            current_id,
            total: entries.len(),
            available,
        }
    }

    /// 设置凭据禁用状态（Admin API）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.disabled = disabled;
            if !disabled {
                // 启用时重置失败计数
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.disabled_reason = None;
            } else {
                entry.disabled_reason = Some(DisabledReason::Manual);
            }
        }
        if disabled {
            self.clear_session_affinity_for_credential(id);
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    ///
    /// 修改优先级后会立即按新优先级重新选择当前凭据。
    /// 即使持久化失败，内存中的优先级和当前凭据选择也会生效。
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.priority = priority;
        }
        // 立即按新优先级重新选择当前凭据（无论持久化是否成功）
        self.select_highest_priority();
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据并发硬上限（Admin API）。0 = 不限制。
    ///
    /// 仅修改运行时与持久化配置，不影响当前在途计数。
    pub fn set_max_concurrency(&self, id: u64, max_concurrency: u32) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.max_concurrency = max_concurrency;
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 批量设置凭据并发硬上限（Admin API）。0 = 不限制。
    ///
    /// 对指定的所有 id 设置同一个上限；忽略不存在的 id。
    /// 返回实际生效的凭据数量。仅在至少命中一个凭据时持久化一次。
    pub fn set_max_concurrency_batch(
        &self,
        ids: &[u64],
        max_concurrency: u32,
    ) -> anyhow::Result<usize> {
        let mut applied = 0usize;
        {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                if ids.contains(&entry.id) {
                    entry.credentials.max_concurrency = max_concurrency;
                    applied += 1;
                }
            }
        }
        if applied > 0 {
            self.persist_credentials()?;
        }
        Ok(applied)
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disabled_reason == Some(DisabledReason::InvalidConfig) {
                anyhow::bail!("凭据 #{} 因配置无效被禁用，请修正配置后重启服务", id);
            }
            entry.failure_count = 0;
            entry.refresh_failure_count = 0;
            entry.disabled = false;
            entry.disabled_reason = None;
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // API Key 凭据直接使用 kiro_api_key，无需刷新
        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?
        } else {
            // 检查是否需要刷新 token
            let needs_refresh =
                is_token_expired(&credentials) || is_token_expiring_soon(&credentials);

            if needs_refresh {
                let _guard = self.refresh_lock.lock().await;
                let current_creds = {
                    let entries = self.entries.lock();
                    entries
                        .iter()
                        .find(|e| e.id == id)
                        .map(|e| e.credentials.clone())
                        .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
                };

                if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                    let effective_proxy = current_creds.effective_proxy(self.proxy.as_ref());
                    let new_creds =
                        refresh_token(&current_creds, &self.config, effective_proxy.as_ref())
                            .await?;
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.credentials = new_creds.clone();
                        }
                    }
                    // 持久化失败只记录警告，不影响本次请求
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                    }
                    new_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
                } else {
                    current_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
                }
            } else {
                credentials
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
            }
        };

        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let usage_limits =
            get_usage_limits(&credentials, &self.config, &token, effective_proxy.as_ref()).await?;

        // 更新订阅等级到凭据（仅在发生变化时持久化）
        if let Some(subscription_title) = usage_limits.subscription_title() {
            let changed = {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    let old_title = entry.credentials.subscription_title.clone();
                    if old_title.as_deref() != Some(subscription_title) {
                        entry.credentials.subscription_title = Some(subscription_title.to_string());
                        tracing::info!(
                            "凭据 #{} 订阅等级已更新: {:?} -> {}",
                            id,
                            old_title,
                            subscription_title
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            if changed {
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("订阅等级更新后持久化失败（不影响本次请求）: {}", e);
                }
            }
        }

        Ok(usage_limits)
    }

    /// 设置指定凭据的超额开关（Admin API）。
    ///
    /// `enabled=true` → ENABLED，`false` → DISABLED。
    /// 先通过 `get_usage_limits_for` 确保 token 新鲜（它会按需刷新并持久化），
    /// 再用当前有效 token 调用上游 `setUserPreference`。
    /// 成功后返回该凭据最新的额度信息（含改写后的超额状态）。
    pub async fn set_overage_for(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<UsageLimitsResponse> {
        let overage_status = if enabled { "ENABLED" } else { "DISABLED" };

        // 先拉一次额度，顺带保证 token 已刷新且有效。
        let _ = self.get_usage_limits_for(id).await?;

        // 取当前（已刷新的）凭据与有效 token。
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?
        } else {
            credentials
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
        };

        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        set_user_preference(
            &credentials,
            &self.config,
            &token,
            effective_proxy.as_ref(),
            overage_status,
        )
        .await?;

        tracing::info!("凭据 #{} 超额开关已设置为 {}", id, overage_status);

        // 重新拉取一次，返回上游确认后的最新状态。
        let usage_limits =
            get_usage_limits(&credentials, &self.config, &token, effective_proxy.as_ref()).await?;
        Ok(usage_limits)
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（API Key: kiroApiKey 不为空; OAuth: refreshToken 不为空）
    /// 2. 基于 kiroApiKey 或 refreshToken 的 SHA-256 哈希检测重复
    /// 3. OAuth: 尝试刷新 Token 验证凭据有效性; API Key: 跳过
    /// 4. 分配新 ID（当前最大 ID + 1）
    /// 5. 添加到 entries 列表
    /// 6. 持久化到配置文件
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, mut new_cred: KiroCredentials) -> anyhow::Result<u64> {
        new_cred.canonicalize_auth_method();
        if new_cred.machine_id.is_none() {
            new_cred.machine_id = Some(machine_id::generate_from_credentials(
                &new_cred,
                &self.config,
            ));
        }

        // 1. 基本验证
        if new_cred.is_api_key_credential() {
            let api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            if api_key.is_empty() {
                anyhow::bail!("kiroApiKey 为空");
            }
        } else {
            validate_refresh_token(&new_cred)?;
        }

        // 2. 基于哈希检测重复
        if new_cred.is_api_key_credential() {
            let new_api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;
            let new_api_key_hash = sha256_hex(new_api_key);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .kiro_api_key
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_api_key_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（kiroApiKey 重复）");
            }
        } else {
            let new_refresh_token = new_cred
                .refresh_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;
            let new_refresh_token_hash = sha256_hex(new_refresh_token);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .refresh_token
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_refresh_token_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（refreshToken 重复）");
            }
        }

        // 3. 验证凭据有效性（API Key 无需网络刷新）
        let mut validated_cred = if new_cred.is_api_key_credential() {
            new_cred.clone()
        } else {
            let effective_proxy = new_cred.effective_proxy(self.proxy.as_ref());
            refresh_token(&new_cred, &self.config, effective_proxy.as_ref()).await?
        };

        // 4. 分配新 ID
        let new_id = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
        };

        // 5. 设置 ID 并保留用户输入的元数据
        validated_cred.id = Some(new_id);
        validated_cred.priority = new_cred.priority;
        validated_cred.auth_method = new_cred.auth_method.map(|m| {
            if m.eq_ignore_ascii_case("builder-id") || m.eq_ignore_ascii_case("iam") {
                "idc".to_string()
            } else {
                m
            }
        });
        validated_cred.provider = new_cred.provider;
        validated_cred.client_id = new_cred.client_id;
        validated_cred.client_secret = new_cred.client_secret;
        validated_cred.region = new_cred.region;
        validated_cred.auth_region = new_cred.auth_region;
        validated_cred.api_region = new_cred.api_region;
        validated_cred.machine_id = new_cred.machine_id;
        validated_cred.email = new_cred.email;
        validated_cred.proxy_url = new_cred.proxy_url;
        validated_cred.proxy_username = new_cred.proxy_username;
        validated_cred.proxy_password = new_cred.proxy_password;
        validated_cred.kiro_api_key = new_cred.kiro_api_key;

        {
            let mut entries = self.entries.lock();
            entries.push(CredentialEntry {
                id: new_id,
                credentials: validated_cred,
                failure_count: 0,
                refresh_failure_count: 0,
                disabled: false,
                disabled_reason: None,
                success_count: 0,
                request_count: 0,
                last_used_at: None,
                active: 0,
                waiting: 0,
            });
        }

        // 6. 持久化
        self.persist_credentials()?;

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 删除凭据（Admin API）
    ///
    /// # 前置条件
    /// - 凭据必须已禁用（disabled = true）
    ///
    /// # 行为
    /// 1. 验证凭据存在
    /// 2. 验证凭据已禁用
    /// 3. 从 entries 移除
    /// 4. 如果删除的是当前凭据，切换到优先级最高的可用凭据
    /// 5. 如果删除后没有凭据，将 current_id 重置为 0
    /// 6. 持久化到文件
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        let was_current = {
            let mut entries = self.entries.lock();

            // 查找凭据
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            // 检查是否已禁用
            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }

            // 记录是否是当前凭据
            let current_id = *self.current_id.lock();
            let was_current = current_id == id;

            // 删除凭据
            entries.retain(|e| e.id != id);

            was_current
        };

        // 如果删除的是当前凭据，切换到优先级最高的可用凭据
        if was_current {
            self.select_highest_priority();
        }
        self.clear_session_affinity_for_credential(id);

        // 如果删除后没有任何凭据，将 current_id 重置为 0（与初始化行为保持一致）
        {
            let entries = self.entries.lock();
            if entries.is_empty() {
                let mut current_id = self.current_id.lock();
                *current_id = 0;
                tracing::info!("所有凭据已删除，current_id 已重置为 0");
            }
        }

        // 持久化更改
        self.persist_credentials()?;

        // 立即回写统计数据，清除已删除凭据的残留条目
        self.save_stats();

        tracing::info!("已删除凭据 #{}", id);
        Ok(())
    }

    /// 强制刷新指定凭据的 Token（Admin API）
    ///
    /// 无条件调用上游 API 重新获取 access token，不检查是否过期。
    /// 适用于排查问题、Token 异常但未过期、主动更新凭据状态等场景。
    pub async fn force_refresh_token_for(&self, id: u64) -> anyhow::Result<()> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 获取刷新锁防止并发刷新
        let _guard = self.refresh_lock.lock().await;

        // 无条件调用 refresh_token
        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let new_creds = refresh_token(&credentials, &self.config, effective_proxy.as_ref()).await?;

        // 更新 entries 中对应凭据
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.credentials = new_creds;
                entry.refresh_failure_count = 0;
            }
        }

        // 持久化
        if let Err(e) = self.persist_credentials() {
            tracing::warn!("强制刷新 Token 后持久化失败: {}", e);
        }

        tracing::info!("凭据 #{} Token 已强制刷新", id);
        Ok(())
    }

    /// 获取负载均衡模式（Admin API）
    pub fn get_load_balancing_mode(&self) -> String {
        self.load_balancing_mode.lock().clone()
    }

    fn persist_load_balancing_mode(&self, mode: &str) -> anyhow::Result<()> {
        use anyhow::Context;

        let config_path = match self.config.config_path() {
            Some(path) => path.to_path_buf(),
            None => {
                tracing::warn!("配置文件路径未知，负载均衡模式仅在当前进程生效: {}", mode);
                return Ok(());
            }
        };

        let mut config = Config::load(&config_path)
            .with_context(|| format!("重新加载配置失败: {}", config_path.display()))?;
        config.load_balancing_mode = mode.to_string();
        config
            .save()
            .with_context(|| format!("持久化负载均衡模式失败: {}", config_path.display()))?;

        Ok(())
    }

    /// 设置负载均衡模式（Admin API）
    pub fn set_load_balancing_mode(&self, mode: String) -> anyhow::Result<()> {
        // 验证模式值
        if mode != "priority" && mode != "balanced" {
            anyhow::bail!("无效的负载均衡模式: {}", mode);
        }

        let previous_mode = self.get_load_balancing_mode();
        if previous_mode == mode {
            return Ok(());
        }

        *self.load_balancing_mode.lock() = mode.clone();
        self.clear_all_session_affinity();

        if let Err(err) = self.persist_load_balancing_mode(&mode) {
            *self.load_balancing_mode.lock() = previous_mode;
            return Err(err);
        }

        tracing::info!("负载均衡模式已设置为: {}", mode);
        Ok(())
    }
}

impl Drop for MultiTokenManager {
    fn drop(&mut self) {
        if self.stats_dirty.load(Ordering::Relaxed) {
            self.save_stats();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_token_expired_with_expired_token() {
        let mut credentials = KiroCredentials::default();
        credentials.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_with_valid_token() {
        let mut credentials = KiroCredentials::default();
        let future = Utc::now() + Duration::hours(1);
        credentials.expires_at = Some(future.to_rfc3339());
        assert!(!is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_within_5_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(3);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_no_expires_at() {
        let credentials = KiroCredentials::default();
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_within_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(8);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_beyond_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(15);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(!is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_validate_refresh_token_missing() {
        let credentials = KiroCredentials::default();
        let result = validate_refresh_token(&credentials);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_refresh_token_valid() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        let result = validate_refresh_token(&credentials);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[tokio::test]
    async fn test_refresh_token_rejects_api_key_credential() {
        let config = Config::default();
        let mut credentials = KiroCredentials::default();
        credentials.kiro_api_key = Some("ksk_test_key_123".to_string());
        credentials.auth_method = Some("api_key".to_string());

        let result = refresh_token(&credentials, &config, None).await;

        assert!(result.is_err(), "API Key 凭据应被 refresh_token 拒绝");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("API Key 凭据不支持刷新"),
            "期望错误消息包含 'API Key 凭据不支持刷新'，实际: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_refresh_token() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.refresh_token = Some("a".repeat(150));

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("凭据已存在"));
    }

    #[tokio::test]
    async fn test_add_credential_api_key_success() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut api_key_cred = KiroCredentials::default();
        api_key_cred.kiro_api_key = Some("ksk_test_key_123".to_string());
        api_key_cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(api_key_cred).await;
        assert!(result.is_ok());
        let id = result.unwrap();
        assert!(id > 0);
        assert_eq!(manager.total_count(), 1);
        assert_eq!(manager.available_count(), 1);
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_api_key() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.kiro_api_key = Some("ksk_existing_key".to_string());
        existing.auth_method = Some("api_key".to_string());

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.kiro_api_key = Some("ksk_existing_key".to_string());
        duplicate.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("kiroApiKey 重复")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_empty_rejected() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut cred = KiroCredentials::default();
        cred.kiro_api_key = Some(String::new());
        cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(cred).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("kiroApiKey 为空")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_missing_key_rejected() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        // kiro_api_key is None

        let result = manager.add_credential(cred).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("缺少 kiroApiKey")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_and_oauth_coexist() {
        let config = Config::default();

        let mut oauth_cred = KiroCredentials::default();
        oauth_cred.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![oauth_cred], None, None, false).unwrap();

        let mut api_key_cred = KiroCredentials::default();
        api_key_cred.kiro_api_key = Some("ksk_new_key".to_string());
        api_key_cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(api_key_cred).await;
        assert!(result.is_ok());
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    // MultiTokenManager 测试

    #[test]
    fn test_multi_token_manager_new() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.priority = 0;
        let mut cred2 = KiroCredentials::default();
        cred2.priority = 1;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    #[test]
    fn test_multi_token_manager_empty_credentials() {
        let config = Config::default();
        let result = MultiTokenManager::new(config, vec![], None, None, false);
        // 支持 0 个凭据启动（可通过管理面板添加）
        assert!(result.is_ok());
        let manager = result.unwrap();
        assert_eq!(manager.total_count(), 0);
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_duplicate_ids() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.id = Some(1);
        let mut cred2 = KiroCredentials::default();
        cred2.id = Some(1); // 重复 ID

        let result = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false);
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("重复的凭据 ID"),
            "错误消息应包含 '重复的凭据 ID'，实际: {}",
            err_msg
        );
    }

    #[test]
    fn test_multi_token_manager_api_key_missing_kiro_api_key_auto_disabled() {
        let config = Config::default();

        // auth_method=api_key 但缺少 kiro_api_key → 应被自动禁用
        let mut bad_cred = KiroCredentials::default();
        bad_cred.auth_method = Some("api_key".to_string());
        // kiro_api_key 保持 None

        let mut good_cred = KiroCredentials::default();
        good_cred.refresh_token = Some("valid_token".to_string());

        let manager =
            MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 1); // bad_cred 被禁用，只剩 1 个可用
    }

    #[test]
    fn test_multi_token_manager_api_key_with_kiro_api_key_not_disabled() {
        let config = Config::default();

        // auth_method=api_key 且有 kiro_api_key → 不应被禁用
        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        cred.kiro_api_key = Some("ksk_test123".to_string());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_multi_token_manager_report_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        // 前两次失败不会禁用（使用 ID 1）
        assert!(manager.report_failure(1));
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);

        // 第三次失败会禁用第一个凭据
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 1);

        // 继续失败第二个凭据（使用 ID 2）
        assert!(manager.report_failure(2));
        assert!(manager.report_failure(2));
        assert!(!manager.report_failure(2)); // 所有凭据都禁用了
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_report_success() {
        let config = Config::default();
        let cred = KiroCredentials::default();

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 失败两次（使用 ID 1）
        manager.report_failure(1);
        manager.report_failure(1);

        // 成功后重置计数（使用 ID 1）
        manager.report_success(1);

        // 再失败两次不会禁用
        manager.report_failure(1);
        manager.report_failure(1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_multi_token_manager_switch_to_next() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.refresh_token = Some("token1".to_string());
        let mut cred2 = KiroCredentials::default();
        cred2.refresh_token = Some("token2".to_string());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let initial_id = manager.snapshot().current_id;

        // 切换到下一个
        assert!(manager.switch_to_next());
        assert_ne!(manager.snapshot().current_id, initial_id);
    }

    #[test]
    fn test_set_load_balancing_mode_persists_to_config_file() {
        let config_path =
            std::env::temp_dir().join(format!("kiro-load-balancing-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&config_path, r#"{"loadBalancingMode":"priority"}"#).unwrap();

        let config = Config::load(&config_path).unwrap();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .set_load_balancing_mode("balanced".to_string())
            .unwrap();

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(persisted.load_balancing_mode, "balanced");
        assert_eq!(manager.get_load_balancing_mode(), "balanced");

        std::fs::remove_file(&config_path).unwrap();
    }

    #[test]
    fn test_single_credentials_format_persists_as_object() {
        let credentials_path = std::env::temp_dir().join(format!(
            "kiro-single-credentials-{}.json",
            uuid::Uuid::new_v4()
        ));

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let _manager = MultiTokenManager::new(
            Config::default(),
            vec![cred],
            None,
            Some(credentials_path.clone()),
            false,
        )
        .unwrap();

        let persisted = std::fs::read_to_string(&credentials_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&persisted).unwrap();
        assert!(json.is_object());
        assert!(json.get("machineId").and_then(|v| v.as_str()).is_some());

        std::fs::remove_file(&credentials_path).unwrap();
    }

    #[tokio::test]
    async fn test_balanced_mode_uses_session_affinity() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut cred1 = KiroCredentials {
            id: Some(1),
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test_1".to_string()),
            ..Default::default()
        };
        cred1.priority = 0;
        let mut cred2 = KiroCredentials {
            id: Some(2),
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test_2".to_string()),
            ..Default::default()
        };
        cred2.priority = 1;

        let manager = MultiTokenManager::new(config, vec![cred1, cred2], None, None, true).unwrap();

        let (first, _g1) = manager
            .acquire_context_for_session(None, Some("conversation-a"))
            .await
            .unwrap();
        manager.report_success(first.id);

        let (same_session, _g2) = manager
            .acquire_context_for_session(None, Some("conversation-a"))
            .await
            .unwrap();
        assert_eq!(same_session.id, first.id);

        let (other_session, _g3) = manager
            .acquire_context_for_session(None, Some("conversation-b"))
            .await
            .unwrap();
        assert_ne!(other_session.id, first.id);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(1);
        }
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(2);
        }

        assert_eq!(manager.available_count(), 0);

        // 应触发自愈：重置失败计数并重新启用，避免必须重启进程
        let (ctx, _guard) = manager.acquire_context(None).await.unwrap();
        assert!(ctx.token == "t1" || ctx.token == "t2");
        assert_eq!(manager.available_count(), 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_balanced_retries_until_bad_credential_disabled()
     {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut bad_cred = KiroCredentials::default();
        bad_cred.priority = 0;
        bad_cred.refresh_token = Some("bad".to_string());

        let mut good_cred = KiroCredentials::default();
        good_cred.priority = 1;
        good_cred.access_token = Some("good-token".to_string());
        good_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();

        let (ctx, _guard) = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(ctx.token, "good-token");
    }

    #[test]
    fn test_multi_token_manager_report_refresh_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        assert_eq!(manager.available_count(), 2);
        for _ in 0..(MAX_FAILURES_PER_CREDENTIAL - 1) {
            assert!(manager.report_refresh_failure(1));
        }
        assert_eq!(manager.available_count(), 2);

        assert!(manager.report_refresh_failure(1));
        assert_eq!(manager.available_count(), 1);

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        assert!(first.disabled);
        assert_eq!(first.refresh_failure_count, MAX_FAILURES_PER_CREDENTIAL);
        assert_eq!(snapshot.current_id, 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_refresh_failure_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_refresh_failure(1);
            manager.report_refresh_failure(2);
        }
        assert_eq!(manager.available_count(), 0);

        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
    }

    #[test]
    fn test_multi_token_manager_report_quota_exhausted() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_quota_exhausted(1));
        assert_eq!(manager.available_count(), 1);

        // 再禁用第二个后，无可用凭据
        assert!(!manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 0);
    }

    #[tokio::test]
    async fn test_multi_token_manager_quota_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.report_quota_exhausted(1);
        manager.report_quota_exhausted(2);
        assert_eq!(manager.available_count(), 0);

        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
        assert_eq!(manager.available_count(), 0);
    }

    // ============ 凭据级 Region 优先级测试 ============

    #[test]
    fn test_credential_region_priority_uses_credential_auth_region() {
        // 凭据配置了 auth_region 时，应使用凭据的 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("eu-west-1".to_string());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "eu-west-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_credential_region() {
        // 凭据未配置 auth_region 但配置了 region 时，应回退到凭据.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-central-1".to_string());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "eu-central-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_config() {
        // 凭据未配置 auth_region 和 region 时，应回退到 config
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let credentials = KiroCredentials::default();
        assert!(credentials.auth_region.is_none());
        assert!(credentials.region.is_none());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_multiple_credentials_use_respective_regions() {
        // 多凭据场景下，不同凭据使用各自的 auth_region
        let mut config = Config::default();
        config.region = "ap-northeast-1".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.auth_region = Some("us-east-1".to_string());

        let mut cred2 = KiroCredentials::default();
        cred2.region = Some("eu-west-1".to_string());

        let cred3 = KiroCredentials::default(); // 无 region，使用 config

        assert_eq!(cred1.effective_auth_region(&config), "us-east-1");
        assert_eq!(cred2.effective_auth_region(&config), "eu-west-1");
        assert_eq!(cred3.effective_auth_region(&config), "ap-northeast-1");
    }

    #[test]
    fn test_idc_oidc_endpoint_uses_credential_auth_region() {
        // 验证 IdC OIDC endpoint URL 使用凭据 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("eu-central-1".to_string());

        let region = credentials.effective_auth_region(&config);
        let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);

        assert_eq!(refresh_url, "https://oidc.eu-central-1.amazonaws.com/token");
    }

    #[test]
    fn test_social_refresh_endpoint_uses_credential_auth_region() {
        // 验证 Social refresh endpoint URL 使用凭据 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("ap-southeast-1".to_string());

        let region = credentials.effective_auth_region(&config);
        let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);

        assert_eq!(
            refresh_url,
            "https://prod.ap-southeast-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    #[test]
    fn test_api_call_uses_effective_api_region() {
        // 验证 API 调用使用 effective_api_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        // 凭据.region 不参与 api_region 回退链
        let api_region = credentials.effective_api_region(&config);
        let api_host = format!("q.{}.amazonaws.com", api_region);

        assert_eq!(api_host, "q.us-west-2.amazonaws.com");
    }

    #[test]
    fn test_api_call_uses_credential_api_region() {
        // 凭据配置了 api_region 时，API 调用应使用凭据的 api_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.api_region = Some("eu-central-1".to_string());

        let api_region = credentials.effective_api_region(&config);
        let api_host = format!("q.{}.amazonaws.com", api_region);

        assert_eq!(api_host, "q.eu-central-1.amazonaws.com");
    }

    #[test]
    fn test_credential_region_empty_string_treated_as_set() {
        // 空字符串 auth_region 被视为已设置（虽然不推荐，但行为应一致）
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("".to_string());

        let region = credentials.effective_auth_region(&config);
        // 空字符串被视为已设置，不会回退到 config
        assert_eq!(region, "");
    }

    #[test]
    fn test_auth_and_api_region_independent() {
        // auth_region 和 api_region 互不影响
        let mut config = Config::default();
        config.region = "default".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("auth-only".to_string());
        credentials.api_region = Some("api-only".to_string());

        assert_eq!(credentials.effective_auth_region(&config), "auth-only");
        assert_eq!(credentials.effective_api_region(&config), "api-only");
    }

    // ============ 并发控制（least-active + 硬上限 + RAII 守卫）测试 ============

    /// 构造 n 个带有效 token 的 API Key 凭据（无需网络刷新），返回 Arc 管理器（已 init_weak_self）。
    fn make_concurrency_manager(n: usize, mode: &str) -> std::sync::Arc<MultiTokenManager> {
        let mut config = Config::default();
        config.load_balancing_mode = mode.to_string();
        let creds: Vec<KiroCredentials> = (0..n)
            .map(|i| {
                let mut c = KiroCredentials::default();
                c.id = Some((i + 1) as u64);
                c.auth_method = Some("api_key".to_string());
                c.kiro_api_key = Some(format!("ksk_test_{}", i + 1));
                c.priority = 0;
                c
            })
            .collect();
        let manager = MultiTokenManager::new(config, creds, None, None, true).unwrap();
        let manager = std::sync::Arc::new(manager);
        manager.init_weak_self();
        manager
    }

    fn active_of(manager: &MultiTokenManager, id: u64) -> u32 {
        manager
            .snapshot()
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.active_concurrency)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn test_least_active_spreads_load_across_credentials() {
        // 5 个凭据，持有 5 个并发槽位不释放 → 应均匀摊到每个凭据各 1（不踩踏）
        let manager = make_concurrency_manager(5, "balanced");
        let mut guards = Vec::new();
        for _ in 0..5 {
            let (_ctx, guard) = manager.acquire_context(None).await.unwrap();
            guards.push(guard);
        }
        for id in 1..=5 {
            assert_eq!(
                active_of(&manager, id),
                1,
                "凭据 #{} 应恰好分到 1 个在途请求",
                id
            );
        }
        // 释放全部守卫后，active 应全部归零（无泄漏）
        drop(guards);
        for id in 1..=5 {
            assert_eq!(active_of(&manager, id), 0, "凭据 #{} 释放后应归零", id);
        }
    }

    #[tokio::test]
    async fn test_guard_drop_releases_active_slot() {
        let manager = make_concurrency_manager(1, "balanced");
        {
            let (_ctx, _guard) = manager.acquire_context(None).await.unwrap();
            assert_eq!(active_of(&manager, 1), 1, "占用后 active=1");
        }
        assert_eq!(active_of(&manager, 1), 0, "守卫 drop 后 active 归零");
    }

    #[tokio::test]
    async fn test_max_concurrency_busy_when_all_full() {
        // 单凭据 max_concurrency=1：占住后第二个请求应等待并最终返回繁忙
        let manager = make_concurrency_manager(1, "balanced");
        manager.set_max_concurrency(1, 1).unwrap();

        let (_ctx, _guard) = manager.acquire_context(None).await.unwrap();
        assert_eq!(active_of(&manager, 1), 1);

        // 第二个请求：满载，短等待后繁忙
        let started = Instant::now();
        let err = manager.acquire_context(None).await.err().unwrap();
        assert!(
            err.to_string().contains("CONCURRENCY_BUSY"),
            "应返回繁忙错误，实际: {}",
            err
        );
        // 应确实等待过（接近等待预算），而非立即失败
        let elapsed = started.elapsed();
        assert!(elapsed >= CONCURRENCY_WAIT_BUDGET / 2);
        // 且只等待「一个」预算窗口即返回，不应是 max_attempts 轮叠加的多倍预算
        // （token_manager 层单次 acquire 只等一个 budget；provider 层识别 CONCURRENCY_BUSY
        //  后直接返回 429，不再多轮重试 acquire）。留 2x 余量吸收调度抖动。
        assert!(
            elapsed < CONCURRENCY_WAIT_BUDGET * 2,
            "单次 acquire 应只等待约一个预算窗口，实际 {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_max_concurrency_skips_full_and_picks_free() {
        // 2 凭据各 max=1：占住 #1 后，下一个请求应跳过满载的 #1 选中空闲的 #2
        let manager = make_concurrency_manager(2, "balanced");
        manager.set_max_concurrency(1, 1).unwrap();
        manager.set_max_concurrency(2, 1).unwrap();

        let (first, _g1) = manager.acquire_context(None).await.unwrap();
        let (second, _g2) = manager.acquire_context(None).await.unwrap();
        assert_ne!(first.id, second.id, "第二个请求应换到另一个未满凭据");
        assert_eq!(active_of(&manager, 1), 1);
        assert_eq!(active_of(&manager, 2), 1);
    }

    #[tokio::test]
    async fn test_priority_mode_only_uses_highest_tier_when_unlimited() {
        // priority 模式 + max_concurrency=0：永远只用最高优先级档，低优先级 active 始终 0
        let mut config = Config::default();
        config.load_balancing_mode = "priority".to_string();
        let mut high = KiroCredentials::default();
        high.id = Some(1);
        high.auth_method = Some("api_key".to_string());
        high.kiro_api_key = Some("ksk_high".to_string());
        high.priority = 0;
        let mut low = KiroCredentials::default();
        low.id = Some(2);
        low.auth_method = Some("api_key".to_string());
        low.kiro_api_key = Some("ksk_low".to_string());
        low.priority = 1;
        let manager = MultiTokenManager::new(config, vec![high, low], None, None, true).unwrap();
        let manager = std::sync::Arc::new(manager);
        manager.init_weak_self();

        let mut guards = Vec::new();
        for _ in 0..5 {
            let (ctx, guard) = manager.acquire_context(None).await.unwrap();
            assert_eq!(ctx.id, 1, "priority 模式不限流时只用最高优先级档 #1");
            guards.push(guard);
        }
        assert_eq!(active_of(&manager, 2), 0, "低优先级 #2 始终不被使用");
    }

    #[tokio::test]
    async fn test_priority_mode_falls_to_next_tier_when_top_full() {
        // priority 模式 + 高优先级档设上限并打满 → 落到下一档
        let mut config = Config::default();
        config.load_balancing_mode = "priority".to_string();
        let mut high = KiroCredentials::default();
        high.id = Some(1);
        high.auth_method = Some("api_key".to_string());
        high.kiro_api_key = Some("ksk_high".to_string());
        high.priority = 0;
        let mut low = KiroCredentials::default();
        low.id = Some(2);
        low.auth_method = Some("api_key".to_string());
        low.kiro_api_key = Some("ksk_low".to_string());
        low.priority = 1;
        let manager = MultiTokenManager::new(config, vec![high, low], None, None, true).unwrap();
        let manager = std::sync::Arc::new(manager);
        manager.init_weak_self();
        manager.set_max_concurrency(1, 1).unwrap();

        let (first, _g1) = manager.acquire_context(None).await.unwrap();
        assert_eq!(first.id, 1, "首个请求用最高优先级 #1");
        // #1 满载 → 第二个落到 #2
        let (second, g2) = manager.acquire_context(None).await.unwrap();
        assert_eq!(second.id, 2, "高优先级满载时应落到下一档 #2");

        // 释放 #1 的槽位（_g1 仍持有，这里释放 #2 不影响）；真正要验证的是：
        // 当 #1 不再满载后，下一个请求应回到最高优先级档 #1，而不是粘在 #2。
        drop(g2);
        drop(_g1); // #1 释放，高档恢复可用
        let (third, _g3) = manager.acquire_context(None).await.unwrap();
        assert_eq!(third.id, 1, "高优先级档解除满载后应回到 #1，不粘在低档");
    }

    #[tokio::test]
    async fn test_priority_mode_spreads_within_same_tier() {
        // priority 模式 + 同一优先级档多个号 + 不限流：
        // 应在档内按 active least-active 摊开，而不是一直钉住同一个号
        let mut config = Config::default();
        config.load_balancing_mode = "priority".to_string();
        let creds: Vec<KiroCredentials> = (0..3)
            .map(|i| {
                let mut c = KiroCredentials::default();
                c.id = Some((i + 1) as u64);
                c.auth_method = Some("api_key".to_string());
                c.kiro_api_key = Some(format!("ksk_{}", i + 1));
                c.priority = 0; // 同一优先级档
                c
            })
            .collect();
        let manager = MultiTokenManager::new(config, creds, None, None, true).unwrap();
        let manager = std::sync::Arc::new(manager);
        manager.init_weak_self();

        // 持有 3 个并发不释放 → 同档 3 个号应各分到 1（least-active 摊开，不踩踏）
        let mut guards = Vec::new();
        for _ in 0..3 {
            let (_ctx, guard) = manager.acquire_context(None).await.unwrap();
            guards.push(guard);
        }
        for id in 1..=3 {
            assert_eq!(
                active_of(&manager, id),
                1,
                "同优先级档内凭据 #{} 应分到 1 个在途（档内 least-active 摊开）",
                id
            );
        }
    }

    #[tokio::test]
    async fn test_request_count_independent_from_active() {
        // active 与 request_count 独立：成功 acquire 后 request_count +1，active 由守卫管理
        let manager = make_concurrency_manager(1, "balanced");
        {
            let (_ctx, _guard) = manager.acquire_context(None).await.unwrap();
            assert_eq!(manager.get_request_count(1), 1);
            assert_eq!(active_of(&manager, 1), 1);
        }
        // 守卫释放后 request_count 不回退，active 归零
        assert_eq!(manager.get_request_count(1), 1);
        assert_eq!(active_of(&manager, 1), 0);
    }

    // ====================================================================
    // 粘性会话「绑定号满载过载」单元测试（方案 §3.6.4 第二阶段）
    // 与 E2E 互补：这里用 api_key 凭据（try_ensure_token 不走网络）确定性地
    // 覆盖 wait_for_sticky 的三个分支：等到原号 / 超时换号 / 超阈值换号。
    // ====================================================================

    /// 把会话绑定到指定凭据并占满它（max=1），返回占满它的那个 guard。
    async fn bind_and_saturate(
        manager: &std::sync::Arc<MultiTokenManager>,
        session: &str,
    ) -> (u64, ConcurrencyGuard) {
        // 首次 acquire 建立绑定
        let (ctx, guard) = manager
            .acquire_context_for_session(None, Some(session))
            .await
            .unwrap();
        let bound = ctx.id;
        // 给绑定号设上限 1：此时它已被这个 guard 占用 active=1，即满载
        manager.set_max_concurrency(bound, 1).unwrap();
        (bound, guard)
    }

    #[tokio::test]
    async fn test_sticky_overload_waits_then_gets_original_when_released_in_budget() {
        // 绑定号满载，但在等待预算内释放 → 粘性请求等到后仍用原号（保缓存）
        let manager = make_concurrency_manager(2, "balanced");
        let session = "sticky-A";
        let (bound, guard) = bind_and_saturate(&manager, session).await;
        assert_eq!(active_of(&manager, bound), 1, "绑定号已满载");

        // 在 ~150ms 后释放占位（远小于 2s 预算）
        let m2 = manager.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(StdDuration::from_millis(150)).await;
            drop(guard); // 释放绑定号槽位
            let _ = m2; // keep alive
        });

        // 同会话请求：应短等后抢回原号
        let (ctx, _g) = manager
            .acquire_context_for_session(None, Some(session))
            .await
            .unwrap();
        releaser.await.unwrap();
        assert_eq!(ctx.id, bound, "预算内释放后应仍命中原绑定号 #{}", bound);
        assert!(ctx.session_affinity_hit, "应标记为命中会话亲和");
    }

    #[tokio::test]
    async fn test_sticky_overload_switches_when_budget_times_out() {
        // 绑定号满载且全程不释放 → 等满预算后放弃亲和，换到另一个号并重绑
        let manager = make_concurrency_manager(2, "balanced");
        let session = "sticky-B";
        let (bound, _hold) = bind_and_saturate(&manager, session).await; // _hold 全程持有，绝不释放
        let other = if bound == 1 { 2 } else { 1 };

        let started = Instant::now();
        let (ctx, _g) = manager
            .acquire_context_for_session(None, Some(session))
            .await
            .unwrap();
        let waited = started.elapsed();

        assert_eq!(ctx.id, other, "超时后应换到另一个号 #{}", other);
        assert!(!ctx.session_affinity_hit, "换号请求不应标记亲和命中");
        // 确实等满了一个预算窗口才换（不是秒换），证明"宁可短等保缓存"
        assert!(
            waited >= CONCURRENCY_WAIT_BUDGET,
            "应等满等待预算后才换号，实际等待 {:?}",
            waited
        );

        // 重绑验证：原号仍满载时，同会话再请求应稳定到新号 other
        let (ctx2, _g2) = manager
            .acquire_context_for_session(None, Some(session))
            .await
            .unwrap();
        assert_eq!(ctx2.id, other, "换号后会话应已重绑到新号 #{}", other);
        assert!(ctx2.session_affinity_hit, "重绑后应命中亲和");
    }

    #[tokio::test]
    async fn test_sticky_overload_gives_up_when_waiters_reach_threshold() {
        // 等待者达阈值(STICKY_MAX_WAITING=2)时，新的（第 3 个）等待者立即放弃换号，不再排队。
        // 构造：绑定号(max=1)满载且不释放；另起 2 个同会话请求占满 waiting 名额。
        let manager = make_concurrency_manager(2, "balanced");
        let session = "sticky-C";
        let (bound, _hold) = bind_and_saturate(&manager, session).await;
        let other = if bound == 1 { 2 } else { 1 };

        // 先制造 2 个在等的请求（它们会卡在 wait_for_sticky 轮询里，把 waiting 抬到 2）
        let m1 = manager.clone();
        let s1 = session.to_string();
        let w1 = tokio::spawn(async move {
            m1.acquire_context_for_session(None, Some(&s1)).await
        });
        let m2 = manager.clone();
        let s2 = session.to_string();
        let w2 = tokio::spawn(async move {
            m2.acquire_context_for_session(None, Some(&s2)).await
        });

        // 等到 waiting 抬到 2（阈值）
        let mut waited_up = false;
        for _ in 0..100 {
            if manager
                .snapshot()
                .entries
                .iter()
                .find(|e| e.id == bound)
                .map(|e| e.waiting_concurrency)
                .unwrap_or(0)
                >= STICKY_MAX_WAITING
            {
                waited_up = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        assert!(waited_up, "应有 2 个等待者把 waiting 抬到阈值");

        // 第 3 个同会话请求：因 waiting 已达阈值，应立即放弃亲和换号（不再排队），用时远小于预算
        let started = Instant::now();
        let (ctx3, _g3) = manager
            .acquire_context_for_session(None, Some(session))
            .await
            .unwrap();
        let waited3 = started.elapsed();
        assert_eq!(ctx3.id, other, "超阈值的请求应立即换到备用号 #{}", other);
        assert!(
            waited3 < CONCURRENCY_WAIT_BUDGET,
            "超阈值应立即换号而非等满预算，实际 {:?}",
            waited3
        );

        // 收尾：前两个等待者最终也会超时换号（不阻塞测试结束）
        let _ = tokio::time::timeout(CONCURRENCY_WAIT_BUDGET * 2, async {
            let _ = w1.await;
            let _ = w2.await;
        })
        .await;
    }
}
