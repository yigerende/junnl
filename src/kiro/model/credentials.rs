//! Kiro OAuth 凭证数据模型
//!
//! 支持从 Kiro IDE 的凭证文件加载，使用 Social 认证方式
//! 支持单凭据和多凭据配置格式

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::http_client::ProxyConfig;
use crate::model::config::Config;

/// Kiro IDE 给 Builder ID 写入的占位 profile ARN。
///
/// 该 ARN 不是有效 profile；Builder ID 账号请求上游 API 时不能携带它。
pub const KIRO_BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

/// Google / GitHub Social 登录账号的默认 profile ARN。
pub const KIRO_SOCIAL_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";

const KIRO_ENTERPRISE_FALLBACK_ACCOUNT_ID: &str = "610548660232";
const KIRO_ENTERPRISE_FALLBACK_PROFILE_ID: &str = "VNECVYCYYAWN";

/// Enterprise / IAM Identity Center 自动获取 profile 失败时使用的 fallback ARN。
pub fn enterprise_fallback_profile_arn(region: Option<&str>) -> String {
    let region = region.map(str::trim).unwrap_or_default();
    let region = if region.to_ascii_lowercase().starts_with("eu-") {
        "eu-central-1"
    } else {
        "us-east-1"
    };

    format!(
        "arn:aws:codewhisperer:{region}:{KIRO_ENTERPRISE_FALLBACK_ACCOUNT_ID}:profile/{KIRO_ENTERPRISE_FALLBACK_PROFILE_ID}"
    )
}

/// Kiro OAuth 凭证
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroCredentials {
    /// 凭据唯一标识符（自增 ID）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,

    /// 访问令牌
    #[serde(alias = "access_token")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,

    /// 刷新令牌
    #[serde(alias = "refresh_token")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,

    /// Profile ARN
    #[serde(alias = "profile_arn")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,

    /// 过期时间 (RFC3339 格式)
    #[serde(alias = "expires_at")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    /// 认证方式 (social / idc)
    #[serde(alias = "auth_method")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,

    /// 登录 Provider（Google / Github / BuilderId / Enterprise 等）
    ///
    /// 旧配置可能没有该字段，运行时会根据 authMethod/clientId 做兼容推断。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// SSO Start URL（Enterprise / IAM Identity Center 账号使用）
    #[serde(alias = "start_url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_url: Option<String>,

    /// OIDC Client ID (IdC 认证需要)
    #[serde(alias = "client_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,

    /// OIDC Client Secret (IdC 认证需要)
    #[serde(alias = "client_secret")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,

    /// 凭据优先级（数字越小优先级越高，默认为 0）
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    #[serde(alias = "auth_region")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    #[serde(alias = "api_region")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    /// 凭据级 Machine ID 配置（可选）
    /// 未配置时回退到 config.json 的 machineId；都未配置时运行时生成随机值并写回
    #[serde(alias = "machine_id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,

    /// 用户邮箱（从 Anthropic API 获取）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// 订阅等级（KIRO PRO+ / KIRO FREE 等）
    #[serde(alias = "subscription_title")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub subscription_title: Option<String>,

    /// 凭据级代理 URL（可选）
    /// 支持 http/https/socks5 协议
    /// 特殊值 "direct" 表示显式不使用代理（即使全局配置了代理）
    /// 未配置时回退到全局代理配置
    #[serde(alias = "proxy_url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    #[serde(alias = "proxy_username")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    #[serde(alias = "proxy_password")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,

    /// 代理池代理 ID 列表，按优先级从高到低排列。
    ///
    /// 新版 Admin UI 使用该字段引用 config.json 中的 proxies；为空时继续兼容旧的
    /// proxyUrl/proxyUsername/proxyPassword 字段。
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proxy_ids: Vec<u64>,

    /// 凭据是否被禁用（默认为 false）
    #[serde(default)]
    pub disabled: bool,

    /// Kiro API Key（headless 模式）
    /// 格式: ksk_xxxxxxxx
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(alias = "kiro_api_key")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,

    /// 端点名称（可选）
    ///
    /// 决定该凭据走哪套 Kiro API。未配置时回退到 `config.defaultEndpoint`（默认 "ide"）。
    /// 端点名必须在启动时注册的端点 registry 中存在。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// 凭据并发硬上限（0 = 不限制，默认）
    ///
    /// 当 > 0 时，调度层会跳过 `active >= max_concurrency` 的凭据（满载），
    /// 实现第二层兜底保护，避免单个凭据被打爆。旧配置文件无此字段时默认 0。
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    pub max_concurrency: u32,
}

