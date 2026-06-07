//! 日志初始化与落盘。
//!
//! 在原有「输出到控制台」的基础上，**新增**一个按天滚动的 JSON 文件层，
//! 用非阻塞 writer 落盘到 `logs/app-YYYY-MM-DD.log`，便于事后排查。
//!
//! 设计原则（务必保持）：
//! - 文件写入走 `tracing_appender::non_blocking`，请求线程只入内存 channel，
//!   真正的磁盘 I/O 在后台线程完成，**绝不阻塞请求主链路**。
//! - 控制台层保留原样，docker logs / pm2 仍可正常查看。
//! - **文件层默认只落 WARN+**（INFO 噪音量大、会撑爆磁盘），实时 INFO 看控制台
//!   或 admin「运行日志」页；排查时用 `JUNNL_LOG_FILE_LEVEL=info` 临时落全量。
//! - 仅初始化日志基础设施，不触碰任何业务逻辑。

use std::path::PathBuf;
use std::time::Duration;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// 默认日志留存天数。
const DEFAULT_RETAIN_DAYS: i64 = 3;

/// 解析日志目录：优先 `JUNNL_LOG_DIR` 环境变量，否则用工作目录下的 `logs/`。
///
/// 注意：默认 `logs/` 是**相对当前工作目录(cwd)**的相对路径，cwd 取决于进程
/// 启动方式（脚本/systemd/docker），并非二进制所在目录。排查「找不到日志文件」
/// 时优先用 [`log_dir_absolute`] 看真实落盘路径。
pub fn log_dir() -> PathBuf {
    match std::env::var("JUNNL_LOG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("logs"),
    }
}

/// 返回日志目录的绝对路径（供前端展示「日志到底落在哪」）。
///
/// 若目录已存在则用 `canonicalize` 解析真实路径；否则回退为 `cwd + 相对路径`，
/// 保证即使目录尚未创建也能给出一个可读的绝对路径。
///
/// Windows 上 `canonicalize` 会返回 `\\?\` 扩展长度前缀，这里去掉，让展示更友好。
pub fn log_dir_absolute() -> PathBuf {
    let resolved = std::fs::canonicalize(log_dir()).unwrap_or_else(|_| {
        let dir = log_dir();
        std::env::current_dir()
            .map(|cwd| cwd.join(&dir))
            .unwrap_or(dir)
    });
    strip_unc_prefix(resolved)
}

/// 去掉 Windows `canonicalize` 产生的 `\\?\` 扩展长度前缀（非 Windows 原样返回）。
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

/// 校验日志文件名是否为合法的滚动日志文件名（`app.YYYY-MM-DD`）。
///
/// 仅允许 `app.` 前缀 + 合法日期，杜绝 `..`、路径分隔符等穿越攻击。
pub fn is_valid_log_filename(name: &str) -> bool {
    let Some(date_part) = name.strip_prefix("app.") else {
        return false;
    };
    chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").is_ok()
}

/// 当天（UTC）滚动日志文件名，与 `rolling::daily` 的命名一致。
pub fn today_log_filename() -> String {
    format!("app.{}", chrono::Utc::now().date_naive())
}

