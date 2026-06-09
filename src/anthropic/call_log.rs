//! 调用日志模块
//!
//! 内存环形缓冲，记录每次请求的下游模型 → 上游模型映射情况。
//! 超过容量上限自动丢弃最早的条目。重启后清空。

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 默认保留条数
///
/// 5000 条约占 2-5 MB 内存，覆盖约 0.5-1 小时调用历史。下游多人打并发时，
/// 旧值 1000 条几分钟就被刷没，截图过来往往看不到出错请求最初的入口日志。
pub const DEFAULT_CAPACITY: usize = 5000;
/// 容量上限（防止前端设置过大撑爆内存）
const MAX_CAPACITY: usize = 100_000;

/// 单条调用日志
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLogEntry {
    /// 条目自增 ID（进程内唯一，用于请求完成后回填首token/总耗时）
    #[serde(default)]
    pub id: u64,
    /// 请求时间（Unix 毫秒）
    pub timestamp_ms: i64,
    /// 下游请求的模型名（原始）
    pub downstream_model: String,
    /// 实际调用上游的模型名
    pub upstream_model: String,
    /// 是否流式
    pub stream: bool,
    /// 端点（/v1 或 /cc/v1）
    pub endpoint: String,
    /// 是否命中模型映射（下游 != 上游）
    pub mapped: bool,
    /// 来源 IP（X-Forwarded-For / X-Real-IP，反代后的真实来源）
    #[serde(default)]
    pub client_ip: Option<String>,
    /// 来源域名（Host 头，访问用的中转域名）
    #[serde(default)]
    pub client_host: Option<String>,
    /// 实际使用的凭据 ID
    #[serde(default)]
    pub credential_id: Option<u64>,
    /// 该凭据累计请求次数（含本次、含失败）
    #[serde(default)]
    pub credential_request_count: Option<u64>,
    /// 会话 ID（conversationId，用于核对同会话是否进同凭据）
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// conversationId 来源（metadata / x-session-id / ... / random）
    #[serde(default)]
    pub conversation_id_source: Option<String>,
    /// 是否命中会话亲和（balanced 模式复用了已绑定凭据）
    #[serde(default)]
    pub session_affinity_hit: bool,
    /// 本次请求是否成功（true=2xx 正常完成）
    #[serde(default)]
    pub success: bool,
    /// 首 token 耗时（毫秒）：请求进入到上游首字节到达的间隔。
    /// 仅流式请求可观测，非流式为上游响应就绪耗时；未知时为 None。
    #[serde(default)]
    pub first_token_ms: Option<u64>,
    /// 总耗时（毫秒）：请求进入到响应流读完（或非流式响应构建完毕）的间隔。
    /// 在请求完成后回填；未完成/已被淘汰时为 None。
    #[serde(default)]
    pub total_duration_ms: Option<u64>,
}

/// 调用日志环形缓冲
#[derive(Clone)]
pub struct CallLog {
    inner: Arc<RwLock<CallLogInner>>,
}

struct CallLogInner {
    entries: VecDeque<CallLogEntry>,
    capacity: usize,
    /// 下一个分配的条目 ID（自增，进程内唯一）
    next_id: u64,
}

