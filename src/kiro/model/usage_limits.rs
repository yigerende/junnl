//! 使用额度查询数据模型
//!
//! 包含 getUsageLimits API 的响应类型定义

use serde::Deserialize;

/// 使用额度查询响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    pub next_date_reset: Option<f64>,

    /// 订阅信息
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,

    /// 使用量明细列表
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,

    /// 超额配置（账号级别的超额计费开关状态）
    #[serde(default)]
    pub overage_configuration: Option<OverageConfiguration>,
}

/// 超额配置
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    /// 超额状态：ENABLED / DISABLED
    #[serde(default)]
    pub overage_status: Option<String>,

    /// 超额能力：账号是否有资格开启超额
    #[serde(default)]
    pub overage_capability: Option<String>,
}

/// 订阅信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    /// 订阅标题 (KIRO PRO+ / KIRO FREE 等)
    #[serde(default)]
    pub subscription_title: Option<String>,
}

/// 使用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct UsageBreakdown {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 奖励额度列表
    #[serde(default)]
    pub bonuses: Vec<Bonus>,

    /// 免费试用信息
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    pub next_date_reset: Option<f64>,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,

    /// 超额上限（基础额度之外可继续使用的额度）
    #[serde(default)]
    pub overage_cap: i64,

    /// 超额上限（精确值）
    #[serde(default)]
    pub overage_cap_with_precision: f64,
}

/// 奖励额度
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: f64,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: f64,

    /// 状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub status: Option<String>,
}

impl Bonus {
    /// 检查 bonus 是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

/// 免费试用信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct FreeTrialInfo {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 免费试用过期时间 (Unix 时间戳)
    #[serde(default)]
    pub free_trial_expiry: Option<f64>,

    /// 免费试用状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub free_trial_status: Option<String>,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

// ============ 便捷方法实现 ============

impl FreeTrialInfo {
    /// 检查免费试用是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl UsageLimitsResponse {
    /// 获取订阅标题
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    /// 获取第一个使用量明细
    fn primary_breakdown(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 获取总使用限额（精确值）
    ///
    /// 累加基础额度、激活的免费试用额度和激活的奖励额度
    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.usage_limit_with_precision;

        // 累加激活的 free trial 额度
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }

        // 累加激活的 bonus 额度
        for bonus in &breakdown.bonuses {
            if bonus.is_active() {
                total += bonus.usage_limit;
            }
        }

        total
    }

    /// 获取总当前使用量（精确值）
    ///
    /// 累加基础使用量、激活的免费试用使用量和激活的奖励使用量
    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.current_usage_with_precision;