/// 判断是否为零（用于跳过序列化）
fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn canonicalize_auth_method_value(value: &str) -> &str {
    if value.eq_ignore_ascii_case("builder-id") || value.eq_ignore_ascii_case("iam") {
        "idc"
    } else if value.eq_ignore_ascii_case("api_key") || value.eq_ignore_ascii_case("apikey") {
        "api_key"
    } else {
        value
    }
}

fn is_builder_id_provider_value(value: &str) -> bool {
    let normalized = value
        .trim()
        .chars()
        .filter(|c| *c != '-' && *c != '_' && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    normalized == "builderid" || normalized == "awsbuilderid"
}

#[allow(dead_code)] // 上游 Builder ID 判定辅助，暂未接线，保留备用
fn is_builder_id_auth_method_value(value: &str) -> bool {
    is_builder_id_provider_value(value)
}

fn is_enterprise_provider_value(value: &str) -> bool {
    value.eq_ignore_ascii_case("enterprise") || value.eq_ignore_ascii_case("externalidp")
}

fn is_social_provider_value(value: &str) -> bool {
    value.eq_ignore_ascii_case("github") || value.eq_ignore_ascii_case("google")
}

fn valid_explicit_profile_arn(value: Option<&str>) -> Option<&str> {
    let trimmed = value?.trim();
    if trimmed.is_empty() || trimmed == KIRO_BUILDER_ID_PROFILE_ARN {
        None
    } else {
        Some(trimmed)
    }
}

/// 凭据配置（支持单对象或数组格式）
///
/// 自动识别配置文件格式：
/// - 单对象格式（旧格式，向后兼容）
/// - 数组格式（新格式，支持多凭据）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CredentialsConfig {
    /// 单个凭据（旧格式）
    Single(KiroCredentials),
    /// 多凭据数组（新格式）
    Multiple(Vec<KiroCredentials>),
}

impl CredentialsConfig {
    /// 从文件加载凭据配置
    ///
    /// - 如果文件不存在，返回空数组
    /// - 如果文件内容为空，返回空数组
    /// - 支持单对象或数组格式
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();

        // 文件不存在时返回空数组
        if !path.exists() {
            return Ok(CredentialsConfig::Multiple(vec![]));
        }

        let content = fs::read_to_string(path)?;

        // 文件为空时返回空数组
        if content.trim().is_empty() {
            return Ok(CredentialsConfig::Multiple(vec![]));
        }

        let config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// 转换为按优先级排序的凭据列表
    pub fn into_sorted_credentials(self) -> Vec<KiroCredentials> {
        match self {
            CredentialsConfig::Single(mut cred) => {
                cred.canonicalize_auth_method();
                vec![cred]
            }
            CredentialsConfig::Multiple(mut creds) => {
                // 按优先级排序（数字越小优先级越高）
                creds.sort_by_key(|c| c.priority);
                for cred in &mut creds {
                    cred.canonicalize_auth_method();
                }
                creds
            }
        }
    }

    /// 判断是否为多凭据格式（数组格式）
    pub fn is_multiple(&self) -> bool {
        matches!(self, CredentialsConfig::Multiple(_))
    }
}

impl KiroCredentials {
    /// 特殊值：显式不使用代理
    pub const PROXY_DIRECT: &'static str = "direct";

    /// 获取默认凭证文件路径
    pub fn default_credentials_path() -> &'static str {
        "credentials.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    pub fn effective_auth_region<'a>(&'a self, config: &'a Config) -> &'a str {
        self.auth_region
            .as_deref()
            .or(self.region.as_deref())
            .unwrap_or(config.effective_auth_region())
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先级：凭据.api_region > config.api_region > config.region
    pub fn effective_api_region<'a>(&'a self, config: &'a Config) -> &'a str {
        self.api_region
            .as_deref()
            .unwrap_or(config.effective_api_region())
    }