/// 解析留存天数：`JUNNL_LOG_RETAIN_DAYS`，非法值回退默认。
fn retain_days() -> i64 {
    std::env::var("JUNNL_LOG_RETAIN_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_RETAIN_DAYS)
}

/// 文件层日志级别过滤器。
///
/// 落盘文件**默认只记录 WARN 及以上**（warn/error），把 INFO 噪音挡在磁盘外——
/// 实时 INFO 仍可在控制台和 admin「运行日志」页查看，无需长期落盘。
///
/// 排查时可用 `JUNNL_LOG_FILE_LEVEL=info`（或 debug）临时让文件层也落全量；
/// 若设置了 `RUST_LOG`，则文件层沿用 `RUST_LOG`（兼容老用法、便于一次性调全）。
fn file_filter() -> EnvFilter {
    // 显式的 JUNNL_LOG_FILE_LEVEL 优先级最高。
    if let Ok(level) = std::env::var("JUNNL_LOG_FILE_LEVEL") {
        if !level.trim().is_empty() {
            if let Ok(f) = EnvFilter::try_new(level.trim()) {
                return f;
            }
        }
    }
    // 其次沿用 RUST_LOG（若用户显式设了，说明想统一调级别）。
    if let Ok(f) = EnvFilter::try_from_default_env() {
        return f;
    }
    // 默认：文件只落 WARN 及以上。
    EnvFilter::new("warn")
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

    // 文件层独立过滤器：默认只落 WARN+，避免 INFO 噪音撑爆磁盘。
    let file_filter = file_filter();

    let console_layer = fmt::layer().with_target(true).with_filter(console_filter);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_log_filenames_accepted() {
        assert!(is_valid_log_filename("app.2026-06-07"));
        assert!(is_valid_log_filename("app.2025-01-01"));
    }

    #[test]
    fn invalid_log_filenames_rejected() {
        // 缺前缀 / 非法日期 / 路径穿越 / 分隔符一律拒绝。
        assert!(!is_valid_log_filename("app.txt"));
        assert!(!is_valid_log_filename("app."));
        assert!(!is_valid_log_filename("2026-06-07"));
        assert!(!is_valid_log_filename("app.2026-13-99"));
        assert!(!is_valid_log_filename("app.../../etc/passwd"));
        assert!(!is_valid_log_filename("app.2026-06-07/../secret"));
        assert!(!is_valid_log_filename("../app.2026-06-07"));
        assert!(!is_valid_log_filename(""));
    }

    #[test]
    fn today_filename_matches_rolling_format() {
        let name = today_log_filename();
        assert!(name.starts_with("app."));
        assert!(is_valid_log_filename(&name));
    }

    /// 核心回归：文件层默认过滤器（warn）必须**放行 WARN/ERROR、拦截 INFO/DEBUG**。
    ///
    /// 用一个内存 writer 接同样的 `EnvFilter::new("warn")`，发四条不同级别的日志，
    /// 断言落盘内容里有 WARN/ERROR、没有 INFO/DEBUG。直接验证「出 warn/error 时
    /// 一定会进日志文件」这一关键诉求，防止以后误把级别调没。
    #[test]
    fn file_layer_keeps_warn_and_error_drops_info() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        // 共享内存缓冲 + 实现 MakeWriter 的句柄。
        #[derive(Clone)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);
        struct BufGuard(Arc<Mutex<Vec<u8>>>);
        impl Write for BufGuard {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for SharedBuf {
            type Writer = BufGuard;
            fn make_writer(&'a self) -> Self::Writer {
                BufGuard(self.0.clone())
            }
        }

        let buf = SharedBuf(Arc::new(Mutex::new(Vec::new())));
        // 与生产文件层一致：JSON + EnvFilter("warn")。
        let layer = fmt::layer()
            .json()
            .with_writer(buf.clone())
            .with_filter(EnvFilter::new("warn"));
        let subscriber = tracing_subscriber::registry().with(layer);

        // 仅在本作用域内生效，不污染其他测试的全局 subscriber。
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("info_should_be_dropped");
            tracing::debug!("debug_should_be_dropped");
            tracing::warn!("warn_must_be_kept");
            tracing::error!("error_must_be_kept");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(out.contains("warn_must_be_kept"), "WARN 必须落盘: {out}");
        assert!(out.contains("error_must_be_kept"), "ERROR 必须落盘: {out}");
        assert!(
            !out.contains("info_should_be_dropped"),
            "INFO 不应落盘: {out}"
        );
        assert!(
            !out.contains("debug_should_be_dropped"),
            "DEBUG 不应落盘: {out}"
        );
    }
}