impl CallLog {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(CallLogInner {
                entries: VecDeque::with_capacity(capacity.min(1024)),
                capacity,
                next_id: 1,
            })),
        }
    }

    /// 记录一条调用日志，超过容量时丢弃最早的。
    ///
    /// 返回分配给该条目的 ID，供请求完成后通过 [`update_timing`](Self::update_timing)
    /// 回填首token/总耗时。调用方传入的 `entry.id` 会被忽略并覆盖。
    pub fn record(&self, mut entry: CallLogEntry) -> u64 {
        let mut inner = self.inner.write();
        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1);
        entry.id = id;
        let cap = inner.capacity;
        inner.entries.push_back(entry);
        while inner.entries.len() > cap {
            inner.entries.pop_front();
        }
        id
    }

    /// 按 ID 回填首token/总耗时。仅 `Some(_)` 的字段会被写入，`None` 保持原值。
    ///
    /// 条目可能已被环形缓冲淘汰，此时静默忽略。最新条目在队尾，故从尾部反向查找。
    pub fn update_timing(
        &self,
        id: u64,
        first_token_ms: Option<u64>,
        total_duration_ms: Option<u64>,
    ) {
        let mut inner = self.inner.write();
        if let Some(entry) = inner.entries.iter_mut().rev().find(|e| e.id == id) {
            if first_token_ms.is_some() {
                entry.first_token_ms = first_token_ms;
            }
            if total_duration_ms.is_some() {
                entry.total_duration_ms = total_duration_ms;
            }
        }
    }

    /// 获取最近的日志（按时间倒序，最新在前），最多 limit 条
    pub fn recent(&self, limit: usize) -> Vec<CallLogEntry> {
        let inner = self.inner.read();
        inner.entries.iter().rev().take(limit).cloned().collect()
    }

    /// 清空所有日志
    pub fn clear(&self) {
        self.inner.write().entries.clear();
    }

    /// 获取当前容量上限
    pub fn capacity(&self) -> usize {
        self.inner.read().capacity
    }

    /// 设置容量上限，立即裁剪超出的旧条目
    pub fn set_capacity(&self, capacity: usize) -> usize {
        let capacity = capacity.clamp(1, MAX_CAPACITY);
        let mut inner = self.inner.write();
        inner.capacity = capacity;
        while inner.entries.len() > capacity {
            inner.entries.pop_front();
        }
        capacity
    }
}

impl Default for CallLog {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> CallLogEntry {
        CallLogEntry {
            id: 0,
            timestamp_ms: 0,
            downstream_model: "claude".into(),
            upstream_model: "kiro".into(),
            stream: true,
            endpoint: "/v1".into(),
            mapped: false,
            client_ip: None,
            client_host: None,
            credential_id: Some(1),
            credential_request_count: None,
            conversation_id: None,
            conversation_id_source: None,
            session_affinity_hit: false,
            success: true,
            first_token_ms: None,
            total_duration_ms: None,
        }
    }

    #[test]
    fn record_assigns_incrementing_ids() {
        let log = CallLog::new(10);
        let id1 = log.record(sample_entry());
        let id2 = log.record(sample_entry());
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        // record 覆盖调用方传入的 id
        let recent = log.recent(10);
        assert_eq!(recent[0].id, 2); // 最新在前
        assert_eq!(recent[1].id, 1);
    }

    #[test]
    fn update_timing_backfills_by_id() {
        let log = CallLog::new(10);
        let id = log.record(sample_entry());
        log.update_timing(id, Some(120), None);
        log.update_timing(id, None, Some(3400));

        let entry = &log.recent(1)[0];
        assert_eq!(entry.first_token_ms, Some(120));
        assert_eq!(entry.total_duration_ms, Some(3400));
    }

    #[test]
    fn update_timing_none_preserves_existing() {
        let log = CallLog::new(10);
        let id = log.record(sample_entry());
        log.update_timing(id, Some(100), Some(200));
        // 再次以 None 调用不应清空已写入的值
        log.update_timing(id, None, None);

        let entry = &log.recent(1)[0];
        assert_eq!(entry.first_token_ms, Some(100));
        assert_eq!(entry.total_duration_ms, Some(200));
    }

    #[test]
    fn update_timing_ignores_evicted_entry() {
        let log = CallLog::new(2);
        let id1 = log.record(sample_entry()); // 将被淘汰
        log.record(sample_entry());
        log.record(sample_entry()); // 触发淘汰 id1

        // 对已淘汰条目回填应静默忽略，不 panic
        log.update_timing(id1, Some(50), Some(60));
        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().all(|e| e.id != id1));
    }

    #[test]
    fn update_timing_targets_correct_entry() {
        let log = CallLog::new(10);
        let id1 = log.record(sample_entry());
        let id2 = log.record(sample_entry());
        log.update_timing(id1, Some(11), Some(111));
        log.update_timing(id2, Some(22), Some(222));

        let recent = log.recent(10);
        let e2 = recent.iter().find(|e| e.id == id2).unwrap();
        let e1 = recent.iter().find(|e| e.id == id1).unwrap();
        assert_eq!(e1.first_token_ms, Some(11));
        assert_eq!(e1.total_duration_ms, Some(111));
        assert_eq!(e2.first_token_ms, Some(22));
        assert_eq!(e2.total_duration_ms, Some(222));
    }
}