        // 累加激活的 free trial 使用量
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }

        // 累加激活的 bonus 使用量
        for bonus in &breakdown.bonuses {
            if bonus.is_active() {
                total += bonus.current_usage;
            }
        }

        total
    }

    /// 超额状态：ENABLED / DISABLED / UNKNOWN（上游未返回时）
    pub fn overage_status(&self) -> &str {
        self.overage_configuration
            .as_ref()
            .and_then(|c| c.overage_status.as_deref())
            .unwrap_or("UNKNOWN")
    }

    /// 是否已开启超额
    pub fn overage_enabled(&self) -> bool {
        self.overage_status() == "ENABLED"
    }

    /// 超额能力（账号是否有资格开启超额）
    pub fn overage_capability(&self) -> Option<&str> {
        self.overage_configuration
            .as_ref()
            .and_then(|c| c.overage_capability.as_deref())
    }

    /// 基础额度（精确值，不含超额/试用/奖励）
    pub fn base_limit(&self) -> f64 {
        self.primary_breakdown()
            .map(|b| b.usage_limit_with_precision)
            .unwrap_or(0.0)
    }

    /// 超额上限（精确值）
    pub fn overage_cap(&self) -> f64 {
        self.primary_breakdown()
            .map(|b| {
                if b.overage_cap_with_precision > 0.0 {
                    b.overage_cap_with_precision
                } else {
                    b.overage_cap as f64
                }
            })
            .unwrap_or(0.0)
    }

    /// 基础当前使用量（精确值，不含试用/奖励）
    pub fn base_usage(&self) -> f64 {
        self.primary_breakdown()
            .map(|b| b.current_usage_with_precision)
            .unwrap_or(0.0)
    }

    /// 总额度 = 基础额度 + 超额上限（仅在超额开启时计入超额）
    pub fn total_limit_with_overage(&self) -> f64 {
        if self.overage_enabled() {
            self.base_limit() + self.overage_cap()
        } else {
            self.base_limit()
        }
    }

    /// 已用超额 = max(0, 基础使用量 - 基础额度)，上限为超额上限
    pub fn overage_usage(&self) -> f64 {
        let over = (self.base_usage() - self.base_limit()).max(0.0);
        let cap = self.overage_cap();
        if cap > 0.0 { over.min(cap) } else { over }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 贴近截图：基础 1000 + 超额 10000，已用 14.42，超额开启。
    fn parse(json: &str) -> UsageLimitsResponse {
        serde_json::from_str(json).expect("parse usage limits")
    }

    #[test]
    fn parses_overage_enabled_and_computes_totals() {
        let r = parse(
            r#"{
                "overageConfiguration": { "overageStatus": "ENABLED", "overageCapability": "CAPABLE" },
                "usageBreakdownList": [{
                    "currentUsage": 14,
                    "currentUsageWithPrecision": 14.42,
                    "usageLimit": 1000,
                    "usageLimitWithPrecision": 1000.0,
                    "overageCap": 10000,
                    "overageCapWithPrecision": 10000.0
                }]
            }"#,
        );
        assert_eq!(r.overage_status(), "ENABLED");
        assert!(r.overage_enabled());
        assert_eq!(r.overage_capability(), Some("CAPABLE"));
        assert_eq!(r.base_limit(), 1000.0);
        assert_eq!(r.overage_cap(), 10000.0);
        // 总额度 = 基础 + 超额 = 11000
        assert_eq!(r.total_limit_with_overage(), 11000.0);
        // 已用 14.42 < 基础 1000，超额用量为 0
        assert_eq!(r.overage_usage(), 0.0);
    }

    #[test]
    fn overage_usage_counts_beyond_base() {
        let r = parse(
            r#"{
                "overageConfiguration": { "overageStatus": "ENABLED" },
                "usageBreakdownList": [{
                    "currentUsageWithPrecision": 1250.0,
                    "usageLimitWithPrecision": 1000.0,
                    "overageCapWithPrecision": 10000.0
                }]
            }"#,
        );
        // 超基础 250
        assert_eq!(r.overage_usage(), 250.0);
    }

    #[test]
    fn overage_usage_capped_at_overage_cap() {
        let r = parse(
            r#"{
                "overageConfiguration": { "overageStatus": "ENABLED" },
                "usageBreakdownList": [{
                    "currentUsageWithPrecision": 99999.0,
                    "usageLimitWithPrecision": 1000.0,
                    "overageCapWithPrecision": 10000.0
                }]
            }"#,
        );
        // 超出部分 98999，但上限是 10000
        assert_eq!(r.overage_usage(), 10000.0);
    }

    #[test]
    fn overage_disabled_excludes_overage_from_total() {
        let r = parse(
            r#"{
                "overageConfiguration": { "overageStatus": "DISABLED" },
                "usageBreakdownList": [{
                    "usageLimitWithPrecision": 1000.0,
                    "overageCapWithPrecision": 10000.0
                }]
            }"#,
        );
        assert!(!r.overage_enabled());
        // 关闭时总额度 = 基础，不含超额
        assert_eq!(r.total_limit_with_overage(), 1000.0);
    }

    #[test]
    fn missing_overage_config_is_unknown() {
        let r = parse(r#"{ "usageBreakdownList": [{ "usageLimitWithPrecision": 500.0 }] }"#);
        assert_eq!(r.overage_status(), "UNKNOWN");
        assert!(!r.overage_enabled());
        assert_eq!(r.overage_cap(), 0.0);
        assert_eq!(r.total_limit_with_overage(), 500.0);
    }
}
