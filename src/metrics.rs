use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use crate::model::LatencySummary;

const SAMPLE_WINDOW: usize = 512;

#[derive(Default)]
pub(crate) struct LatencyWindow {
    samples_us: Mutex<VecDeque<u64>>,
}

impl LatencyWindow {
    pub(crate) fn record(&self, duration: Duration) {
        self.record_ms(duration.as_secs_f64() * 1_000.0);
    }

    pub(crate) fn record_ms(&self, milliseconds: f64) {
        let microseconds = (milliseconds.max(0.0) * 1_000.0).round() as u64;
        let mut samples = self.samples_us.lock().expect("latency mutex poisoned");
        if samples.len() >= SAMPLE_WINDOW {
            samples.pop_front();
        }
        samples.push_back(microseconds);
    }

    pub(crate) fn snapshot(&self) -> LatencySummary {
        let samples = self.samples_us.lock().expect("latency mutex poisoned");
        if samples.is_empty() {
            return LatencySummary::default();
        }
        let mut ordered = samples.iter().copied().collect::<Vec<_>>();
        ordered.sort_unstable();
        LatencySummary {
            samples: ordered.len() as u64,
            p50_ms: percentile(&ordered, 0.50) as f64 / 1_000.0,
            p95_ms: percentile(&ordered, 0.95) as f64 / 1_000.0,
            max_ms: *ordered.last().unwrap_or(&0) as f64 / 1_000.0,
        }
    }
}

fn percentile(ordered: &[u64], quantile: f64) -> u64 {
    let rank = ((ordered.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(ordered.len().saturating_sub(1));
    ordered[rank]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_window_reports_nearest_rank_percentiles() {
        let window = LatencyWindow::default();
        for milliseconds in 1..=100 {
            window.record_ms(milliseconds as f64);
        }
        let summary = window.snapshot();
        assert_eq!(summary.samples, 100);
        assert_eq!(summary.p50_ms, 50.0);
        assert_eq!(summary.p95_ms, 95.0);
        assert_eq!(summary.max_ms, 100.0);
    }
}
