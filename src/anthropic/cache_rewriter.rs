use crate::model::config::{CacheOptimizerConfig, CacheSegment};

#[derive(Clone, Copy)]
pub(crate) enum ResponsePath {
    Stream,
    NonStream,
    Buffered,
}

pub(crate) fn rewrite_cache_usage(
    raw_read: i32,
    raw_write: i32,
    config: &CacheOptimizerConfig,
    path: ResponsePath,
) -> (i32, i32) {
    if !config.enabled {
        return (raw_read, raw_write);
    }

    let path_enabled = match path {
        ResponsePath::Stream => config.enabled_stream,
        ResponsePath::NonStream => config.enabled_non_stream,
        ResponsePath::Buffered => config.enabled_buffered,
    };
    if !path_enabled {
        return (raw_read, raw_write);
    }

    if config.rewrite_only_when_present && raw_read == 0 && raw_write == 0 {
        return (0, 0);
    }

    match config.mode.as_str() {
        "passthrough" => (raw_read, raw_write),
        "zero" => (0, 0),
        "cap" => (
            raw_read.min(config.read_max as i32),
            raw_write.min(config.write_max as i32),
        ),
        "random" => (
            random_in_range(config.read_min, config.read_max),
            random_in_range(config.write_min, config.write_max),
        ),
        "weighted" => weighted_rewrite(raw_read, raw_write, config),
        _ => (raw_read, raw_write),
    }
}

/// 改写缓存读写，并同步 5m/1h 拆分。
///
/// 在 `rewrite_cache_usage` 基础上，把 cache_creation 的 5m/1h 拆分同步到改写后的
/// 总写值（归整到 5m，清空 1h），避免下游读到「总 cache_creation 与 5m+1h 拆分不一致」
/// 的数据。下游 new-api 的 Claude 计费实际以 5m/1h 拆分值结算，若拆分值不同步，
/// 改写会被架空。
///
/// 入参/返回均为 `(read, creation_total, creation_5m, creation_1h)`。
pub(crate) fn rewrite_cache_usage_with_split(
    raw_read: i32,
    raw_creation: i32,
    raw_5m: i32,
    raw_1h: i32,
    config: &CacheOptimizerConfig,
    path: ResponsePath,
) -> (i32, i32, i32, i32) {
    let (new_read, new_creation) = rewrite_cache_usage(raw_read, raw_creation, config, path);
    if new_creation == raw_creation {
        // 总写值未变，拆分保持原样。
        return (new_read, new_creation, raw_5m, raw_1h);
    }
    // 总写值被改写：把拆分归整到 5m，清空 1h，保证 5m+1h == 总值。
    (new_read, new_creation, new_creation, 0)
}

/// 计算改写后的 input_tokens。
///
/// 仅当模拟缓存开启、当前路径开启、且 `input_random_max > 0` 时，
/// 返回 `Some(随机 [1, input_random_max])`；否则返回 `None`（表示不改写，沿用原值）。
///
/// 下限取 1 而非 0：下游 new-api 解析 Claude 流式 usage 时，message_delta 的
/// input_tokens 只有在 `> 0` 才会覆盖 message_start 里的真实值。若这里返回 0，
/// new-api 会丢弃该 0 并回退使用 message_start 的真实大值，导致偶发的超大 input 计费。
pub(crate) fn rewrite_input_tokens(
    config: &CacheOptimizerConfig,
    path: ResponsePath,
) -> Option<i32> {
    if !config.enabled {
        return None;
    }
    let path_enabled = match path {
        ResponsePath::Stream => config.enabled_stream,
        ResponsePath::NonStream => config.enabled_non_stream,
        ResponsePath::Buffered => config.enabled_buffered,
    };
    if !path_enabled || config.input_random_max == 0 {
        return None;
    }
    Some(random_in_range(1, config.input_random_max as u64))
}

