use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    Rustls,
    NativeTls,
}

impl Default for TlsBackend {
    fn default() -> Self {
        Self::Rustls
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSegment {
    pub min: u64,
    pub max: u64,
    pub weight: u32,
}

/// 输入放大分档：真实输入 token 落在 [min, max] 时，读/写缓存分别乘对应倍率。
/// 倍率支持 1 位小数（如 1.5、2.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputScaleSegment {
    pub min: u64,
    pub max: u64,
    pub read_multiplier: f64,
    pub write_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheOptimizerConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub enabled_stream: bool,

    #[serde(default = "default_true")]
    pub enabled_non_stream: bool,

    #[serde(default = "default_true")]
    pub enabled_buffered: bool,

    #[serde(default = "default_cache_optimizer_mode")]
    pub mode: String,

    #[serde(default = "default_read_min")]
    pub read_min: u64,

    #[serde(default = "default_read_max")]
    pub read_max: u64,

    #[serde(default)]
    pub write_min: u64,

    #[serde(default = "default_write_max")]
    pub write_max: u64,

    #[serde(default = "default_weight_read_only")]
    pub weight_read_only: u32,

    #[serde(default = "default_weight_write_only")]
    pub weight_write_only: u32,

    #[serde(default = "default_weight_read_write")]
    pub weight_read_write: u32,

    #[serde(default)]
    pub weight_none: u32,

    #[serde(default)]
    pub use_segment_weights: bool,

    #[serde(default = "default_read_segments")]
    pub read_segments: Vec<CacheSegment>,

    #[serde(default = "default_write_segments")]
    pub write_segments: Vec<CacheSegment>,

    #[serde(default = "default_true")]
    pub rewrite_only_when_present: bool,

    #[serde(default = "default_true")]
    pub keep_raw_breakdown: bool,

    /// input_tokens 随机上限：>0 时把返回给下游的 input_tokens 替换为随机 [0, N]，
    /// =0 表示不替换（保持原有计算逻辑）。仅在模拟缓存开启时生效。
    #[serde(default)]
    pub input_random_max: u32,

    // ===== 探活豁免：请求输入过小时（如渠道探活）完全不改写，原样真实返回 =====
    /// 探活豁免阈值：请求输入 token ≤ 此值时豁免改写。None=不启用豁免。
    /// 判断依据是「请求进来时估算的输入」，不是上游返回值。
    #[serde(default)]
    pub probe_bypass_max_input_tokens: Option<u64>,
    /// 流式请求是否参与探活豁免
    #[serde(default)]
    pub probe_bypass_stream: bool,
    /// 非流式请求是否参与探活豁免
    #[serde(default)]
    pub probe_bypass_non_stream: bool,
    /// 缓冲流式（/cc）请求是否参与探活豁免
    #[serde(default)]
    pub probe_bypass_buffered: bool,

    // ===== 输入放大：按上游真实输入分档，对模拟改写后的读/写缓存乘倍率 =====
    /// 输入放大总开关，仅在模拟缓存开启时生效
    #[serde(default)]
    pub input_scale_enabled: bool,
    /// 放大后读缓存上限。None=不封顶。与 read_max 独立。
    #[serde(default)]
    pub input_scale_max_read: Option<u64>,
    /// 放大后写缓存上限。None=不封顶。与 write_max 独立。
    #[serde(default)]
    pub input_scale_max_write: Option<u64>,
    /// 输入放大分档（按 final_input_tokens 落档，读/写各自倍率）
    #[serde(default)]
    pub input_scale_segments: Vec<InputScaleSegment>,
}

fn default_cache_optimizer_mode() -> String {
    "weighted".to_string()
}
fn default_read_min() -> u64 {
    300
}
fn default_read_max() -> u64 {
    1200
}
fn default_write_max() -> u64 {
    500
}
fn default_weight_read_only() -> u32 {
    55
}
fn default_weight_write_only() -> u32 {
    15
}
fn default_weight_read_write() -> u32 {
    30
}
fn default_read_segments() -> Vec<CacheSegment> {
    vec![
        CacheSegment {
            min: 90000,
            max: 110000,
            weight: 35,
        },
        CacheSegment {
            min: 110001,
            max: 130000,
            weight: 45,
        },
        CacheSegment {
            min: 130001,
            max: 145000,
            weight: 20,
        },
    ]
}
fn default_write_segments() -> Vec<CacheSegment> {
    vec![
        CacheSegment {
            min: 20,
            max: 200,
            weight: 55,
        },
        CacheSegment {
            min: 201,
            max: 800,
            weight: 35,
        },
        CacheSegment {
            min: 801,
            max: 3000,
            weight: 10,
        },
    ]
}

impl Default for CacheOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            enabled_stream: true,
            enabled_non_stream: true,
            enabled_buffered: true,
            mode: default_cache_optimizer_mode(),
            read_min: default_read_min(),
            read_max: default_read_max(),
            write_min: 0,
            write_max: default_write_max(),
            weight_read_only: default_weight_read_only(),
            weight_write_only: default_weight_write_only(),
            weight_read_write: default_weight_read_write(),
            weight_none: 0,
            use_segment_weights: false,
            read_segments: default_read_segments(),
            write_segments: default_write_segments(),
            rewrite_only_when_present: true,
            keep_raw_breakdown: true,
            input_random_max: 0,
            probe_bypass_max_input_tokens: None,
            probe_bypass_stream: false,
            probe_bypass_non_stream: false,
            probe_bypass_buffered: false,
            input_scale_enabled: false,
            input_scale_max_read: None,
            input_scale_max_write: None,
            input_scale_segments: Vec::new(),
        }
    }
}