    /// 获取有效的代理配置
    /// 优先级：凭据代理 > 全局代理 > 无代理
    /// 特殊值 "direct" 表示显式不使用代理（即使全局配置了代理）
    pub fn effective_proxy(&self, global_proxy: Option<&ProxyConfig>) -> Option<ProxyConfig> {
        match self.proxy_url.as_deref() {
            Some(url) if url.eq_ignore_ascii_case(Self::PROXY_DIRECT) => None,
            Some(url) => {
                let mut proxy = ProxyConfig::new(url);
                if let (Some(username), Some(password)) =
                    (&self.proxy_username, &self.proxy_password)
                {
                    proxy = proxy.with_auth(username, password);
                }
                Some(proxy)
            }
            None => global_proxy.cloned(),
        }
    }

    pub fn canonicalize_auth_method(&mut self) {
        let auth_method = match &self.auth_method {
            Some(m) => m,
            None => return,
        };

        let canonical = canonicalize_auth_method_value(auth_method);
        if canonical != auth_method {
            self.auth_method = Some(canonical.to_string());
        }
    }

    /// 判断凭据是否按 IdC / Builder ID 认证处理。
    pub fn is_idc_auth(&self) -> bool {
        self.auth_method
            .as_deref()
            .map(|m| {
                m.eq_ignore_ascii_case("idc")
                    || m.eq_ignore_ascii_case("builder-id")
                    || m.eq_ignore_ascii_case("iam")
                    || m.eq_ignore_ascii_case("external_idp")
            })
            .unwrap_or_else(|| self.client_id.is_some() && self.client_secret.is_some())
    }

    /// 判断凭据是否按 Social 认证处理。
    pub fn is_social_auth(&self) -> bool {
        if self.is_api_key_credential() {
            return false;
        }

        let provider_is_non_social = self
            .provider
            .as_deref()
            .is_some_and(|p| is_enterprise_provider_value(p) || is_builder_id_provider_value(p));
        if provider_is_non_social {
            return false;
        }

        let provider_is_social = self
            .provider
            .as_deref()
            .is_some_and(is_social_provider_value);

        let auth_is_social = self
            .auth_method
            .as_deref()
            .map(|m| m.eq_ignore_ascii_case("social"))
            .unwrap_or(false);

        provider_is_social || auth_is_social || !self.is_idc_auth()
    }

    /// 判断凭据是否是 Enterprise / IAM Identity Center / External IdP。
    pub fn is_enterprise_auth(&self) -> bool {
        if self.is_api_key_credential() {
            return false;
        }

        if self
            .auth_method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("social"))
            || self
                .provider
                .as_deref()
                .is_some_and(is_social_provider_value)
        {
            return false;
        }

        let provider_is_enterprise = self
            .provider
            .as_deref()
            .is_some_and(is_enterprise_provider_value);