fn weighted_rewrite(raw_read: i32, raw_write: i32, config: &CacheOptimizerConfig) -> (i32, i32) {
    let total_weight = config.weight_read_only
        + config.weight_write_only
        + config.weight_read_write
        + config.weight_none;

    if total_weight == 0 {
        return (0, 0);
    }

    let shape = weighted_pick(&[
        ("readOnly", config.weight_read_only),
        ("writeOnly", config.weight_write_only),
        ("readWrite", config.weight_read_write),
        ("none", config.weight_none),
    ]);

    // If rewrite_only_when_present, constrain shapes based on upstream
    let shape = if config.rewrite_only_when_present {
        match (raw_read > 0, raw_write > 0) {
            (true, false) => {
                if shape == "writeOnly" || shape == "readWrite" {
                    "readOnly"
                } else {
                    shape
                }
            }
            (false, true) => {
                if shape == "readOnly" || shape == "readWrite" {
                    "writeOnly"
                } else {
                    shape
                }
            }
            (false, false) => "none",
            _ => shape,
        }
    } else {
        shape
    };

    let read_val = if config.use_segment_weights {
        random_from_segments(&config.read_segments, config.read_min, config.read_max)
    } else {
        random_in_range(config.read_min, config.read_max)
    };

    let write_val = if config.use_segment_weights {
        random_from_segments(&config.write_segments, config.write_min, config.write_max)
    } else {
        random_in_range(config.write_min, config.write_max)
    };

    match shape {
        "readOnly" => (read_val, 0),
        "writeOnly" => (0, write_val),
        "readWrite" => (read_val, write_val),
        _ => (0, 0),
    }
}

fn random_in_range(min: u64, max: u64) -> i32 {
    if min >= max {
        return min as i32;
    }
    fastrand::u64(min..=max) as i32
}

fn random_from_segments(segments: &[CacheSegment], fallback_min: u64, fallback_max: u64) -> i32 {
    if segments.is_empty() {
        return random_in_range(fallback_min, fallback_max);
    }

    let total: u32 = segments.iter().map(|s| s.weight).sum();
    if total == 0 {
        return random_in_range(fallback_min, fallback_max);
    }

    let mut roll = fastrand::u32(0..total);
    for seg in segments {
        if roll < seg.weight {
            return random_in_range(seg.min, seg.max);
        }
        roll -= seg.weight;
    }

    random_in_range(fallback_min, fallback_max)
}

