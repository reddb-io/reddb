//! Time-bucket aggregation functions for time-series queries

use super::chunk::TimeSeriesPoint;

/// Aggregation type for downsampling and GROUP BY time_bucket
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AggregationType {
    Avg,
    Min,
    Max,
    Sum,
    Count,
    First,
    Last,
}

impl AggregationType {
    /// Parse from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "avg" | "average" | "mean" => Some(Self::Avg),
            "min" | "minimum" => Some(Self::Min),
            "max" | "maximum" => Some(Self::Max),
            "sum" | "total" => Some(Self::Sum),
            "count" => Some(Self::Count),
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            _ => None,
        }
    }
}

/// Bucket points by time intervals and aggregate
///
/// Returns (bucket_timestamp, aggregated_value) pairs
pub fn time_bucket(
    points: &[TimeSeriesPoint],
    bucket_ns: u64,
    agg: AggregationType,
) -> Vec<(u64, f64)> {
    if points.is_empty() || bucket_ns == 0 {
        return Vec::new();
    }

    // Group by bucket
    let mut buckets: Vec<(u64, Vec<f64>)> = Vec::new();
    let mut current_bucket_start = (points[0].timestamp_ns / bucket_ns) * bucket_ns;
    let mut current_values = Vec::new();

    for point in points {
        let bucket_start = (point.timestamp_ns / bucket_ns) * bucket_ns;
        if bucket_start != current_bucket_start {
            if !current_values.is_empty() {
                buckets.push((current_bucket_start, std::mem::take(&mut current_values)));
            }
            current_bucket_start = bucket_start;
        }
        current_values.push(point.value);
    }
    if !current_values.is_empty() {
        buckets.push((current_bucket_start, current_values));
    }

    // Aggregate each bucket
    buckets
        .into_iter()
        .map(|(ts, values)| (ts, aggregate(&values, agg)))
        .collect()
}

/// Apply an aggregation function to a slice of values
pub fn aggregate(values: &[f64], agg: AggregationType) -> f64 {
    match agg {
        AggregationType::Avg => {
            if values.is_empty() {
                0.0
            } else {
                values.iter().sum::<f64>() / values.len() as f64
            }
        }
        AggregationType::Min => values.iter().cloned().fold(f64::INFINITY, f64::min),
        AggregationType::Max => values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        AggregationType::Sum => values.iter().sum(),
        AggregationType::Count => values.len() as f64,
        AggregationType::First => values.first().copied().unwrap_or(0.0),
        AggregationType::Last => values.last().copied().unwrap_or(0.0),
    }
}

/// Streaming window aggregator for incremental computation
pub struct WindowAggregator {
    agg_type: AggregationType,
    sum: f64,
    count: usize,
    min: f64,
    max: f64,
    first: Option<f64>,
    last: f64,
}

impl WindowAggregator {
    pub fn new(agg_type: AggregationType) -> Self {
        Self {
            agg_type,
            sum: 0.0,
            count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            first: None,
            last: 0.0,
        }
    }

    pub fn add(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        if self.first.is_none() {
            self.first = Some(value);
        }
        self.last = value;
    }

    pub fn result(&self) -> f64 {
        match self.agg_type {
            AggregationType::Avg => {
                if self.count == 0 {
                    0.0
                } else {
                    self.sum / self.count as f64
                }
            }
            AggregationType::Min => self.min,
            AggregationType::Max => self.max,
            AggregationType::Sum => self.sum,
            AggregationType::Count => self.count as f64,
            AggregationType::First => self.first.unwrap_or(0.0),
            AggregationType::Last => self.last,
        }
    }

    pub fn reset(&mut self) {
        self.sum = 0.0;
        self.count = 0;
        self.min = f64::INFINITY;
        self.max = f64::NEG_INFINITY;
        self.first = None;
        self.last = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_points(data: &[(u64, f64)]) -> Vec<TimeSeriesPoint> {
        data.iter()
            .map(|&(ts, val)| TimeSeriesPoint {
                timestamp_ns: ts,
                value: val,
            })
            .collect()
    }

    #[test]
    fn test_time_bucket_avg() {
        let points = make_points(&[
            (0, 10.0),
            (500, 20.0),
            (1000, 30.0),
            (1500, 40.0),
            (2000, 50.0),
        ]);
        let result = time_bucket(&points, 1000, AggregationType::Avg);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], (0, 15.0)); // avg(10, 20)
        assert_eq!(result[1], (1000, 35.0)); // avg(30, 40)
        assert_eq!(result[2], (2000, 50.0)); // avg(50)
    }

    #[test]
    fn test_time_bucket_sum() {
        let points = make_points(&[(0, 1.0), (500, 2.0), (1000, 3.0)]);
        let result = time_bucket(&points, 1000, AggregationType::Sum);
        assert_eq!(result[0], (0, 3.0)); // sum(1, 2)
        assert_eq!(result[1], (1000, 3.0)); // sum(3)
    }

    #[test]
    fn test_time_bucket_min_max() {
        let points = make_points(&[(0, 5.0), (500, 2.0), (1000, 8.0), (1500, 3.0)]);

        let mins = time_bucket(&points, 1000, AggregationType::Min);
        assert_eq!(mins[0], (0, 2.0));
        assert_eq!(mins[1], (1000, 3.0));

        let maxs = time_bucket(&points, 1000, AggregationType::Max);
        assert_eq!(maxs[0], (0, 5.0));
        assert_eq!(maxs[1], (1000, 8.0));
    }

    #[test]
    fn test_time_bucket_count() {
        let points = make_points(&[(0, 1.0), (100, 2.0), (200, 3.0), (1000, 4.0)]);
        let result = time_bucket(&points, 1000, AggregationType::Count);
        assert_eq!(result[0], (0, 3.0));
        assert_eq!(result[1], (1000, 1.0));
    }

    #[test]
    fn test_window_aggregator() {
        let mut agg = WindowAggregator::new(AggregationType::Avg);
        agg.add(10.0);
        agg.add(20.0);
        agg.add(30.0);
        assert_eq!(agg.result(), 20.0);

        agg.reset();
        agg.add(100.0);
        assert_eq!(agg.result(), 100.0);
    }

    #[test]
    fn test_aggregation_type_parse() {
        assert_eq!(AggregationType::from_str("avg"), Some(AggregationType::Avg));
        assert_eq!(AggregationType::from_str("MIN"), Some(AggregationType::Min));
        assert_eq!(AggregationType::from_str("unknown"), None);
    }
}
