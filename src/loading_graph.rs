//! Small allocation-light helpers for the native world-loading throughput graph.

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const EMPTY: char = '·';

/// Render recent non-negative throughput samples as a compact Unicode bar graph.
pub(crate) fn sparkline(samples: &[f64]) -> String {
    let peak = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite() && *sample > 0.0)
        .fold(0.0f64, f64::max);
    samples
        .iter()
        .map(|sample| {
            if !sample.is_finite() || *sample <= 0.0 || peak == 0.0 {
                EMPTY
            } else {
                let level = ((*sample / peak * SPARK.len() as f64).ceil() as usize)
                    .clamp(1, SPARK.len())
                    - 1;
                SPARK[level]
            }
        })
        .collect()
}

pub(crate) fn mib_per_second(bytes: u64, elapsed_seconds: f64) -> f64 {
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 / (1024.0 * 1024.0) / elapsed_seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_scales_samples_and_marks_idle_intervals() {
        let graph = sparkline(&[0.0, 1.0, 2.0, 4.0]);
        let chars: Vec<_> = graph.chars().collect();
        assert_eq!(chars.len(), 4);
        assert_eq!(chars[0], EMPTY);
        assert!(chars[1] < chars[2] && chars[2] < chars[3]);
        assert_eq!(chars[3], '█');
        assert_eq!(sparkline(&[0.0, f64::NAN]), "··");
    }

    #[test]
    fn mib_rate_uses_binary_mebibytes_and_rejects_invalid_time() {
        assert_eq!(mib_per_second(2 * 1024 * 1024, 2.0), 1.0);
        assert_eq!(mib_per_second(100, 0.0), 0.0);
        assert_eq!(mib_per_second(100, f64::INFINITY), 0.0);
    }
}
