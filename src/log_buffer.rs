//! 内存日志缓冲 + 实时订阅。
//!
//! 用一个自定义 `tracing` Layer 抓取所有日志事件，转成结构化 [`LogRecord`]：
//!   1. 存入固定容量的环形缓冲（供前端「拉取最近 N 条」）；
//!   2. 通过 broadcast 通道实时广播（供前端 SSE「实时滚动」）。
//!
//! 设计原则：
//! - 全程内存操作 + 非阻塞广播，绝不阻塞请求主链路。
//! - 广播通道满时丢弃最旧消息（lossy），不反压。
//! - 通过全局 `OnceLock` 暴露，避免改动现有 AppState / AdminState 结构。

use std::collections::VecDeque;
use std::sync::OnceLock;

use parking_lot::RwLock;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// 环形缓冲容量（与调用日志 5000 对齐，约覆盖 0.5-1 小时）。
const BUFFER_CAPACITY: usize = 5000;
/// broadcast 通道缓冲；满时最旧消息被丢弃（订阅者会收到 Lagged，不影响发送方）。
const BROADCAST_CAPACITY: usize = 1024;

/// 单条结构化日志记录。字段与落盘 JSON 对齐，便于前端统一展示。
#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    /// 自增序号，前端用于去重 / 排序 / 增量拉取。
    pub seq: u64,
    /// Unix 毫秒时间戳。
    pub ts: i64,
    /// 日志级别：ERROR / WARN / INFO / DEBUG / TRACE。
    pub level: String,
    /// 目标模块路径（target）。
    pub target: String,
    /// 日志消息文本。
    pub message: String,
    /// 该事件所属请求的 request_id（若在请求 span 内）。
    pub request_id: Option<String>,
    /// 其余结构化字段（event / reason / 耗时等），扁平为字符串映射。
    pub fields: std::collections::BTreeMap<String, String>,
}

struct Inner {
    entries: VecDeque<LogRecord>,
    seq: u64,
}

/// 日志缓冲 + 广播中心。
pub struct LogBuffer {
    inner: RwLock<Inner>,
    sender: broadcast::Sender<LogRecord>,
}

impl LogBuffer {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: RwLock::new(Inner {
                entries: VecDeque::with_capacity(BUFFER_CAPACITY.min(1024)),
                seq: 0,
            }),
            sender,
        }
    }

    /// 追加一条记录：写入环形缓冲并广播。返回分配的 seq。
    fn push(&self, mut record: LogRecord) {
        let mut inner = self.inner.write();
        inner.seq += 1;
        record.seq = inner.seq;
        inner.entries.push_back(record.clone());
        while inner.entries.len() > BUFFER_CAPACITY {
            inner.entries.pop_front();
        }
        drop(inner);
        // 没有订阅者时 send 返回 Err，忽略即可。
        let _ = self.sender.send(record);
    }

    /// 取最近 `limit` 条（时间正序，最旧在前）。
    pub fn recent(&self, limit: usize) -> Vec<LogRecord> {
        let inner = self.inner.read();
        let len = inner.entries.len();
        let start = len.saturating_sub(limit);
        inner.entries.iter().skip(start).cloned().collect()
    }

    /// 订阅实时日志广播。
    pub fn subscribe(&self) -> broadcast::Receiver<LogRecord> {
        self.sender.subscribe()
    }
}

/// 全局日志缓冲单例。
static LOG_BUFFER: OnceLock<LogBuffer> = OnceLock::new();

/// 获取全局日志缓冲（首次访问时初始化）。
pub fn global() -> &'static LogBuffer {
    LOG_BUFFER.get_or_init(LogBuffer::new)
}

/// 字段访问器：把事件的所有字段收集成字符串映射，单独抽出 message。
#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: std::collections::BTreeMap<String, String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{:?}", value);
        if field.name() == "message" {
            self.message = rendered;
        } else {
            self.fields.insert(field.name().to_string(), rendered);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

/// 自定义 tracing Layer：抓取事件 → 写入全局日志缓冲。
///
/// 仅消费事件，不影响其它 Layer（控制台、文件）的输出。
pub struct LogBufferLayer;

impl<S> Layer<S> for LogBufferLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        // 从当前 span 作用域里查找 request_id（由 #[instrument(fields(request_id))] 注入）。
        // event_scope 对显式无 parent 的事件可能返回 None，回退到 current span 链。
        let mut request_id = None;
        let scope = ctx
            .event_scope(event)
            .or_else(|| ctx.lookup_current().and_then(|s| s.scope().into()));
        if let Some(scope) = scope {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<RequestIdExt>() {
                    request_id = Some(fields.0.clone());
                }
            }
        }

        let meta = event.metadata();
        let record = LogRecord {
            seq: 0, // push 时分配
            ts: chrono::Utc::now().timestamp_millis(),
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            message: std::mem::take(&mut visitor.message),
            request_id,
            fields: visitor.fields,
        };
        global().push(record);
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        // 在 span 创建时把 request_id 字段值缓存到 span extensions，
        // 供该 span 内的事件查找（避免每条事件重复解析 span 字段）。
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        if let Some(rid) = visitor.fields.get("request_id") {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(RequestIdExt(rid.clone()));
            }
        }
    }
}

/// 存入 span extensions 的 request_id 缓存。
struct RequestIdExt(String);
