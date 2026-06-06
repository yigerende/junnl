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
}