/// 单条模型映射
///
/// - `alias`：请求的模型（下游发来的模型名）
/// - `target`：实际模型（转发给上游 Kiro 的模型名）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMapping {
    pub alias: String,
    pub target: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 模型映射配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingConfig {
    #[serde(default)]
    pub enabled: bool,

    /// 模型列表是否用 alias 替换已映射的 target 显示
    #[serde(default = "default_true")]
    pub hide_mapped_targets: bool,

    #[serde(default)]
    pub mappings: Vec<ModelMapping>,
}

impl Default for ModelMappingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hide_mapped_targets: true,
            mappings: Vec::new(),
        }
    }
}

impl ModelMappingConfig {
    /// 解析下游请求的模型名 → 上游实际模型名。
    /// 未启用或无命中则原样返回。大小写不敏感。
    pub fn resolve_alias(&self, requested: &str) -> String {
        if !self.enabled {
            return requested.to_string();
        }
        let needle = requested.trim().to_lowercase();
        self.mappings
            .iter()
            .find(|m| m.enabled && m.alias.trim().to_lowercase() == needle)
            .map(|m| m.target.clone())
            .unwrap_or_else(|| requested.to_string())
    }

    /// 反查：上游实际模型名 → 下游展示用的 alias。
    /// 未启用、未开启隐藏或无命中则返回 None。大小写不敏感。
    pub fn alias_for_target(&self, target: &str) -> Option<String> {
        if !self.enabled || !self.hide_mapped_targets {
            return None;
        }
        let needle = target.trim().to_lowercase();
        self.mappings
            .iter()
            .find(|m| m.enabled && m.target.trim().to_lowercase() == needle)
            .map(|m| m.alias.clone())
    }
}

/// 代理池中的单个代理配置。
///
/// `protocol` 只允许 http / https / socks5；运行时会用 id 被凭据引用。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProfile {
    pub id: u64,

    #[serde(default)]
    pub name: String,

    pub protocol: String,
    pub host: String,
    pub port: u16,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// 是否开启非流式响应的 thinking 块提取（默认 true）
    ///
    /// 启用后，非流式响应中的 `<thinking>...</thinking>` 标签会被解析为
    /// 独立的 `{"type": "thinking", ...}` 内容块,与流式响应行为一致。
    #[serde(default = "default_extract_thinking")]
    pub extract_thinking: bool,

    /// Prompt Cache TTL（秒），默认 300 秒
    #[serde(default = "default_prompt_cache_ttl_seconds")]
    pub prompt_cache_ttl_seconds: u64,

    /// 是否启用本地 Prompt Cache usage 记账，默认 true
    #[serde(default = "default_true")]
    pub prompt_cache_accounting_enabled: bool,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用，默认 "ide"）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 端点特定的配置
    ///
    /// 键为端点名（如 "ide" / "cli"），值为该端点自由定义的参数对象。
    /// 未在此表出现的端点沿用实现内置默认值。
    #[serde(default)]
    pub endpoints: HashMap<String, serde_json::Value>,

    /// 模拟缓存优化器配置
    #[serde(default)]
    pub cache_optimizer: CacheOptimizerConfig,

    /// 模型映射配置
    #[serde(default)]
    pub model_mapping: ModelMappingConfig,

    /// Admin 管理的代理池配置
    #[serde(default)]
    pub proxies: Vec<ProxyProfile>,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "0.12.155".to_string()
}

fn default_system_version() -> String {
    const SYSTEM_VERSIONS: &[&str] = &["darwin#24.6.0", "win32#10.0.22631"];
    SYSTEM_VERSIONS[fastrand::usize(..SYSTEM_VERSIONS.len())].to_string()
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_extract_thinking() -> bool {
    true
}

fn default_prompt_cache_ttl_seconds() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

fn default_endpoint() -> String {
    crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            load_balancing_mode: default_load_balancing_mode(),
            extract_thinking: default_extract_thinking(),
            prompt_cache_ttl_seconds: default_prompt_cache_ttl_seconds(),
            prompt_cache_accounting_enabled: default_true(),
            default_endpoint: default_endpoint(),
            endpoints: HashMap::new(),
            cache_optimizer: CacheOptimizerConfig::default(),
            model_mapping: ModelMappingConfig::default(),
            proxies: Vec::new(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在，返回默认配置
            let mut config = Self::default();
            config.config_path = Some(path.to_path_buf());
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// 获取配置文件路径（如果有）
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// 将当前配置写回原始配置文件
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("配置文件路径未知，无法保存配置"))?;

        let content = serde_json::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(path, content)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}