        let auth_is_external_idp = self
            .auth_method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("external_idp"));

        let has_enterprise_start_url = self
            .start_url
            .as_deref()
            .map(str::trim)
            .is_some_and(|url| !url.is_empty());

        provider_is_enterprise || auth_is_external_idp || has_enterprise_start_url
    }

    /// 判断凭据是否是 External IdP；流式接口需要额外 TokenType header。
    pub fn is_external_idp_auth(&self) -> bool {
        self.auth_method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("external_idp"))
            || self
                .provider
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case("externalidp"))
    }

    /// 判断凭据是否明确标识为 AWS Builder ID。
    #[allow(dead_code)] // 上游 Builder ID 判定辅助，暂未接线，保留备用
    pub fn is_builder_id_auth(&self) -> bool {
        if self.is_api_key_credential() || self.is_social_auth() || self.is_enterprise_auth() {
            return false;
        }

        let provider_is_builder_id = self
            .provider
            .as_deref()
            .is_some_and(is_builder_id_provider_value);

        let auth_method_is_builder_id = self
            .auth_method
            .as_deref()
            .is_some_and(is_builder_id_auth_method_value);

        provider_is_builder_id || auth_method_is_builder_id
    }

    /// 判断 ARN 是否为 Kiro IDE Builder ID 占位符。
    pub fn is_placeholder_profile_arn(profile_arn: &str) -> bool {
        profile_arn.trim() == KIRO_BUILDER_ID_PROFILE_ARN
    }

    /// 判断是否应该尝试通过 ListAvailableProfiles 自动获取真实 profileArn。
    pub fn should_fetch_profile_arn(&self) -> bool {
        if self.is_api_key_credential() {
            return false;
        }

        self.profile_arn
            .as_deref()
            .map(str::trim)
            .map(|arn| arn.is_empty() || Self::is_placeholder_profile_arn(arn))
            .unwrap_or(true)
    }

    /// 获取请求 Kiro API 时应使用的 profileArn。
    ///
    /// 与 KAM 1.7.5 的 resolveProfileArn 保持一致：
    /// 显式真实 ARN > Enterprise fallback > Social 固定 ARN > Builder ID 占位符。
    /// 注意：Builder ID 占位符只适合 ListAvailableModels，不适合流式生成接口。
    pub fn resolved_profile_arn(&self) -> Option<String> {
        if self.is_api_key_credential() {
            return None;
        }

        if let Some(profile_arn) = valid_explicit_profile_arn(self.profile_arn.as_deref()) {
            return Some(profile_arn.to_string());
        }

        if self.is_enterprise_auth() {
            Some(enterprise_fallback_profile_arn(
                self.region.as_deref().or(self.api_region.as_deref()),
            ))
        } else if self.is_social_auth() {
            Some(KIRO_SOCIAL_PROFILE_ARN.to_string())
        } else {
            Some(KIRO_BUILDER_ID_PROFILE_ARN.to_string())
        }
    }

    /// 获取流式生成请求中应传递的 profileArn。
    ///
    /// Enterprise fallback / 真实 ARN / Social ARN 都会发送；Builder ID 占位符会移除。
    pub fn resolved_stream_profile_arn(&self) -> Option<String> {
        let arn = self.resolved_profile_arn()?;
        if Self::is_placeholder_profile_arn(&arn) && !self.is_enterprise_auth() {
            None
        } else {
            Some(arn)
        }
    }

    /// 检查凭据是否支持 Opus 模型
    ///
    /// Free 账号不支持 Opus 模型，需要 PRO 或更高等级订阅
    pub fn supports_opus(&self) -> bool {
        match &self.subscription_title {
            Some(title) => {
                let title_upper = title.to_uppercase();
                // 如果包含 FREE，则不支持 Opus
                !title_upper.contains("FREE")
            }
            // 如果还没有获取订阅信息，暂时允许（首次使用时会获取）
            None => true,
        }
    }

    /// 检查是否为 API Key 凭据
    ///
    /// API Key 凭据直接使用 kiro_api_key 作为 Bearer Token，无需 refreshToken
    pub fn is_api_key_credential(&self) -> bool {
        self.kiro_api_key.is_some()
            || self
                .auth_method
                .as_deref()
                .map(|m| m.eq_ignore_ascii_case("api_key") || m.eq_ignore_ascii_case("apikey"))
                .unwrap_or(false)
    }
}