fn weighted_pick<'a>(entries: &[(&'a str, u32)]) -> &'a str {
    let total: u32 = entries.iter().map(|(_, w)| *w).sum();
    if total == 0 {
        return "none";
    }
    let mut roll = fastrand::u32(0..total);
    for (name, weight) in entries {
        if roll < *weight {
            return name;
        }
        roll -= weight;
    }
    entries.last().map(|(n, _)| *n).unwrap_or("none")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(mode: &str, enabled: bool) -> CacheOptimizerConfig {
        CacheOptimizerConfig {
            enabled,
            enabled_stream: true,
            enabled_non_stream: true,
            enabled_buffered: true,
            mode: mode.to_string(),
            read_min: 5000,
            read_max: 10000,
            write_min: 100,
            write_max: 500,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_returns_original() {
        let config = make_config("weighted", false);
        let (r, w) = rewrite_cache_usage(1000, 200, &config, ResponsePath::Stream);
        assert_eq!((r, w), (1000, 200));
    }

    #[test]
    fn path_disabled_returns_original() {
        let mut config = make_config("zero", true);
        config.enabled_stream = false;
        let (r, w) = rewrite_cache_usage(1000, 200, &config, ResponsePath::Stream);
        assert_eq!((r, w), (1000, 200));
        // But NonStream should work
        let (r2, w2) = rewrite_cache_usage(1000, 200, &config, ResponsePath::NonStream);
        assert_eq!((r2, w2), (0, 0));
    }

    #[test]
    fn zero_mode() {
        let config = make_config("zero", true);
        let (r, w) = rewrite_cache_usage(9999, 8888, &config, ResponsePath::NonStream);
        assert_eq!((r, w), (0, 0));
    }

    #[test]
    fn passthrough_mode() {
        let config = make_config("passthrough", true);
        let (r, w) = rewrite_cache_usage(1234, 567, &config, ResponsePath::Buffered);
        assert_eq!((r, w), (1234, 567));
    }

    #[test]
    fn cap_mode() {
        let mut config = make_config("cap", true);
        config.read_max = 500;
        config.write_max = 100;
        let (r, w) = rewrite_cache_usage(1000, 200, &config, ResponsePath::Stream);
        assert_eq!((r, w), (500, 100));
        // Under cap keeps original
        let (r2, w2) = rewrite_cache_usage(300, 50, &config, ResponsePath::Stream);
        assert_eq!((r2, w2), (300, 50));
    }

    #[test]
    fn random_mode_within_range() {
        let config = make_config("random", true);
        for _ in 0..100 {
            let (r, w) = rewrite_cache_usage(99999, 99999, &config, ResponsePath::Stream);
            assert!(r >= 5000 && r <= 10000, "read {r} out of range");
            assert!(w >= 100 && w <= 500, "write {w} out of range");
        }
    }

    #[test]
    fn rewrite_only_when_present_skips_zero_input() {
        let mut config = make_config("random", true);
        config.rewrite_only_when_present = true;
        let (r, w) = rewrite_cache_usage(0, 0, &config, ResponsePath::Stream);
        assert_eq!((r, w), (0, 0));
    }

    #[test]
    fn weighted_mode_produces_valid_shapes() {
        let mut config = make_config("weighted", true);
        config.weight_read_only = 100;
        config.weight_write_only = 0;
        config.weight_read_write = 0;
        config.weight_none = 0;
        config.rewrite_only_when_present = false;
        for _ in 0..50 {
            let (r, w) = rewrite_cache_usage(1000, 1000, &config, ResponsePath::Stream);
            assert!(r >= 5000 && r <= 10000);
            assert_eq!(w, 0); // readOnly shape => write is 0
        }
    }

    #[test]
    fn rewrite_input_tokens_never_returns_zero() {
        // 下游 new-api 在 message_delta 的 input_tokens 为 0 时会丢弃并回退到
        // message_start 的真实大值，因此改写后的 input 必须 >= 1。
        let mut config = make_config("weighted", true);
        config.input_random_max = 10;
        for _ in 0..500 {
            let v = rewrite_input_tokens(&config, ResponsePath::Stream)
                .expect("input_random_max > 0 should rewrite");
            assert!(v >= 1 && v <= 10, "input {v} out of [1,10]");
        }
    }

    #[test]
    fn rewrite_input_tokens_disabled_when_max_zero() {
        let mut config = make_config("weighted", true);
        config.input_random_max = 0;
        assert_eq!(rewrite_input_tokens(&config, ResponsePath::Stream), None);
    }

    #[test]
    fn rewrite_with_split_syncs_5m_1h_when_total_changed() {
        // cap 模式：写上限 22000，喂 480000 + 拆分(480000,0)。
        let mut config = make_config("cap", true);
        config.read_max = 165_000;
        config.write_max = 22_000;
        let (read, creation, c5m, c1h) = rewrite_cache_usage_with_split(
            150_000, 480_000, 480_000, 0, &config, ResponsePath::Buffered,
        );
        assert_eq!(read, 150_000); // < 165000，cap 不变
        assert_eq!(creation, 22_000); // cap 到上限
        // 总值变了 → 5m/1h 必须同步，5m+1h == 总值
        assert_eq!(c5m, 22_000);
        assert_eq!(c1h, 0);
        assert_eq!(c5m + c1h, creation);
    }

    #[test]
    fn rewrite_with_split_keeps_5m_1h_when_total_unchanged() {
        // cap 模式但总值未超上限 → 拆分保持原样。
        let mut config = make_config("cap", true);
        config.write_max = 100_000;
        let (_read, creation, c5m, c1h) = rewrite_cache_usage_with_split(
            0, 8000, 5000, 3000, &config, ResponsePath::NonStream,
        );
        assert_eq!(creation, 8000); // 8000 < 100000，不变
        assert_eq!(c5m, 5000); // 原样保留
        assert_eq!(c1h, 3000);
    }

    #[test]
    fn disabled_is_identity_for_all_fields() {
        // 关闭模拟缓存：四个字段必须原样返回，不被任何模式/上限影响。
        let mut config = make_config("zero", false); // 即便 mode=zero，关闭时也不应清零
        config.read_max = 1;
        config.write_max = 1;
        config.input_random_max = 99;
        for path in [ResponsePath::Stream, ResponsePath::NonStream, ResponsePath::Buffered] {
            let (r, c, m5, h1) =
                rewrite_cache_usage_with_split(150_000, 480_000, 300_000, 180_000, &config, path);
            assert_eq!((r, c, m5, h1), (150_000, 480_000, 300_000, 180_000),
                "关闭时四字段必须原样返回");
            assert_eq!(rewrite_input_tokens(&config, path), None,
                "关闭时 input 不改写");
        }
    }

    #[test]
    fn passthrough_mode_is_identity_when_enabled() {
        // 开启但 mode=passthrough：等同关闭，原样返回。
        let config = make_config("passthrough", true);
        let (r, c, m5, h1) = rewrite_cache_usage_with_split(
            150_000, 480_000, 300_000, 180_000, &config, ResponsePath::Buffered,
        );
        assert_eq!((r, c, m5, h1), (150_000, 480_000, 300_000, 180_000));
    }

    /// 贴近正式环境的 weighted 配置（含读写分段权重）。
    fn prod_weighted_config() -> CacheOptimizerConfig {
        CacheOptimizerConfig {
            enabled: true,
            enabled_stream: true,
            enabled_non_stream: true,
            enabled_buffered: true,
            mode: "weighted".to_string(),
            read_min: 15_000,
            read_max: 165_000,
            write_min: 5,
            write_max: 22_000,
            weight_read_only: 12,
            weight_write_only: 8,
            weight_read_write: 90,
            weight_none: 0,
            rewrite_only_when_present: true,
            use_segment_weights: true,
            read_segments: vec![
                CacheSegment { min: 15_000, max: 70_000, weight: 18 },
                CacheSegment { min: 70_001, max: 110_000, weight: 52 },
                CacheSegment { min: 110_001, max: 165_000, weight: 30 },
            ],
            write_segments: vec![
                CacheSegment { min: 5, max: 800, weight: 72 },
                CacheSegment { min: 801, max: 6500, weight: 24 },
                CacheSegment { min: 6501, max: 22_000, weight: 4 },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn weighted_with_split_stays_in_range_and_syncs() {
        // 上游有读有写（48万写 / 15万读），weighted 模式跑多次：
        // 读/写要么 0、要么落在配置范围内；5m+1h 必须等于改写后的写总值。
        let config = prod_weighted_config();
        for _ in 0..1000 {
            let (read, creation, c5m, c1h) = rewrite_cache_usage_with_split(
                150_000, 480_000, 480_000, 0, &config, ResponsePath::Buffered,
            );
            // 写：要么 0（writeOnly 形态不会发生，因为上游有读有写 + readWrite 权重高，
            // 但 readOnly 形态会让写=0），要么落在 [5, 22000]
            assert!(
                creation == 0 || (5..=22_000).contains(&creation),
                "creation {creation} 超出 [5,22000]"
            );
            // 读：要么 0（writeOnly 形态），要么落在 [15000, 165000]
            assert!(
                read == 0 || (15_000..=165_000).contains(&read),
                "read {read} 超出 [15000,165000]"
            );
            // 关键：改写后总写值绝不应是上游真实的 480000
            assert_ne!(creation, 480_000, "写未被改写，仍是上游真实值");
            // 5m/1h 同步：改写后(总值变了)5m+1h == creation
            assert_eq!(c5m + c1h, creation, "5m+1h 必须等于写总值");
        }
    }
}
