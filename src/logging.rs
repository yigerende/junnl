//! 日志初始化与落盘。
//!
//! 在原有「输出到控制台」的基础上，**新增**一个按天滚动的 JSON 文件层，
//! 用非阻塞 writer 落盘到 `logs/app-YYYY-MM-DD.log`，便于事后排查。
//!
//! 设计原则（务必保持）：
//! - 文件写入走 `tracing_appender::non_blocking`，请求线程只入内存 channel，
//!   真正的磁盘 I/O 在后台线程完成，**绝不阻塞请求主链路**。
//! - 控制台层保留原样，docker logs / pm2 仍可正常查看。
//! - 仅初始化日志基础设施，不触碰任何业务逻辑。

use std::path::PathBuf;
use std::time::Duration;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// 默认日志留存天数。
const DEFAULT_RETAIN_DAYS: i64 = 14;

/// 解析日志目录：优先 `JUNNL_LOG_DIR` 环境变量，否则用工作目录下的 `logs/`。
fn log_dir() -> PathBuf {
    match std::env::var("JUNNL_LOG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("logs"),
    }
}

/// 解析留存天数：`JUNNL_LOG_RETAIN_DAYS`，非法值回退默认。
fn retain_days() -> i64 {
    std::env::var("JUNNL_LOG_RETAIN_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_RETAIN_DAYS)
}

/// 初始化全局日志。
///
/// 返回 `WorkerGuard`：**必须在 `main` 里持有到进程结束**，否则非阻塞写线程会
/// 在 guard 释放时退出，导致尚未刷盘的日志丢失。
///
/// 若文件层初始化失败（如目录不可写），自动降级为「仅控制台」，不会 panic，
/// 保证服务一定能起来。
#[must_use = "持有返回的 WorkerGuard 直到进程退出，否则文件日志会丢失"]
pub fn init() -> Option<WorkerGuard> {
    // 控制台过滤器（沿用 RUST_LOG，默认 info）。
    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let dir = log_dir();
    let file_ready = std::fs::create_dir_all(&dir).is_ok();

    if !file_ready {
        // 目录建不出来：控制台 + 内存缓冲（供 admin 面板），仅丢失落盘。
        let buffer_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        tracing_subscriber::registry()
            .with(fmt::layer().with_target(true).with_filter(console_filter))
            .with(crate::log_buffer::LogBufferLayer.with_filter(buffer_filter))
            .init();
        eprintln!("[logging] 无法创建日志目录 {:?}，已降级为仅控制台输出", dir);
        return None;
    }

    // 文件层：按天滚动的 JSON，非阻塞写。
    let file_appender = tracing_appender::rolling::daily(&dir, "app");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // 文件层独立过滤器（与控制台同源，但需独立实例）。
    let file_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let console_layer = fmt::layer()
        .with_target(true)
        .with_filter(console_filter);

    let file_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(non_blocking)
        .with_filter(file_filter);

    // 内存缓冲层：抓取日志供 admin 实时面板（拉取 + SSE）。
    let buffer_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let buffer_layer = crate::log_buffer::LogBufferLayer.with_filter(buffer_filter);

    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .with(buffer_layer)
        .init();

    // 启动时清理一次旧日志，并起一个每天清理的后台任务。
    spawn_cleanup_task(dir, retain_days());

    Some(guard)
}

/// 删除日志目录下超过留存期的 `app.<日期>` 文件。
///
/// `tracing_appender::rolling::daily(dir, "app")` 生成的文件名形如
/// `app.2026-06-07`。这里按文件名尾部的日期判断是否过期。
fn cleanup_once(dir: &std::path::Path, retain: i64) {
    // 文件名日期由 `rolling::daily` 按 UTC 生成（如 app.2026-06-06），
    // 这里的 cutoff 也必须用 UTC，否则跨时区会多删/少删一天。
    let cutoff = chrono::Utc::now().date_naive() - chrono::Duration::days(retain);
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // 仅处理我们自己生成的滚动文件：以 "app." 开头。
        let Some(date_part) = name.strip_prefix("app.") else {
            continue;
        };
        let Ok(file_date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") else {
            continue;
        };
        if file_date < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 启动时清理一次，并起后台任务每 24h 清理一次。后台任务失败不影响主流程。
fn spawn_cleanup_task(dir: PathBuf, retain: i64) {
    cleanup_once(&dir, retain);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        // 第一次 tick 立即返回，跳过（启动时已清理过）。
        ticker.tick().await;
        loop {
            ticker.tick().await;
            cleanup_once(&dir, retain);
        }
    });
}