#[cfg(test)]
impl KiroCredentials {
    fn from_json(json_string: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_string)
    }

    fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn test_from_json() {
        let json = r#"{
            "accessToken": "test_token",
            "refreshToken": "test_refresh",
            "profileArn": "arn:aws:test",
            "expiresAt": "2024-01-01T00:00:00Z",
            "authMethod": "social"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.access_token, Some("test_token".to_string()));
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.profile_arn, Some("arn:aws:test".to_string()));
        assert_eq!(creds.expires_at, Some("2024-01-01T00:00:00Z".to_string()));
        assert_eq!(creds.auth_method, Some("social".to_string()));
    }

    #[test]
    fn test_from_json_accepts_snake_case_windows_idc_export() {
        let json = r#"{
            "type": "kiro",
            "access_token": "test_access",
            "refresh_token": "test_refresh",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123:profile/REAL",
            "expires_at": "2026-06-05T09:38:26.836Z",
            "auth_method": "idc",
            "provider": "Enterprise",
            "last_refresh": "2026-06-05T08:38:26.836Z",
            "email": "user@example.com",
            "client_id": "client123",
            "client_secret": "secret456",
            "region": "us-east-1",
            "start_url": "https://d-example.awsapps.com/start",
            "machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.access_token, Some("test_access".to_string()));
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(
            creds.profile_arn,
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string())
        );
        assert_eq!(creds.auth_method, Some("idc".to_string()));
        assert_eq!(creds.provider, Some("Enterprise".to_string()));
        assert_eq!(creds.client_id, Some("client123".to_string()));
        assert_eq!(creds.client_secret, Some("secret456".to_string()));
        assert_eq!(
            creds.start_url,
            Some("https://d-example.awsapps.com/start".to_string())
        );
        assert_eq!(
            creds.machine_id,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())
        );
    }

    #[test]
    fn test_from_json_with_unknown_keys() {
        let json = r#"{
            "accessToken": "test_token",
            "unknownField": "should be ignored"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.access_token, Some("test_token".to_string()));
    }

    #[test]
    fn test_to_json() {
        let creds = KiroCredentials {
            id: None,
            access_token: Some("token".to_string()),
            refresh_token: None,
            profile_arn: None,
            expires_at: None,
            auth_method: Some("social".to_string()),
            provider: None,
            start_url: None,
            client_id: None,
            client_secret: None,
            priority: 0,
            region: None,
            auth_region: None,
            api_region: None,
            machine_id: None,
            email: None,
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_ids: Vec::new(),
            disabled: false,
            kiro_api_key: None,
            endpoint: None,
            max_concurrency: 0,
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("accessToken"));
        assert!(json.contains("authMethod"));
        assert!(!json.contains("refreshToken"));
        // priority 为 0 时不序列化
        assert!(!json.contains("priority"));
    }

    #[test]
    fn test_resolved_profile_arn_social_default() {
        let creds = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some(KIRO_SOCIAL_PROFILE_ARN)
        );
    }

    #[test]
    fn test_resolved_profile_arn_idc_without_profile_arn_uses_builder_placeholder() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert!(creds.is_idc_auth());
        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some(KIRO_BUILDER_ID_PROFILE_ARN)
        );
        assert_eq!(creds.resolved_stream_profile_arn(), None);
    }

    #[test]
    fn test_resolved_profile_arn_preserves_explicit_value() {
        let creds = KiroCredentials {
            auth_method: Some("social".to_string()),
            profile_arn: Some("arn:explicit".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some("arn:explicit")
        );
    }

    #[test]
    fn test_resolved_profile_arn_preserves_explicit_value_for_builder_id() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            provider: Some("BuilderId".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert!(creds.is_builder_id_auth());
        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
        assert_eq!(
            creds.resolved_stream_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
    }

    #[test]
    fn test_resolved_profile_arn_idc_without_start_url_keeps_real_explicit_arn() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert!(!creds.is_builder_id_auth());
        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
        assert_eq!(
            creds.resolved_stream_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
    }

    #[test]
    fn test_resolved_profile_arn_enterprise_with_start_url_keeps_real_explicit_arn() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string()),
            start_url: Some("https://d-example.awsapps.com/start/".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert!(!creds.is_builder_id_auth());
        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
    }

    #[test]
    fn test_resolved_profile_arn_keeps_builder_id_placeholder_for_models_only() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            profile_arn: Some(KIRO_BUILDER_ID_PROFILE_ARN.to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some(KIRO_BUILDER_ID_PROFILE_ARN)
        );
        assert_eq!(creds.resolved_stream_profile_arn(), None);
    }

    #[test]
    fn test_resolved_stream_profile_arn_omits_builder_placeholder() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_profile_arn().as_deref(),
            Some(KIRO_BUILDER_ID_PROFILE_ARN)
        );
        assert_eq!(creds.resolved_stream_profile_arn(), None);
    }

    #[test]
    fn test_resolved_stream_profile_arn_keeps_social_default() {
        let creds = KiroCredentials {
            auth_method: Some("social".to_string()),
            provider: Some("Github".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_stream_profile_arn().as_deref(),
            Some(KIRO_SOCIAL_PROFILE_ARN)
        );
    }

    #[test]
    fn test_resolved_stream_profile_arn_keeps_real_explicit_arn() {
        let creds = KiroCredentials {
            auth_method: Some("idc".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL".to_string()),
            start_url: Some("https://d-example.awsapps.com/start/".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };

        assert_eq!(
            creds.resolved_stream_profile_arn().as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123:profile/REAL")
        );
    }

    #[test]
    fn test_external_idp_auth_method_is_preserved_for_token_type_header() {
        let json = r#"[
            {"refreshToken": "test", "authMethod": "external_idp"}
        ]"#;

        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        let list = config.into_sorted_credentials();

        assert_eq!(list[0].auth_method.as_deref(), Some("external_idp"));
        assert!(list[0].is_idc_auth());
        assert!(list[0].is_external_idp_auth());
        assert_eq!(
            list[0].resolved_profile_arn(),
            Some(enterprise_fallback_profile_arn(None))
        );
    }

    #[test]
    fn test_start_url_roundtrip() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "authMethod": "idc",
            "startUrl": "https://d-example.awsapps.com/start/"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(
            creds.start_url.as_deref(),
            Some("https://d-example.awsapps.com/start/")
        );

        let serialized = creds.to_pretty_json().unwrap();
        assert!(serialized.contains("startUrl"));
        assert!(serialized.contains("https://d-example.awsapps.com/start/"));
    }

    #[test]
    fn test_default_credentials_path() {
        assert_eq!(
            KiroCredentials::default_credentials_path(),
            "credentials.json"
        );
    }

    #[test]
    fn test_priority_default() {
        let json = r#"{"refreshToken": "test"}"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.priority, 0);
    }

    #[test]
    fn test_priority_explicit() {
        let json = r#"{"refreshToken": "test", "priority": 5}"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.priority, 5);
    }

    #[test]
    fn test_credentials_config_single() {
        let json = r#"{"refreshToken": "test", "expiresAt": "2025-12-31T00:00:00Z"}"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, CredentialsConfig::Single(_)));
    }

    #[test]
    fn test_credentials_config_multiple() {
        let json = r#"[
            {"refreshToken": "test1", "priority": 1},
            {"refreshToken": "test2", "priority": 0}
        ]"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, CredentialsConfig::Multiple(_)));
        assert_eq!(config.into_sorted_credentials().len(), 2);
    }

    #[test]
    fn test_credentials_config_priority_sorting() {
        let json = r#"[
            {"refreshToken": "t1", "priority": 2},
            {"refreshToken": "t2", "priority": 0},
            {"refreshToken": "t3", "priority": 1}
        ]"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        let list = config.into_sorted_credentials();

        // 验证按优先级排序
        assert_eq!(list[0].refresh_token, Some("t2".to_string())); // priority 0
        assert_eq!(list[1].refresh_token, Some("t3".to_string())); // priority 1
        assert_eq!(list[2].refresh_token, Some("t1".to_string())); // priority 2
    }

    // ============ Region 字段测试 ============

    #[test]
    fn test_region_field_parsing() {
        // 测试解析包含 region 字段的 JSON
        let json = r#"{
            "refreshToken": "test_refresh",
            "region": "us-east-1"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.region, Some("us-east-1".to_string()));
    }

    #[test]
    fn test_region_field_missing_backward_compat() {
        // 测试向后兼容：不包含 region 字段的旧格式 JSON
        let json = r#"{
            "refreshToken": "test_refresh",
            "authMethod": "social"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.region, None);
    }

    #[test]
    fn test_region_field_serialization() {
        let creds = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: Some("test".to_string()),
            profile_arn: None,
            expires_at: None,
            auth_method: None,
            provider: None,
            start_url: None,
            client_id: None,
            client_secret: None,
            priority: 0,
            region: Some("eu-west-1".to_string()),
            auth_region: None,
            api_region: None,
            machine_id: None,
            email: None,
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_ids: Vec::new(),
            disabled: false,
            kiro_api_key: None,
            endpoint: None,
            max_concurrency: 0,
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("region"));
        assert!(json.contains("eu-west-1"));
    }

    #[test]
    fn test_region_field_none_not_serialized() {
        let creds = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: Some("test".to_string()),
            profile_arn: None,
            expires_at: None,
            auth_method: None,
            provider: None,
            start_url: None,
            client_id: None,
            client_secret: None,
            priority: 0,
            region: None,
            auth_region: None,
            api_region: None,
            machine_id: None,
            email: None,
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_ids: Vec::new(),
            disabled: false,
            kiro_api_key: None,
            endpoint: None,
            max_concurrency: 0,
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("region"));
    }

    // ============ MachineId 字段测试 ============

    #[test]
    fn test_machine_id_field_parsing() {
        let machine_id = "a".repeat(64);
        let json = format!(
            r#"{{
                "refreshToken": "test_refresh",
                "machineId": "{machine_id}"
            }}"#
        );

        let creds = KiroCredentials::from_json(&json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.machine_id, Some(machine_id));
    }

    #[test]
    fn test_machine_id_field_serialization() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.machine_id = Some("b".repeat(64));

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("machineId"));
    }

    #[test]
    fn test_machine_id_field_none_not_serialized() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.machine_id = None;

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("machineId"));
    }

    #[test]
    fn test_multiple_credentials_with_different_regions() {
        // 测试多凭据场景下不同凭据使用各自的 region
        let json = r#"[
            {"refreshToken": "t1", "region": "us-east-1"},
            {"refreshToken": "t2", "region": "eu-west-1"},
            {"refreshToken": "t3"}
        ]"#;

        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        let list = config.into_sorted_credentials();

        assert_eq!(list[0].region, Some("us-east-1".to_string()));
        assert_eq!(list[1].region, Some("eu-west-1".to_string()));
        assert_eq!(list[2].region, None);
    }

    #[test]
    fn test_region_field_with_all_fields() {
        // 测试包含所有字段的完整 JSON
        let json = r#"{
            "id": 1,
            "accessToken": "access",
            "refreshToken": "refresh",
            "profileArn": "arn:aws:test",
            "expiresAt": "2025-12-31T00:00:00Z",
            "authMethod": "idc",
            "clientId": "client123",
            "clientSecret": "secret456",
            "priority": 5,
            "region": "ap-northeast-1"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.id, Some(1));
        assert_eq!(creds.access_token, Some("access".to_string()));
        assert_eq!(creds.refresh_token, Some("refresh".to_string()));
        assert_eq!(creds.profile_arn, Some("arn:aws:test".to_string()));
        assert_eq!(creds.expires_at, Some("2025-12-31T00:00:00Z".to_string()));
        assert_eq!(creds.auth_method, Some("idc".to_string()));
        assert_eq!(creds.client_id, Some("client123".to_string()));
        assert_eq!(creds.client_secret, Some("secret456".to_string()));
        assert_eq!(creds.priority, 5);
        assert_eq!(creds.region, Some("ap-northeast-1".to_string()));
    }

    #[test]
    fn test_region_roundtrip() {
        // 测试序列化和反序列化的往返一致性
        let original = KiroCredentials {
            id: Some(42),
            access_token: Some("token".to_string()),
            refresh_token: Some("refresh".to_string()),
            profile_arn: None,
            expires_at: None,
            auth_method: Some("social".to_string()),
            provider: None,
            start_url: None,
            client_id: None,
            client_secret: None,
            priority: 3,
            region: Some("us-west-2".to_string()),
            auth_region: None,
            api_region: None,
            machine_id: Some("c".repeat(64)),
            email: None,
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_ids: Vec::new(),
            disabled: false,
            kiro_api_key: None,
            endpoint: None,
            max_concurrency: 0,
        };

        let json = original.to_pretty_json().unwrap();
        let parsed = KiroCredentials::from_json(&json).unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.access_token, original.access_token);
        assert_eq!(parsed.refresh_token, original.refresh_token);
        assert_eq!(parsed.priority, original.priority);
        assert_eq!(parsed.region, original.region);
        assert_eq!(parsed.machine_id, original.machine_id);
    }

    // ============ auth_region / api_region 字段测试 ============

    #[test]
    fn test_auth_region_field_parsing() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "authRegion": "eu-central-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.auth_region, Some("eu-central-1".to_string()));
        assert_eq!(creds.api_region, None);
    }

    #[test]
    fn test_api_region_field_parsing() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "apiRegion": "ap-southeast-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.api_region, Some("ap-southeast-1".to_string()));
        assert_eq!(creds.auth_region, None);
    }

    #[test]
    fn test_auth_api_region_serialization() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.auth_region = Some("eu-west-1".to_string());
        creds.api_region = Some("us-west-2".to_string());

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("authRegion"));
        assert!(json.contains("eu-west-1"));
        assert!(json.contains("apiRegion"));
        assert!(json.contains("us-west-2"));
    }

    #[test]
    fn test_auth_api_region_none_not_serialized() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.auth_region = None;
        creds.api_region = None;

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("authRegion"));
        assert!(!json.contains("apiRegion"));
    }

    #[test]
    fn test_auth_api_region_roundtrip() {
        let mut original = KiroCredentials::default();
        original.refresh_token = Some("refresh".to_string());
        original.region = Some("us-east-1".to_string());
        original.auth_region = Some("eu-west-1".to_string());
        original.api_region = Some("ap-northeast-1".to_string());

        let json = original.to_pretty_json().unwrap();
        let parsed = KiroCredentials::from_json(&json).unwrap();

        assert_eq!(parsed.region, original.region);
        assert_eq!(parsed.auth_region, original.auth_region);
        assert_eq!(parsed.api_region, original.api_region);
    }

    #[test]
    fn test_backward_compat_no_auth_api_region() {
        // 旧格式 JSON 不包含 authRegion/apiRegion，应正常解析
        let json = r#"{
            "refreshToken": "test_refresh",
            "region": "us-east-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.region, Some("us-east-1".to_string()));
        assert_eq!(creds.auth_region, None);
        assert_eq!(creds.api_region, None);
    }

    // ============ effective_auth_region / effective_api_region 优先级测试 ============

    #[test]
    fn test_effective_auth_region_credential_auth_region_highest() {
        // 凭据.auth_region > 凭据.region > config.auth_region > config.region
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());
        creds.auth_region = Some("cred-auth-region".to_string());

        assert_eq!(creds.effective_auth_region(&config), "cred-auth-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_credential_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());
        // auth_region 未设置

        assert_eq!(creds.effective_auth_region(&config), "cred-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_config_auth_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let creds = KiroCredentials::default();
        // auth_region 和 region 均未设置

        assert_eq!(creds.effective_auth_region(&config), "config-auth-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_config_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        // config.auth_region 未设置

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_auth_region(&config), "config-region");
    }

    #[test]
    fn test_effective_api_region_credential_api_region_highest() {
        // 凭据.api_region > config.api_region > config.region
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.api_region = Some("config-api-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.api_region = Some("cred-api-region".to_string());

        assert_eq!(creds.effective_api_region(&config), "cred-api-region");
    }

    #[test]
    fn test_effective_api_region_fallback_to_config_api_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.api_region = Some("config-api-region".to_string());

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_api_region(&config), "config-api-region");
    }

    #[test]
    fn test_effective_api_region_fallback_to_config_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_api_region(&config), "config-region");
    }

    #[test]
    fn test_effective_api_region_ignores_credential_region() {
        // 凭据.region 不参与 api_region 的回退链
        let mut config = Config::default();
        config.region = "config-region".to_string();

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());

        assert_eq!(creds.effective_api_region(&config), "config-region");
    }

    #[test]
    fn test_auth_and_api_region_independent() {
        // auth_region 和 api_region 互不影响
        let mut config = Config::default();
        config.region = "default".to_string();

        let mut creds = KiroCredentials::default();
        creds.auth_region = Some("auth-only".to_string());
        creds.api_region = Some("api-only".to_string());

        assert_eq!(creds.effective_auth_region(&config), "auth-only");
        assert_eq!(creds.effective_api_region(&config), "api-only");
    }

    // ============ 凭据级代理优先级测试 ============

    #[test]
    fn test_effective_proxy_credential_overrides_global() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("socks5://cred:1080".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, Some(ProxyConfig::new("socks5://cred:1080")));
    }

    #[test]
    fn test_effective_proxy_credential_with_auth() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("http://proxy:3128".to_string());
        creds.proxy_username = Some("user".to_string());
        creds.proxy_password = Some("pass".to_string());

        let result = creds.effective_proxy(Some(&global));
        let expected = ProxyConfig::new("http://proxy:3128").with_auth("user", "pass");
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_effective_proxy_direct_bypasses_global() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("direct".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, None);
    }

    #[test]
    fn test_effective_proxy_direct_case_insensitive() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("DIRECT".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, None);
    }

    #[test]
    fn test_effective_proxy_fallback_to_global() {
        let global = ProxyConfig::new("http://global:8080");
        let creds = KiroCredentials::default();

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, Some(ProxyConfig::new("http://global:8080")));
    }

    #[test]
    fn test_effective_proxy_none_when_no_proxy() {
        let creds = KiroCredentials::default();
        let result = creds.effective_proxy(None);
        assert_eq!(result, None);
    }
}
