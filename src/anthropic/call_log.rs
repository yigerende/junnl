//! 调用日志模块
//!
//! 内存环形缓冲，记录每次请求的下游模型 → 上游模型映射情况。
//! 超过容量上限自动丢弃最早的条目。重启后清空。

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 默认保留条数
pub const DEFAULT_CAPACITY: usize = 1000;
/// 容量上限（防止前端设置过大撑爆内存）
const MAX_CAPACITY: usize = 100_000;

/// 单条调用日志
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLogEntry {
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
}

/// 调用日志环形缓冲
#[derive(Clone)]
pub struct CallLog {
    inner: Arc<RwLock<CallLogInner>>,
}

struct CallLogInner {
    entries: VecDeque<CallLogEntry>,
    capacity: usize,
}

impl CallLog {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(CallLogInner {
                entries: VecDeque::with_capacity(capacity.min(1024)),
                capacity,
            })),
        }
    }

    /// 记录一条调用日志，超过容量时丢弃最早的
    pub fn record(&self, entry: CallLogEntry) {
        let mut inner = self.inner.write();
        let cap = inner.capacity;
        inner.entries.push_back(entry);
        while inner.entries.len() > cap {
            inner.entries.pop_front();
        }
    }

    /// 获取最近的日志（按时间倒序，最新在前），最多 limit 条
    pub fn recent(&self, limit: usize) -> Vec<CallLogEntry> {
        let inner = self.inner.read();
        inner
            .entries
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
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
