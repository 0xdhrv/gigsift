//! Projects when a volume fills, and how sure that projection is.
//!
//! A straight line through free-space samples, with a range taken from how well
//! the line fits. A cleanup or a big download makes the fit worse, which correctly
//! widens the range rather than producing a confident wrong date.

use gigsift_core::{ForecastData, Outlook, SpaceSample, VolumeSpace};

const SECONDS_PER_DAY: f64 = 86_400.0;
const MIN_SAMPLES: usize = 3;
const MIN_SPAN_DAYS: f64 = 1.0;

/// Student's t for an 80% two-sided interval, by degrees of freedom from 1.
const T_80: [f64; 10] = [
    3.078, 1.886, 1.638, 1.533, 1.476, 1.440, 1.415, 1.397, 1.383, 1.372,
];
const T_80_BEYOND_TABLE: f64 = 1.32;

/// `samples` may be in any order. `volume` is the current reading, which anchors
/// the projection to now rather than to the last snapshot.
pub fn forecast(volume: VolumeSpace, samples: &[SpaceSample]) -> ForecastData {
    let mut points: Vec<(f64, f64)> = samples
        .iter()
        .map(|sample| {
            let days = sample.taken_at.as_second() as f64 / SECONDS_PER_DAY;
            (days, sample.available_bytes as f64)
        })
        .collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));

    let first = points.first().map_or(0.0, |point| point.0);
    let span_days = points.last().map_or(0.0, |point| point.0) - first;
    let mut result = ForecastData {
        volume,
        samples: samples.len() as u64,
        span_days,
        outlook: Outlook::InsufficientData,
        bytes_used_per_day: None,
        days_until_full: None,
        days_until_full_low: None,
        days_until_full_high: None,
    };
    if points.len() < MIN_SAMPLES || span_days < MIN_SPAN_DAYS {
        return result;
    }

    let (slope, slope_error) = fit(&points);
    let used_per_day = -slope;
    result.bytes_used_per_day = Some(used_per_day);
    if used_per_day <= 0.0 {
        result.outlook = Outlook::NotFilling;
        return result;
    }

    let margin = t_80(points.len() - 2) * slope_error;
    let available = volume.available_bytes as f64;
    result.outlook = Outlook::Filling;
    result.days_until_full = Some(available / used_per_day);
    result.days_until_full_low = Some(available / (used_per_day + margin));
    result.days_until_full_high =
        (used_per_day > margin).then(|| available / (used_per_day - margin));
    result
}

/// Least squares. Returns the slope and its standard error.
fn fit(points: &[(f64, f64)]) -> (f64, f64) {
    let count = points.len() as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / count;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / count;
    let spread_x: f64 = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum();
    let covariance: f64 = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum();
    let slope = covariance / spread_x;
    let intercept = mean_y - slope * mean_x;
    let residuals: f64 = points
        .iter()
        .map(|p| (p.1 - (intercept + slope * p.0)).powi(2))
        .sum();
    let variance = residuals / (count - 2.0);
    (slope, (variance / spread_x).sqrt())
}

fn t_80(degrees_of_freedom: usize) -> f64 {
    T_80.get(degrees_of_freedom - 1)
        .copied()
        .unwrap_or(T_80_BEYOND_TABLE)
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;

    const GB: u64 = 1_000_000_000;

    fn volume(available: u64) -> VolumeSpace {
        VolumeSpace {
            volume: 1,
            total_bytes: 1000 * GB,
            available_bytes: available,
        }
    }

    fn sample(day: i64, available: u64) -> SpaceSample {
        SpaceSample {
            taken_at: Timestamp::from_second(day * 86_400).unwrap(),
            available_bytes: available,
        }
    }

    /// Free space falling by exactly `rate` a day.
    fn steady(days: i64, start: u64, rate: u64) -> Vec<SpaceSample> {
        (0..days)
            .map(|day| sample(day, start - rate * day as u64))
            .collect()
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-6 * expected.abs().max(1.0)
    }

    #[test]
    fn too_few_samples_say_so() {
        let data = forecast(volume(100 * GB), &steady(2, 200 * GB, GB));
        assert_eq!(data.outlook, Outlook::InsufficientData);
        assert_eq!(data.days_until_full, None);
        assert_eq!(data.bytes_used_per_day, None);
    }

    #[test]
    fn samples_from_a_single_moment_say_so() {
        let samples = [sample(5, GB), sample(5, GB), sample(5, GB), sample(5, GB)];
        let data = forecast(volume(GB), &samples);
        assert_eq!(data.outlook, Outlook::InsufficientData);
    }

    #[test]
    fn no_samples_at_all_are_handled() {
        let data = forecast(volume(GB), &[]);
        assert_eq!(data.outlook, Outlook::InsufficientData);
        assert_eq!(data.samples, 0);
    }

    #[test]
    fn a_perfectly_steady_decline_gives_a_tight_date() {
        let data = forecast(volume(100 * GB), &steady(10, 200 * GB, GB));

        assert_eq!(data.outlook, Outlook::Filling);
        assert!(close(data.bytes_used_per_day.unwrap(), GB as f64));
        assert!(close(data.days_until_full.unwrap(), 100.0));
        assert!(close(data.days_until_full_low.unwrap(), 100.0));
        assert!(close(data.days_until_full_high.unwrap(), 100.0));
    }

    #[test]
    fn the_projection_starts_from_the_current_reading() {
        let data = forecast(volume(50 * GB), &steady(10, 200 * GB, GB));
        assert!(close(data.days_until_full.unwrap(), 50.0));
    }

    #[test]
    fn free_space_growing_is_not_filling() {
        let samples: Vec<_> = (0..5)
            .map(|day| sample(day, 100 * GB + day as u64 * GB))
            .collect();
        let data = forecast(volume(105 * GB), &samples);

        assert_eq!(data.outlook, Outlook::NotFilling);
        assert_eq!(data.days_until_full, None);
    }

    #[test]
    fn noise_widens_the_range_around_the_estimate() {
        let noisy: Vec<_> = (0..10)
            .map(|day| {
                let wobble = if day % 2 == 0 { 3 * GB } else { 0 };
                sample(day, 200 * GB - GB * day as u64 + wobble)
            })
            .collect();

        let data = forecast(volume(100 * GB), &noisy);

        let (low, mid, high) = (
            data.days_until_full_low.unwrap(),
            data.days_until_full.unwrap(),
            data.days_until_full_high.unwrap(),
        );
        assert!(low < mid && mid < high, "{low} {mid} {high}");
        assert!(high - low > 10.0, "{low} {high}");
    }

    #[test]
    fn a_trend_lost_in_noise_has_no_latest_date() {
        let swings = [80, 20, 85, 15, 82, 10, 78, 12];
        let samples: Vec<_> = swings
            .iter()
            .enumerate()
            .map(|(day, gb)| sample(day as i64, gb * GB))
            .collect();

        let data = forecast(volume(50 * GB), &samples);

        assert_eq!(data.outlook, Outlook::Filling);
        assert!(data.days_until_full_high.is_none());
        assert!(data.days_until_full_low.is_some());
    }

    #[test]
    fn sample_order_does_not_matter() {
        let mut samples = steady(6, 200 * GB, 2 * GB);
        let forward = forecast(volume(100 * GB), &samples);
        samples.reverse();
        assert_eq!(forecast(volume(100 * GB), &samples), forward);
    }
}
