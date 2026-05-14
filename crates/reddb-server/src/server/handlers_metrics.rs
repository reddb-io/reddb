use super::*;
use crate::storage::timeseries::retention::DownsamplePolicy;
use prost::Message;
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq, Message)]
struct PromWriteRequest {
    #[prost(message, repeated, tag = "1")]
    timeseries: Vec<PromTimeSeries>,
}

#[derive(Clone, PartialEq, Message)]
struct PromTimeSeries {
    #[prost(message, repeated, tag = "1")]
    labels: Vec<PromLabel>,
    #[prost(message, repeated, tag = "2")]
    samples: Vec<PromSample>,
    #[prost(message, repeated, tag = "4")]
    histograms: Vec<PromNativeHistogram>,
}

#[derive(Clone, PartialEq, Message)]
struct PromNativeHistogram {}

#[derive(Clone, PartialEq, Message)]
struct PromLabel {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    value: String,
}

#[derive(Clone, PartialEq, Message)]
struct PromSample {
    #[prost(double, tag = "1")]
    value: f64,
    #[prost(int64, tag = "2")]
    timestamp: i64,
}

struct DecodedMetricBatch {
    entities: Vec<UnifiedEntity>,
    accepted_series: u64,
    accepted_samples: u64,
    rejected_series: u64,
    rejected_samples: u64,
    cardinality_budget_rejected_series: u64,
}

#[derive(Debug)]
struct PromSelector {
    metric: String,
    matchers: Vec<PromLabelMatcher>,
}

#[derive(Debug)]
enum PromExpression {
    Scalar(f64),
    Selector(PromSelector),
    CounterFunction {
        function: PromCounterFunction,
        selector: PromSelector,
        window_ns: u64,
    },
    Aggregate {
        op: PromAggregateOp,
        grouping: PromGrouping,
        expression: Box<PromExpression>,
    },
    HistogramQuantile {
        quantile: f64,
        expression: Box<PromExpression>,
    },
    Arithmetic {
        op: PromArithmeticOp,
        left: Box<PromExpression>,
        right: Box<PromExpression>,
    },
}

#[derive(Debug, Clone, Copy)]
enum PromCounterFunction {
    Rate,
    IRate,
    Increase,
}

#[derive(Debug, Clone, Copy)]
enum PromAggregateOp {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

#[derive(Debug)]
enum PromGrouping {
    None,
    By(Vec<String>),
    Without(Vec<String>),
}

#[derive(Debug, Clone, Copy)]
enum PromArithmeticOp {
    Add,
    Sub,
    Mul,
    Div,
}

enum PromInstantEval {
    Scalar(f64),
    Vector(Vec<PromVectorSample>),
}

enum PromRangeEval {
    Scalar(f64),
    Matrix(Vec<PromRangeSeries>),
}

#[derive(Debug)]
struct PromLabelMatcher {
    name: String,
    op: PromLabelMatcherOp,
    value: String,
}

#[derive(Debug, Clone, Copy)]
enum PromLabelMatcherOp {
    Equal,
    NotEqual,
}

#[derive(Debug, Clone)]
struct PromVectorSample {
    labels: BTreeMap<String, String>,
    timestamp_ns: u64,
    value: f64,
}

#[derive(Debug, Clone)]
struct PromRangeSeries {
    labels: BTreeMap<String, String>,
    values: Vec<PromRangeValue>,
}

#[derive(Debug, Clone, Copy)]
struct PromRangeValue {
    timestamp_ns: u64,
    value: f64,
}

#[derive(Debug, Clone, Copy)]
struct PromQueryRange {
    start_ns: u64,
    end_ns: u64,
    step_ns: u64,
}

impl RedDBServer {
    pub(crate) fn handle_prometheus_query(
        &self,
        query: &BTreeMap<String, String>,
        body: Option<Vec<u8>>,
    ) -> HttpResponse {
        let raw_query = match prometheus_query_param(query, body.as_deref()) {
            Ok(query) => query,
            Err(err) => return prometheus_error_response(400, "bad_data", err),
        };
        let expression = match parse_prom_expression(&raw_query) {
            Ok(expression) => expression,
            Err(err) => return prometheus_error_response(422, "bad_data", err),
        };

        let eval_time_ns = match prometheus_optional_param(query, body.as_deref(), "time") {
            Ok(Some(raw)) => match parse_prom_timestamp_ns(&raw) {
                Ok(time) => Some(time),
                Err(err) => return prometheus_error_response(400, "bad_data", err),
            },
            Ok(None) => None,
            Err(err) => return prometheus_error_response(400, "bad_data", err),
        };
        match self.prometheus_eval_instant(expression, eval_time_ns) {
            Ok(PromInstantEval::Vector(samples)) => prometheus_vector_response(samples),
            Ok(PromInstantEval::Scalar(value)) => prometheus_scalar_response(value),
            Err(err) => prometheus_query_error_response(err),
        }
    }

    pub(crate) fn handle_prometheus_query_range(
        &self,
        query: &BTreeMap<String, String>,
        body: Option<Vec<u8>>,
    ) -> HttpResponse {
        let raw_query = match prometheus_required_param(query, body.as_deref(), "query") {
            Ok(query) => query,
            Err(err) => return prometheus_error_response(400, "bad_data", err),
        };
        let expression = match parse_prom_expression(&raw_query) {
            Ok(expression) => expression,
            Err(err) => return prometheus_error_response(422, "bad_data", err),
        };
        let range = match parse_prom_query_range(query, body.as_deref()) {
            Ok(range) => range,
            Err(err) => return prometheus_error_response(400, "bad_data", err),
        };

        match self.prometheus_eval_range(expression, range) {
            Ok(PromRangeEval::Matrix(series)) => prometheus_matrix_response(series),
            Ok(PromRangeEval::Scalar(value)) => prometheus_scalar_response(value),
            Err(err) => prometheus_query_error_response(err),
        }
    }

    pub(crate) fn handle_prometheus_remote_write(
        &self,
        query: &BTreeMap<String, String>,
        headers: &BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> HttpResponse {
        match self.ingest_prometheus_remote_write(query, headers, &body) {
            Ok(()) => HttpResponse {
                status: 204,
                body: Vec::new(),
                content_type: "text/plain",
                extra_headers: Vec::new(),
            },
            Err(err) => {
                let (status, msg) = map_runtime_error(&err);
                json_error(status, msg)
            }
        }
    }

    fn ingest_prometheus_remote_write(
        &self,
        query: &BTreeMap<String, String>,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> RedDBResult<()> {
        validate_remote_write_headers(headers)?;
        let collection = resolve_metrics_collection(&self.runtime, query)?;
        let contract = self
            .runtime
            .db()
            .collection_contract(&collection)
            .ok_or_else(|| RedDBError::NotFound(collection.clone()))?;
        if contract.declared_model != CollectionModel::Metrics {
            return Err(RedDBError::Query(format!(
                "collection '{collection}' is not a metrics collection"
            )));
        }

        let decoded = snap::raw::Decoder::new()
            .decompress_vec(body)
            .map_err(|err| RedDBError::Query(format!("invalid remote_write snappy body: {err}")))?;
        let request = PromWriteRequest::decode(decoded.as_slice()).map_err(|err| {
            RedDBError::Query(format!("invalid remote_write protobuf body: {err}"))
        })?;
        let rejected_samples = request
            .timeseries
            .iter()
            .map(|series| series.samples.len() as u64)
            .sum::<u64>();
        let rejected_series = request.timeseries.len() as u64;
        let batch = match decode_metric_batch(&self.runtime, &collection, &contract, request) {
            Ok(batch) => batch,
            Err(err) => {
                self.runtime
                    .record_metrics_ingest(0, 0, rejected_samples, rejected_series);
                return Err(err);
            }
        };
        if !batch.entities.is_empty() {
            self.runtime
                .db()
                .store()
                .bulk_insert(&collection, batch.entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            materialize_metrics_rollups(&self.runtime, &collection, &contract)?;
        }
        self.runtime.record_metrics_ingest(
            batch.accepted_samples,
            batch.accepted_series,
            batch.rejected_samples,
            batch.rejected_series,
        );
        if batch.cardinality_budget_rejected_series > 0 {
            self.runtime.record_metrics_cardinality_budget_rejections(
                batch.cardinality_budget_rejected_series,
            );
        }
        Ok(())
    }

    fn prometheus_instant_vector(
        &self,
        selector: PromSelector,
    ) -> RedDBResult<Vec<PromVectorSample>> {
        let mut latest_by_series: BTreeMap<Vec<(String, String)>, PromVectorSample> =
            BTreeMap::new();
        let store = self.runtime.db().store();
        for contract in self
            .runtime
            .db()
            .collection_contracts()
            .into_iter()
            .filter(|contract| contract.declared_model == CollectionModel::Metrics)
        {
            let Some(manager) = store.get_collection(&contract.name) else {
                continue;
            };
            let entities =
                manager.query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
            for entity in entities {
                let EntityData::TimeSeries(point) = entity.data else {
                    continue;
                };
                if point.metric != selector.metric {
                    continue;
                }
                let labels = prometheus_labels_for_point(&point);
                if !selector_matches(&selector, &labels) {
                    continue;
                }
                let series_key = labels
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                let candidate = PromVectorSample {
                    labels,
                    timestamp_ns: point.timestamp_ns,
                    value: point.value,
                };
                match latest_by_series.get(&series_key) {
                    Some(existing) if existing.timestamp_ns >= candidate.timestamp_ns => {}
                    _ => {
                        latest_by_series.insert(series_key, candidate);
                    }
                }
            }
        }

        Ok(latest_by_series.into_values().collect())
    }

    fn prometheus_range_matrix(
        &self,
        selector: PromSelector,
        range: PromQueryRange,
    ) -> RedDBResult<Vec<PromRangeSeries>> {
        let mut samples_by_series: BTreeMap<Vec<(String, String)>, PromRangeSeries> =
            BTreeMap::new();
        let store = self.runtime.db().store();
        for contract in self
            .runtime
            .db()
            .collection_contracts()
            .into_iter()
            .filter(|contract| contract.declared_model == CollectionModel::Metrics)
        {
            let source_collection =
                select_metrics_range_collection(store.as_ref(), &contract, range);
            let Some(manager) = store.get_collection(&source_collection) else {
                continue;
            };
            let entities =
                manager.query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
            for entity in entities {
                let EntityData::TimeSeries(point) = entity.data else {
                    continue;
                };
                if point.metric != selector.metric {
                    continue;
                }
                if point.timestamp_ns < range.start_ns || point.timestamp_ns > range.end_ns {
                    continue;
                }
                let labels = prometheus_labels_for_point(&point);
                if !selector_matches(&selector, &labels) {
                    continue;
                }
                let series_key = labels
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                samples_by_series
                    .entry(series_key)
                    .or_insert_with(|| PromRangeSeries {
                        labels,
                        values: Vec::new(),
                    })
                    .values
                    .push(PromRangeValue {
                        timestamp_ns: point.timestamp_ns,
                        value: point.value,
                    });
            }
        }

        let mut matrix = Vec::new();
        for mut series in samples_by_series.into_values() {
            series.values.sort_by_key(|sample| sample.timestamp_ns);
            series.values = align_range_values_to_steps(&series.values, range);
            if !series.values.is_empty() {
                matrix.push(series);
            }
        }
        Ok(matrix)
    }

    fn prometheus_eval_instant(
        &self,
        expression: PromExpression,
        eval_time_ns: Option<u64>,
    ) -> RedDBResult<PromInstantEval> {
        match expression {
            PromExpression::Scalar(value) => Ok(PromInstantEval::Scalar(value)),
            PromExpression::Selector(selector) => self
                .prometheus_instant_vector(selector)
                .map(PromInstantEval::Vector),
            PromExpression::CounterFunction {
                function,
                selector,
                window_ns,
            } => self
                .prometheus_counter_function_vector(function, selector, window_ns, eval_time_ns)
                .map(PromInstantEval::Vector),
            PromExpression::Aggregate {
                op,
                grouping,
                expression,
            } => match self.prometheus_eval_instant(*expression, eval_time_ns)? {
                PromInstantEval::Vector(samples) => Ok(PromInstantEval::Vector(aggregate_vector(
                    samples, op, grouping,
                ))),
                PromInstantEval::Scalar(_) => Err(RedDBError::Query(
                    "unsupported PromQL aggregation over scalar".to_string(),
                )),
            },
            PromExpression::HistogramQuantile {
                quantile,
                expression,
            } => match self.prometheus_eval_instant(*expression, eval_time_ns)? {
                PromInstantEval::Vector(samples) => Ok(PromInstantEval::Vector(
                    histogram_quantile_vector(quantile, samples),
                )),
                PromInstantEval::Scalar(_) => Err(RedDBError::Query(
                    "unsupported histogram_quantile over scalar".to_string(),
                )),
            },
            PromExpression::Arithmetic { op, left, right } => {
                let left = self.prometheus_eval_instant(*left, eval_time_ns)?;
                let right = self.prometheus_eval_instant(*right, eval_time_ns)?;
                apply_instant_arithmetic(left, right, op)
            }
        }
    }

    fn prometheus_eval_range(
        &self,
        expression: PromExpression,
        range: PromQueryRange,
    ) -> RedDBResult<PromRangeEval> {
        match expression {
            PromExpression::Scalar(value) => Ok(PromRangeEval::Scalar(value)),
            PromExpression::Selector(selector) => self
                .prometheus_range_matrix(selector, range)
                .map(PromRangeEval::Matrix),
            PromExpression::CounterFunction {
                function,
                selector,
                window_ns,
            } => self
                .prometheus_counter_function_matrix(function, selector, window_ns, range)
                .map(PromRangeEval::Matrix),
            PromExpression::Aggregate {
                op,
                grouping,
                expression,
            } => match self.prometheus_eval_range(*expression, range)? {
                PromRangeEval::Matrix(series) => Ok(PromRangeEval::Matrix(aggregate_matrix(
                    series, op, grouping,
                ))),
                PromRangeEval::Scalar(_) => Err(RedDBError::Query(
                    "unsupported PromQL aggregation over scalar".to_string(),
                )),
            },
            PromExpression::HistogramQuantile {
                quantile,
                expression,
            } => match self.prometheus_eval_range(*expression, range)? {
                PromRangeEval::Matrix(series) => Ok(PromRangeEval::Matrix(
                    histogram_quantile_matrix(quantile, series),
                )),
                PromRangeEval::Scalar(_) => Err(RedDBError::Query(
                    "unsupported histogram_quantile over scalar".to_string(),
                )),
            },
            PromExpression::Arithmetic { op, left, right } => {
                let left = self.prometheus_eval_range(*left, range)?;
                let right = self.prometheus_eval_range(*right, range)?;
                apply_range_arithmetic(left, right, op)
            }
        }
    }

    fn prometheus_counter_function_vector(
        &self,
        function: PromCounterFunction,
        selector: PromSelector,
        window_ns: u64,
        eval_time_ns: Option<u64>,
    ) -> RedDBResult<Vec<PromVectorSample>> {
        let eval_time_ns = match eval_time_ns {
            Some(time) => time,
            None => match self.latest_timestamp_for_selector(&selector)? {
                Some(time) => time,
                None => return Ok(Vec::new()),
            },
        };
        let window_start_ns = eval_time_ns.saturating_sub(window_ns);
        let samples_by_series =
            self.prometheus_samples_by_series(&selector, window_start_ns, eval_time_ns)?;
        let mut vector = Vec::new();
        for mut series in samples_by_series.into_values() {
            series.values.sort_by_key(|sample| sample.timestamp_ns);
            if let Some(value) =
                evaluate_counter_function(function, &series.values, window_start_ns, eval_time_ns)
            {
                vector.push(PromVectorSample {
                    labels: series.labels,
                    timestamp_ns: eval_time_ns,
                    value,
                });
            }
        }
        Ok(vector)
    }

    fn prometheus_counter_function_matrix(
        &self,
        function: PromCounterFunction,
        selector: PromSelector,
        window_ns: u64,
        range: PromQueryRange,
    ) -> RedDBResult<Vec<PromRangeSeries>> {
        let fetch_start_ns = range.start_ns.saturating_sub(window_ns);
        let samples_by_series =
            self.prometheus_samples_by_series(&selector, fetch_start_ns, range.end_ns)?;
        let mut matrix = Vec::new();
        for mut series in samples_by_series.into_values() {
            series.values.sort_by_key(|sample| sample.timestamp_ns);
            let mut values = Vec::new();
            let mut step = range.start_ns;
            while step <= range.end_ns {
                let window_start_ns = step.saturating_sub(window_ns);
                if let Some(value) =
                    evaluate_counter_function(function, &series.values, window_start_ns, step)
                {
                    values.push(PromRangeValue {
                        timestamp_ns: step,
                        value,
                    });
                }
                match step.checked_add(range.step_ns) {
                    Some(next) if next > step => step = next,
                    _ => break,
                }
            }
            if !values.is_empty() {
                series.values = values;
                matrix.push(series);
            }
        }
        Ok(matrix)
    }

    fn latest_timestamp_for_selector(&self, selector: &PromSelector) -> RedDBResult<Option<u64>> {
        let mut latest = None;
        for series in self
            .prometheus_samples_by_series(selector, 0, u64::MAX)?
            .into_values()
        {
            for sample in series.values {
                if latest.is_none_or(|current| sample.timestamp_ns > current) {
                    latest = Some(sample.timestamp_ns);
                }
            }
        }
        Ok(latest)
    }

    fn prometheus_samples_by_series(
        &self,
        selector: &PromSelector,
        start_ns: u64,
        end_ns: u64,
    ) -> RedDBResult<BTreeMap<Vec<(String, String)>, PromRangeSeries>> {
        let mut samples_by_series: BTreeMap<Vec<(String, String)>, PromRangeSeries> =
            BTreeMap::new();
        let store = self.runtime.db().store();
        for contract in self
            .runtime
            .db()
            .collection_contracts()
            .into_iter()
            .filter(|contract| contract.declared_model == CollectionModel::Metrics)
        {
            let Some(manager) = store.get_collection(&contract.name) else {
                continue;
            };
            let entities =
                manager.query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
            for entity in entities {
                let EntityData::TimeSeries(point) = entity.data else {
                    continue;
                };
                if point.metric != selector.metric {
                    continue;
                }
                if point.timestamp_ns < start_ns || point.timestamp_ns > end_ns {
                    continue;
                }
                let labels = prometheus_labels_for_point(&point);
                if !selector_matches(selector, &labels) {
                    continue;
                }
                let series_key = labels
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                samples_by_series
                    .entry(series_key)
                    .or_insert_with(|| PromRangeSeries {
                        labels,
                        values: Vec::new(),
                    })
                    .values
                    .push(PromRangeValue {
                        timestamp_ns: point.timestamp_ns,
                        value: point.value,
                    });
            }
        }
        Ok(samples_by_series)
    }
}

fn prometheus_query_param(
    query: &BTreeMap<String, String>,
    body: Option<&[u8]>,
) -> Result<String, String> {
    prometheus_required_param(query, body, "query")
}

fn prometheus_required_param(
    query: &BTreeMap<String, String>,
    body: Option<&[u8]>,
    name: &str,
) -> Result<String, String> {
    if let Some(value) = query.get("query") {
        if name == "query" {
            return percent_decode(value).map_err(|err| format!("invalid {name} parameter: {err}"));
        }
    }
    if let Some(value) = query.get(name) {
        return percent_decode(value).map_err(|err| format!("invalid {name} parameter: {err}"));
    }
    if let Some(body) = body {
        let body = std::str::from_utf8(body)
            .map_err(|err| format!("invalid form body encoding: {err}"))?;
        for pair in body.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if key == name {
                return percent_decode(value)
                    .map_err(|err| format!("invalid {name} form field: {err}"));
            }
        }
    }
    Err(format!("missing required query parameter '{name}'"))
}

fn prometheus_optional_param(
    query: &BTreeMap<String, String>,
    body: Option<&[u8]>,
    name: &str,
) -> Result<Option<String>, String> {
    match prometheus_required_param(query, body, name) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err == format!("missing required query parameter '{name}'") => Ok(None),
        Err(err) => Err(err),
    }
}

fn parse_prom_query_range(
    query: &BTreeMap<String, String>,
    body: Option<&[u8]>,
) -> Result<PromQueryRange, String> {
    let start_ns = parse_prom_timestamp_ns(&prometheus_required_param(query, body, "start")?)?;
    let end_ns = parse_prom_timestamp_ns(&prometheus_required_param(query, body, "end")?)?;
    let step_ns = parse_prom_step_ns(&prometheus_required_param(query, body, "step")?)?;
    if end_ns < start_ns {
        return Err("invalid query_range: end must be greater than or equal to start".to_string());
    }
    if step_ns == 0 {
        return Err("invalid query_range: step must be positive".to_string());
    }
    let steps = ((end_ns - start_ns) / step_ns) + 1;
    if steps > 11_000 {
        return Err("invalid query_range: too many points; increase step".to_string());
    }
    Ok(PromQueryRange {
        start_ns,
        end_ns,
        step_ns,
    })
}

fn parse_prom_timestamp_ns(input: &str) -> Result<u64, String> {
    let seconds = input
        .parse::<f64>()
        .map_err(|_| format!("invalid query_range timestamp '{input}'"))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(format!("invalid query_range timestamp '{input}'"));
    }
    Ok((seconds * 1_000_000_000.0).round() as u64)
}

fn parse_prom_step_ns(input: &str) -> Result<u64, String> {
    if let Ok(seconds) = input.parse::<f64>() {
        if seconds.is_finite() && seconds > 0.0 {
            return Ok((seconds * 1_000_000_000.0).round() as u64);
        }
        return Err("invalid query_range step: step must be positive".to_string());
    }

    let split = input
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .ok_or_else(|| format!("invalid query_range step '{input}'"))?;
    let amount = input[..split]
        .parse::<f64>()
        .map_err(|_| format!("invalid query_range step '{input}'"))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err("invalid query_range step: step must be positive".to_string());
    }
    let unit = &input[split..];
    let seconds = match unit {
        "ms" => amount / 1_000.0,
        "s" => amount,
        "m" => amount * 60.0,
        "h" => amount * 3_600.0,
        "d" => amount * 86_400.0,
        _ => return Err(format!("invalid query_range step unit '{unit}'")),
    };
    Ok((seconds * 1_000_000_000.0).round() as u64)
}

fn parse_prom_expression(input: &str) -> Result<PromExpression, String> {
    let input = input.trim();
    if let Some((left, op, right)) = split_top_level_arithmetic(input, &['+', '-']) {
        return Ok(PromExpression::Arithmetic {
            op,
            left: Box::new(parse_prom_expression(left)?),
            right: Box::new(parse_prom_expression(right)?),
        });
    }
    if let Some((left, op, right)) = split_top_level_arithmetic(input, &['*', '/']) {
        return Ok(PromExpression::Arithmetic {
            op,
            left: Box::new(parse_prom_expression(left)?),
            right: Box::new(parse_prom_expression(right)?),
        });
    }
    if let Ok(value) = input.parse::<f64>() {
        if value.is_finite() {
            return Ok(PromExpression::Scalar(value));
        }
    }
    if let Some(inner) = input
        .strip_prefix("histogram_quantile(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let (quantile_text, expression_text) = split_histogram_quantile_args(inner)?;
        let quantile = quantile_text
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("invalid histogram_quantile quantile '{quantile_text}'"))?;
        if !quantile.is_finite() || !(0.0..=1.0).contains(&quantile) {
            return Err("invalid histogram_quantile quantile: expected 0 <= q <= 1".to_string());
        }
        return Ok(PromExpression::HistogramQuantile {
            quantile,
            expression: Box::new(parse_prom_expression(expression_text)?),
        });
    }
    if let Some(aggregate) = parse_prom_aggregate(input)? {
        return Ok(aggregate);
    }
    for (name, function) in [
        ("rate", PromCounterFunction::Rate),
        ("irate", PromCounterFunction::IRate),
        ("increase", PromCounterFunction::Increase),
    ] {
        let prefix = format!("{name}(");
        if let Some(inner) = input
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let (selector_text, window_text) = parse_range_vector_inner(inner)?;
            let selector = parse_prom_selector(selector_text)?;
            let window_ns = parse_prom_duration_ns(window_text)?;
            return Ok(PromExpression::CounterFunction {
                function,
                selector,
                window_ns,
            });
        }
    }

    if input.contains('(') || input.contains(')') || input.contains('[') || input.contains(']') {
        return Err(format!(
            "unsupported PromQL '{input}': expected selector or rate/irate/increase(selector[window])"
        ));
    }
    parse_prom_selector(input).map(PromExpression::Selector)
}

fn split_histogram_quantile_args(input: &str) -> Result<(&str, &str), String> {
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '[' => bracket_depth += 1,
            ']' => bracket_depth -= 1,
            '{' => brace_depth += 1,
            '}' => brace_depth -= 1,
            ',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                let left = input[..idx].trim();
                let right = input[idx + ch.len_utf8()..].trim();
                if left.is_empty() || right.is_empty() {
                    break;
                }
                return Ok((left, right));
            }
            _ => {}
        }
    }
    Err("unsupported histogram_quantile shape: expected histogram_quantile(q, expr)".to_string())
}

fn split_top_level_arithmetic<'a>(
    input: &'a str,
    ops: &[char],
) -> Option<(&'a str, PromArithmeticOp, &'a str)> {
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let chars = input.char_indices().collect::<Vec<_>>();
    for (idx, ch) in chars.into_iter().rev() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ')' => paren_depth += 1,
            '(' => paren_depth -= 1,
            ']' => bracket_depth += 1,
            '[' => bracket_depth -= 1,
            '}' => brace_depth += 1,
            '{' => brace_depth -= 1,
            op if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && ops.contains(&op) =>
            {
                if idx == 0 {
                    continue;
                }
                let left = input[..idx].trim();
                let right = input[idx + ch.len_utf8()..].trim();
                if left.is_empty() || right.is_empty() {
                    continue;
                }
                let op = match op {
                    '+' => PromArithmeticOp::Add,
                    '-' => PromArithmeticOp::Sub,
                    '*' => PromArithmeticOp::Mul,
                    '/' => PromArithmeticOp::Div,
                    _ => return None,
                };
                return Some((left, op, right));
            }
            _ => {}
        }
    }
    None
}

fn parse_prom_aggregate(input: &str) -> Result<Option<PromExpression>, String> {
    for (name, op) in [
        ("sum", PromAggregateOp::Sum),
        ("avg", PromAggregateOp::Avg),
        ("min", PromAggregateOp::Min),
        ("max", PromAggregateOp::Max),
        ("count", PromAggregateOp::Count),
    ] {
        let Some(rest) = input.strip_prefix(name) else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            let (inner, after) = parse_parenthesized(rest)?;
            if !after.trim().is_empty() {
                return Err(format!("unsupported PromQL aggregate '{input}'"));
            }
            return Ok(Some(PromExpression::Aggregate {
                op,
                grouping: PromGrouping::None,
                expression: Box::new(parse_prom_expression(inner)?),
            }));
        }

        let (grouping, rest) = if let Some(after) = rest.strip_prefix("by") {
            let (labels, after) = parse_grouping_labels(after.trim_start())?;
            (PromGrouping::By(labels), after)
        } else if let Some(after) = rest.strip_prefix("without") {
            let (labels, after) = parse_grouping_labels(after.trim_start())?;
            (PromGrouping::Without(labels), after)
        } else {
            continue;
        };
        let (inner, after) = parse_parenthesized(rest.trim_start())?;
        if !after.trim().is_empty() {
            return Err(format!("unsupported PromQL aggregate '{input}'"));
        }
        return Ok(Some(PromExpression::Aggregate {
            op,
            grouping,
            expression: Box::new(parse_prom_expression(inner)?),
        }));
    }
    Ok(None)
}

fn parse_grouping_labels(input: &str) -> Result<(Vec<String>, &str), String> {
    let (inner, after) = parse_parenthesized(input)?;
    let mut labels = Vec::new();
    for raw in inner.split(',') {
        let label = raw.trim();
        if label.is_empty() {
            continue;
        }
        if !valid_label_name(label) {
            return Err(format!("unsupported PromQL grouping label '{label}'"));
        }
        labels.push(label.to_string());
    }
    Ok((labels, after))
}

fn parse_parenthesized(input: &str) -> Result<(&str, &str), String> {
    if !input.starts_with('(') {
        return Err("unsupported PromQL: expected parenthesized expression".to_string());
    }
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&input[1..idx], &input[idx + ch.len_utf8()..]));
                }
            }
            _ => {}
        }
    }
    Err("unsupported PromQL: unbalanced parentheses".to_string())
}

fn parse_range_vector_inner(input: &str) -> Result<(&str, &str), String> {
    let close = input
        .rfind(']')
        .ok_or_else(|| "unsupported function shape: missing range window".to_string())?;
    if close != input.len() - 1 {
        return Err("unsupported function shape: range window must end the argument".to_string());
    }
    let open = input[..close]
        .rfind('[')
        .ok_or_else(|| "unsupported function shape: missing range window".to_string())?;
    let selector = input[..open].trim();
    let window = input[open + 1..close].trim();
    if selector.is_empty() || window.is_empty() {
        return Err("unsupported function shape: selector and window are required".to_string());
    }
    Ok((selector, window))
}

fn parse_prom_duration_ns(input: &str) -> Result<u64, String> {
    let split = input
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .ok_or_else(|| format!("invalid PromQL duration '{input}'"))?;
    let amount = input[..split]
        .parse::<f64>()
        .map_err(|_| format!("invalid PromQL duration '{input}'"))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err("invalid PromQL duration: duration must be positive".to_string());
    }
    let unit = &input[split..];
    let seconds = match unit {
        "ms" => amount / 1_000.0,
        "s" => amount,
        "m" => amount * 60.0,
        "h" => amount * 3_600.0,
        "d" => amount * 86_400.0,
        _ => return Err(format!("invalid PromQL duration unit '{unit}'")),
    };
    Ok((seconds * 1_000_000_000.0).round() as u64)
}

fn parse_prom_selector(input: &str) -> Result<PromSelector, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("unsupported PromQL: empty query".to_string());
    }
    if input.contains('(') || input.contains(')') || input.contains('[') || input.contains(']') {
        return Err(format!(
            "unsupported PromQL '{input}': only instant metric selectors are supported"
        ));
    }

    let (metric, matcher_text) = match input.find('{') {
        Some(open) => {
            if !input.ends_with('}') {
                return Err(format!(
                    "unsupported PromQL '{input}': selector braces are not balanced"
                ));
            }
            (&input[..open], Some(&input[open + 1..input.len() - 1]))
        }
        None => (input, None),
    };
    let metric = metric.trim();
    if !valid_metric_name(metric) {
        return Err(format!("unsupported PromQL '{input}': invalid metric name"));
    }
    let matchers = match matcher_text {
        Some(text) => parse_prom_label_matchers(text)?,
        None => Vec::new(),
    };
    Ok(PromSelector {
        metric: metric.to_string(),
        matchers,
    })
}

fn parse_prom_label_matchers(input: &str) -> Result<Vec<PromLabelMatcher>, String> {
    let mut matchers = Vec::new();
    let mut rest = input.trim();
    while !rest.is_empty() {
        let name_len = rest
            .char_indices()
            .take_while(|(_, ch)| *ch == '_' || ch.is_ascii_alphanumeric())
            .map(|(idx, ch)| idx + ch.len_utf8())
            .last()
            .unwrap_or(0);
        if name_len == 0 {
            return Err(format!("unsupported PromQL matcher near '{rest}'"));
        }
        let name = &rest[..name_len];
        if !valid_label_name(name) {
            return Err(format!(
                "unsupported PromQL matcher: invalid label name '{name}'"
            ));
        }
        rest = rest[name_len..].trim_start();

        let (op, after_op) = if rest.starts_with("=~") || rest.starts_with("!~") {
            return Err("unsupported PromQL matcher: regex matchers are not supported".to_string());
        } else if let Some(after) = rest.strip_prefix("!=") {
            (PromLabelMatcherOp::NotEqual, after)
        } else if let Some(after) = rest.strip_prefix('=') {
            (PromLabelMatcherOp::Equal, after)
        } else {
            return Err(format!(
                "unsupported PromQL matcher for label '{name}': expected = or !="
            ));
        };
        rest = after_op.trim_start();
        let (value, after_value) = parse_quoted_matcher_value(rest)?;
        rest = after_value.trim_start();
        matchers.push(PromLabelMatcher {
            name: name.to_string(),
            op,
            value,
        });

        if let Some(after_comma) = rest.strip_prefix(',') {
            rest = after_comma.trim_start();
            if rest.is_empty() {
                return Err("unsupported PromQL matcher: trailing comma".to_string());
            }
        } else if !rest.is_empty() {
            return Err(format!("unsupported PromQL matcher near '{rest}'"));
        }
    }
    Ok(matchers)
}

fn parse_quoted_matcher_value(input: &str) -> Result<(String, &str), String> {
    let mut chars = input.char_indices();
    match chars.next() {
        Some((_, '"')) => {}
        _ => return Err("unsupported PromQL matcher: value must be quoted".to_string()),
    }

    let mut value = String::new();
    let mut escaped = false;
    for (idx, ch) in chars {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Ok((value, &input[idx + ch.len_utf8()..])),
            other => value.push(other),
        }
    }
    Err("unsupported PromQL matcher: unterminated quoted value".to_string())
}

fn selector_matches(selector: &PromSelector, labels: &BTreeMap<String, String>) -> bool {
    selector.matchers.iter().all(|matcher| match matcher.op {
        PromLabelMatcherOp::Equal => labels
            .get(&matcher.name)
            .is_some_and(|value| value == &matcher.value),
        PromLabelMatcherOp::NotEqual => labels
            .get(&matcher.name)
            .is_none_or(|value| value != &matcher.value),
    })
}

fn prometheus_labels_for_point(point: &crate::storage::TimeSeriesData) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("__name__".to_string(), point.metric.clone());
    for (name, value) in &point.tags {
        if matches!(
            name.as_str(),
            "__tenant_id" | "__namespace" | "__reddb_kind"
        ) {
            continue;
        }
        labels.insert(name.clone(), value.clone());
    }
    labels
}

fn prometheus_vector_response(samples: Vec<PromVectorSample>) -> HttpResponse {
    let result = samples
        .into_iter()
        .map(|sample| {
            let mut object = Map::new();
            object.insert("metric".to_string(), string_map_json(sample.labels));
            object.insert(
                "value".to_string(),
                JsonValue::Array(vec![
                    crate::json!(sample.timestamp_ns as f64 / 1_000_000_000.0),
                    JsonValue::String(format_prometheus_value(sample.value)),
                ]),
            );
            JsonValue::Object(object)
        })
        .collect::<Vec<_>>();

    let mut data = Map::new();
    data.insert(
        "resultType".to_string(),
        JsonValue::String("vector".to_string()),
    );
    data.insert("result".to_string(), JsonValue::Array(result));

    let mut root = Map::new();
    root.insert(
        "status".to_string(),
        JsonValue::String("success".to_string()),
    );
    root.insert("data".to_string(), JsonValue::Object(data));
    json_response(200, JsonValue::Object(root))
}

fn align_range_values_to_steps(
    samples: &[PromRangeValue],
    range: PromQueryRange,
) -> Vec<PromRangeValue> {
    let mut aligned = Vec::new();
    let mut sample_index = 0;
    let mut latest = None;
    let mut step = range.start_ns;
    while step <= range.end_ns {
        while sample_index < samples.len() && samples[sample_index].timestamp_ns <= step {
            latest = Some(samples[sample_index]);
            sample_index += 1;
        }
        if let Some(sample) = latest {
            aligned.push(PromRangeValue {
                timestamp_ns: step,
                value: sample.value,
            });
        }
        match step.checked_add(range.step_ns) {
            Some(next) if next > step => step = next,
            _ => break,
        }
    }
    aligned
}

fn evaluate_counter_function(
    function: PromCounterFunction,
    samples: &[PromRangeValue],
    window_start_ns: u64,
    eval_time_ns: u64,
) -> Option<f64> {
    let window = samples
        .iter()
        .copied()
        .filter(|sample| {
            sample.timestamp_ns >= window_start_ns && sample.timestamp_ns <= eval_time_ns
        })
        .collect::<Vec<_>>();
    if window.len() < 2 {
        return None;
    }

    match function {
        PromCounterFunction::Increase => Some(counter_increase(&window)),
        PromCounterFunction::Rate => {
            let first = window.first()?;
            let last = window.last()?;
            let elapsed =
                (last.timestamp_ns.checked_sub(first.timestamp_ns)? as f64) / 1_000_000_000.0;
            if elapsed <= 0.0 {
                None
            } else {
                Some(counter_increase(&window) / elapsed)
            }
        }
        PromCounterFunction::IRate => {
            let previous = window[window.len() - 2];
            let last = window[window.len() - 1];
            let elapsed =
                (last.timestamp_ns.checked_sub(previous.timestamp_ns)? as f64) / 1_000_000_000.0;
            if elapsed <= 0.0 {
                return None;
            }
            let delta = if last.value >= previous.value {
                last.value - previous.value
            } else {
                last.value
            };
            Some(delta / elapsed)
        }
    }
}

fn counter_increase(samples: &[PromRangeValue]) -> f64 {
    samples
        .windows(2)
        .map(|pair| {
            let previous = pair[0].value;
            let current = pair[1].value;
            if current >= previous {
                current - previous
            } else {
                current
            }
        })
        .sum()
}

fn aggregate_vector(
    samples: Vec<PromVectorSample>,
    op: PromAggregateOp,
    grouping: PromGrouping,
) -> Vec<PromVectorSample> {
    let mut groups: BTreeMap<Vec<(String, String)>, (BTreeMap<String, String>, Vec<f64>, u64)> =
        BTreeMap::new();
    for sample in samples {
        let labels = aggregate_labels(&sample.labels, &grouping);
        let key = labels
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let timestamp = sample.timestamp_ns;
        let entry = groups
            .entry(key)
            .or_insert_with(|| (labels, Vec::new(), timestamp));
        entry.1.push(sample.value);
        entry.2 = entry.2.max(timestamp);
    }

    groups
        .into_values()
        .filter_map(|(labels, values, timestamp_ns)| {
            aggregate_values(&values, op).map(|value| PromVectorSample {
                labels,
                timestamp_ns,
                value,
            })
        })
        .collect()
}

fn aggregate_matrix(
    series: Vec<PromRangeSeries>,
    op: PromAggregateOp,
    grouping: PromGrouping,
) -> Vec<PromRangeSeries> {
    let mut groups: BTreeMap<
        Vec<(String, String)>,
        (BTreeMap<String, String>, BTreeMap<u64, Vec<f64>>),
    > = BTreeMap::new();
    for item in series {
        let labels = aggregate_labels(&item.labels, &grouping);
        let key = labels
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let entry = groups
            .entry(key)
            .or_insert_with(|| (labels, BTreeMap::new()));
        for value in item.values {
            entry
                .1
                .entry(value.timestamp_ns)
                .or_default()
                .push(value.value);
        }
    }

    groups
        .into_values()
        .map(|(labels, by_timestamp)| PromRangeSeries {
            labels,
            values: by_timestamp
                .into_iter()
                .filter_map(|(timestamp_ns, values)| {
                    aggregate_values(&values, op).map(|value| PromRangeValue {
                        timestamp_ns,
                        value,
                    })
                })
                .collect(),
        })
        .filter(|series| !series.values.is_empty())
        .collect()
}

fn aggregate_labels(
    labels: &BTreeMap<String, String>,
    grouping: &PromGrouping,
) -> BTreeMap<String, String> {
    match grouping {
        PromGrouping::None => BTreeMap::new(),
        PromGrouping::By(names) => names
            .iter()
            .filter_map(|name| labels.get(name).map(|value| (name.clone(), value.clone())))
            .collect(),
        PromGrouping::Without(names) => labels
            .iter()
            .filter(|(name, _)| name.as_str() != "__name__" && !names.contains(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    }
}

fn aggregate_values(values: &[f64], op: PromAggregateOp) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    match op {
        PromAggregateOp::Sum => Some(values.iter().sum()),
        PromAggregateOp::Avg => Some(values.iter().sum::<f64>() / values.len() as f64),
        PromAggregateOp::Min => values.iter().copied().reduce(f64::min),
        PromAggregateOp::Max => values.iter().copied().reduce(f64::max),
        PromAggregateOp::Count => Some(values.len() as f64),
    }
}

fn histogram_quantile_vector(
    quantile: f64,
    samples: Vec<PromVectorSample>,
) -> Vec<PromVectorSample> {
    let mut groups: BTreeMap<
        Vec<(String, String)>,
        (BTreeMap<String, String>, Vec<(f64, f64)>, u64),
    > = BTreeMap::new();
    for sample in samples {
        let Some(le) = sample
            .labels
            .get("le")
            .and_then(|value| parse_histogram_le(value))
        else {
            continue;
        };
        let labels = histogram_output_labels(&sample.labels);
        let key = labels
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let timestamp = sample.timestamp_ns;
        let entry = groups
            .entry(key)
            .or_insert_with(|| (labels, Vec::new(), timestamp));
        entry.1.push((le, sample.value));
        entry.2 = entry.2.max(timestamp);
    }

    groups
        .into_values()
        .filter_map(|(labels, mut buckets, timestamp_ns)| {
            histogram_quantile(quantile, &mut buckets).map(|value| PromVectorSample {
                labels,
                timestamp_ns,
                value,
            })
        })
        .collect()
}

fn histogram_quantile_matrix(quantile: f64, series: Vec<PromRangeSeries>) -> Vec<PromRangeSeries> {
    let mut groups: BTreeMap<
        Vec<(String, String)>,
        (BTreeMap<String, String>, BTreeMap<u64, Vec<(f64, f64)>>),
    > = BTreeMap::new();
    for item in series {
        let Some(le) = item
            .labels
            .get("le")
            .and_then(|value| parse_histogram_le(value))
        else {
            continue;
        };
        let labels = histogram_output_labels(&item.labels);
        let key = labels
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let entry = groups
            .entry(key)
            .or_insert_with(|| (labels, BTreeMap::new()));
        for value in item.values {
            entry
                .1
                .entry(value.timestamp_ns)
                .or_default()
                .push((le, value.value));
        }
    }

    groups
        .into_values()
        .map(|(labels, by_timestamp)| PromRangeSeries {
            labels,
            values: by_timestamp
                .into_iter()
                .filter_map(|(timestamp_ns, mut buckets)| {
                    histogram_quantile(quantile, &mut buckets).map(|value| PromRangeValue {
                        timestamp_ns,
                        value,
                    })
                })
                .collect(),
        })
        .filter(|series| !series.values.is_empty())
        .collect()
}

fn histogram_output_labels(labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    labels
        .iter()
        .filter(|(name, _)| name.as_str() != "__name__" && name.as_str() != "le")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn parse_histogram_le(value: &str) -> Option<f64> {
    if value.eq_ignore_ascii_case("+inf") || value.eq_ignore_ascii_case("inf") {
        Some(f64::INFINITY)
    } else {
        value.parse::<f64>().ok().filter(|v| v.is_finite())
    }
}

fn histogram_quantile(quantile: f64, buckets: &mut Vec<(f64, f64)>) -> Option<f64> {
    if buckets.is_empty() {
        return None;
    }
    buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total = buckets.last()?.1;
    if total <= 0.0 {
        return None;
    }
    let rank = quantile * total;
    let mut previous_bound = 0.0;
    let mut previous_count = 0.0;
    for (bound, count) in buckets.iter().copied() {
        if count < rank {
            if bound.is_finite() {
                previous_bound = bound;
            }
            previous_count = count;
            continue;
        }
        if !bound.is_finite() {
            return Some(previous_bound);
        }
        let bucket_count = count - previous_count;
        if bucket_count <= 0.0 {
            return Some(bound);
        }
        let fraction = (rank - previous_count) / bucket_count;
        return Some(previous_bound + (bound - previous_bound) * fraction.clamp(0.0, 1.0));
    }
    buckets
        .iter()
        .rev()
        .find_map(|(bound, _)| bound.is_finite().then_some(*bound))
}

fn apply_instant_arithmetic(
    left: PromInstantEval,
    right: PromInstantEval,
    op: PromArithmeticOp,
) -> RedDBResult<PromInstantEval> {
    match (left, right) {
        (PromInstantEval::Scalar(left), PromInstantEval::Scalar(right)) => {
            arithmetic_values(left, right, op).map(PromInstantEval::Scalar)
        }
        (PromInstantEval::Vector(samples), PromInstantEval::Scalar(scalar)) => Ok(
            PromInstantEval::Vector(apply_vector_scalar(samples, scalar, op, false)),
        ),
        (PromInstantEval::Scalar(scalar), PromInstantEval::Vector(samples)) => Ok(
            PromInstantEval::Vector(apply_vector_scalar(samples, scalar, op, true)),
        ),
        (PromInstantEval::Vector(_), PromInstantEval::Vector(_)) => Err(RedDBError::Query(
            "unsupported PromQL vector matching: only vector-scalar arithmetic is supported"
                .to_string(),
        )),
    }
}

fn apply_range_arithmetic(
    left: PromRangeEval,
    right: PromRangeEval,
    op: PromArithmeticOp,
) -> RedDBResult<PromRangeEval> {
    match (left, right) {
        (PromRangeEval::Scalar(left), PromRangeEval::Scalar(right)) => {
            arithmetic_values(left, right, op).map(PromRangeEval::Scalar)
        }
        (PromRangeEval::Matrix(series), PromRangeEval::Scalar(scalar)) => Ok(
            PromRangeEval::Matrix(apply_matrix_scalar(series, scalar, op, false)),
        ),
        (PromRangeEval::Scalar(scalar), PromRangeEval::Matrix(series)) => Ok(
            PromRangeEval::Matrix(apply_matrix_scalar(series, scalar, op, true)),
        ),
        (PromRangeEval::Matrix(_), PromRangeEval::Matrix(_)) => Err(RedDBError::Query(
            "unsupported PromQL vector matching: only vector-scalar arithmetic is supported"
                .to_string(),
        )),
    }
}

fn apply_vector_scalar(
    samples: Vec<PromVectorSample>,
    scalar: f64,
    op: PromArithmeticOp,
    scalar_on_left: bool,
) -> Vec<PromVectorSample> {
    samples
        .into_iter()
        .filter_map(|mut sample| {
            let value = if scalar_on_left {
                arithmetic_values(scalar, sample.value, op).ok()?
            } else {
                arithmetic_values(sample.value, scalar, op).ok()?
            };
            sample.value = value;
            Some(sample)
        })
        .collect()
}

fn apply_matrix_scalar(
    series: Vec<PromRangeSeries>,
    scalar: f64,
    op: PromArithmeticOp,
    scalar_on_left: bool,
) -> Vec<PromRangeSeries> {
    series
        .into_iter()
        .map(|mut item| {
            item.values = item
                .values
                .into_iter()
                .filter_map(|mut value| {
                    value.value = if scalar_on_left {
                        arithmetic_values(scalar, value.value, op).ok()?
                    } else {
                        arithmetic_values(value.value, scalar, op).ok()?
                    };
                    Some(value)
                })
                .collect();
            item
        })
        .filter(|item| !item.values.is_empty())
        .collect()
}

fn arithmetic_values(left: f64, right: f64, op: PromArithmeticOp) -> RedDBResult<f64> {
    let value = match op {
        PromArithmeticOp::Add => left + right,
        PromArithmeticOp::Sub => left - right,
        PromArithmeticOp::Mul => left * right,
        PromArithmeticOp::Div => {
            if right == 0.0 {
                return Err(RedDBError::Query(
                    "unsupported PromQL arithmetic: division by zero".to_string(),
                ));
            }
            left / right
        }
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(RedDBError::Query(
            "unsupported PromQL arithmetic produced a non-finite value".to_string(),
        ))
    }
}

fn prometheus_matrix_response(series: Vec<PromRangeSeries>) -> HttpResponse {
    let result = series
        .into_iter()
        .map(|series| {
            let mut object = Map::new();
            object.insert("metric".to_string(), string_map_json(series.labels));
            object.insert(
                "values".to_string(),
                JsonValue::Array(
                    series
                        .values
                        .into_iter()
                        .map(|value| {
                            JsonValue::Array(vec![
                                crate::json!(value.timestamp_ns as f64 / 1_000_000_000.0),
                                JsonValue::String(format_prometheus_value(value.value)),
                            ])
                        })
                        .collect(),
                ),
            );
            JsonValue::Object(object)
        })
        .collect::<Vec<_>>();

    let mut data = Map::new();
    data.insert(
        "resultType".to_string(),
        JsonValue::String("matrix".to_string()),
    );
    data.insert("result".to_string(), JsonValue::Array(result));

    let mut root = Map::new();
    root.insert(
        "status".to_string(),
        JsonValue::String("success".to_string()),
    );
    root.insert("data".to_string(), JsonValue::Object(data));
    json_response(200, JsonValue::Object(root))
}

fn prometheus_scalar_response(value: f64) -> HttpResponse {
    let mut data = Map::new();
    data.insert(
        "resultType".to_string(),
        JsonValue::String("scalar".to_string()),
    );
    data.insert(
        "result".to_string(),
        JsonValue::Array(vec![
            crate::json!(0.0),
            JsonValue::String(format_prometheus_value(value)),
        ]),
    );

    let mut root = Map::new();
    root.insert(
        "status".to_string(),
        JsonValue::String("success".to_string()),
    );
    root.insert("data".to_string(), JsonValue::Object(data));
    json_response(200, JsonValue::Object(root))
}

fn prometheus_query_error_response(err: RedDBError) -> HttpResponse {
    match err {
        RedDBError::Query(message) => prometheus_error_response(422, "bad_data", message),
        other => prometheus_error_response(500, "internal", other.to_string()),
    }
}

fn prometheus_error_response(
    status: u16,
    error_type: &str,
    error: impl Into<String>,
) -> HttpResponse {
    let mut root = Map::new();
    root.insert("status".to_string(), JsonValue::String("error".to_string()));
    root.insert(
        "errorType".to_string(),
        JsonValue::String(error_type.to_string()),
    );
    root.insert("error".to_string(), JsonValue::String(error.into()));
    json_response(status, JsonValue::Object(root))
}

fn string_map_json(map: BTreeMap<String, String>) -> JsonValue {
    JsonValue::Object(
        map.into_iter()
            .map(|(name, value)| (name, JsonValue::String(value)))
            .collect(),
    )
}

fn format_prometheus_value(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            b'%' => {
                if idx + 2 >= bytes.len() {
                    return Err("truncated percent escape".to_string());
                }
                let high = hex_value(bytes[idx + 1])
                    .ok_or_else(|| "invalid percent escape".to_string())?;
                let low = hex_value(bytes[idx + 2])
                    .ok_or_else(|| "invalid percent escape".to_string())?;
                out.push((high << 4) | low);
                idx += 3;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|err| err.to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_remote_write_headers(headers: &BTreeMap<String, String>) -> RedDBResult<()> {
    let content_encoding = headers
        .get("content-encoding")
        .map(String::as_str)
        .unwrap_or_default();
    if !content_encoding.eq_ignore_ascii_case("snappy") {
        return Err(RedDBError::Query(
            "remote_write requires Content-Encoding: snappy".to_string(),
        ));
    }

    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or_default();
    if !content_type
        .to_ascii_lowercase()
        .starts_with("application/x-protobuf")
    {
        return Err(RedDBError::Query(
            "remote_write requires Content-Type: application/x-protobuf".to_string(),
        ));
    }

    let version = headers
        .get("x-prometheus-remote-write-version")
        .map(String::as_str)
        .unwrap_or_default();
    if version != "0.1.0" {
        return Err(RedDBError::Query(
            "remote_write requires X-Prometheus-Remote-Write-Version: 0.1.0".to_string(),
        ));
    }
    Ok(())
}

fn resolve_metrics_collection(
    runtime: &RedDBRuntime,
    query: &BTreeMap<String, String>,
) -> RedDBResult<String> {
    if let Some(collection) = query.get("collection") {
        return Ok(collection.clone());
    }
    let metrics = runtime
        .db()
        .collection_contracts()
        .into_iter()
        .filter(|contract| contract.declared_model == CollectionModel::Metrics)
        .map(|contract| contract.name)
        .collect::<Vec<_>>();
    match metrics.as_slice() {
        [collection] => Ok(collection.clone()),
        [] => Err(RedDBError::Query(
            "remote_write requires a metrics collection; pass ?collection=<name>".to_string(),
        )),
        _ => Err(RedDBError::Query(
            "remote_write requires ?collection=<name> when multiple metrics collections exist"
                .to_string(),
        )),
    }
}

fn decode_metric_batch(
    runtime: &RedDBRuntime,
    collection: &str,
    contract: &crate::physical::CollectionContract,
    request: PromWriteRequest,
) -> RedDBResult<DecodedMetricBatch> {
    let tenant = crate::runtime::impl_core::current_tenant().unwrap_or_else(|| "default".into());
    let namespace = contract
        .metrics_namespace
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut entities = Vec::new();
    let mut accepted_series = 0_u64;
    let mut accepted_samples = 0_u64;
    let mut rejected_series = 0_u64;
    let mut rejected_samples = 0_u64;
    let mut cardinality_budget_rejected_series = 0_u64;
    let series_budget = metrics_series_budget_per_metric();
    let mut admitted_counts: HashMap<String, usize> = HashMap::new();
    let mut known_series = existing_metrics_series_keys(runtime, collection);

    for series in request.timeseries {
        if !series.histograms.is_empty() {
            return Err(RedDBError::Query(
                "remote_write native histograms are not supported in metrics v0".to_string(),
            ));
        }
        let (metric, mut tags) = decode_labels(series.labels)?;
        tags.insert("__tenant_id".to_string(), tenant.clone());
        tags.insert("__namespace".to_string(), namespace.clone());
        let kind = if metric.ends_with("_total")
            || metric.ends_with("_bucket")
            || metric.ends_with("_sum")
            || metric.ends_with("_count")
        {
            "counter"
        } else {
            "gauge"
        };
        tags.insert("__reddb_kind".to_string(), kind.to_string());

        let series_key = metrics_series_key(&metric, &tags);
        let budget_key = format!("{tenant}\n{namespace}\n{metric}");
        let is_new_series = !known_series.contains(&series_key);
        if is_new_series && series_budget.is_some() {
            let current = match admitted_counts.get(&budget_key).copied() {
                Some(count) => count,
                None => {
                    let count = known_series
                        .iter()
                        .filter(|key| {
                            key.starts_with(&format!("{metric}\n"))
                                && key.contains(&format!("__tenant_id={tenant}"))
                                && key.contains(&format!("__namespace={namespace}"))
                        })
                        .count();
                    admitted_counts.insert(budget_key.clone(), count);
                    count
                }
            };
            let budget = series_budget.expect("checked is_some");
            if current >= budget {
                rejected_series += 1;
                rejected_samples += series.samples.len() as u64;
                cardinality_budget_rejected_series += 1;
                continue;
            }
            admitted_counts.insert(budget_key, current + 1);
            known_series.insert(series_key);
        }

        if !series.samples.is_empty() {
            accepted_series += 1;
        }
        for sample in series.samples {
            if sample.timestamp < 0 {
                return Err(RedDBError::Query(
                    "remote_write sample timestamp must be non-negative".to_string(),
                ));
            }
            if !sample.value.is_finite() {
                return Err(RedDBError::Query(
                    "remote_write sample value must be finite".to_string(),
                ));
            }
            let timestamp_ns = (sample.timestamp as u64)
                .checked_mul(1_000_000)
                .ok_or_else(|| {
                    RedDBError::Query(
                        "remote_write sample timestamp overflows nanoseconds".to_string(),
                    )
                })?;
            let entity = UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TimeSeriesPoint(Box::new(crate::storage::TimeSeriesPointKind {
                    series: collection.to_string(),
                    metric: metric.clone(),
                })),
                EntityData::TimeSeries(crate::storage::TimeSeriesData {
                    metric: metric.clone(),
                    timestamp_ns,
                    value: sample.value,
                    tags: tags.clone(),
                }),
            );
            entities.push(entity);
            accepted_samples += 1;
        }
    }

    Ok(DecodedMetricBatch {
        entities,
        accepted_series,
        accepted_samples,
        rejected_series,
        rejected_samples,
        cardinality_budget_rejected_series,
    })
}

fn metrics_series_budget_per_metric() -> Option<usize> {
    std::env::var("REDDB_METRICS_MAX_SERIES_PER_METRIC")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn existing_metrics_series_keys(runtime: &RedDBRuntime, collection: &str) -> HashSet<String> {
    let Some(manager) = runtime.db().store().get_collection(collection) else {
        return HashSet::new();
    };
    manager
        .query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)))
        .into_iter()
        .filter_map(|entity| match entity.data {
            EntityData::TimeSeries(point) => Some(metrics_series_key(&point.metric, &point.tags)),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct MetricsRollupPolicy {
    target: String,
    aggregation: String,
    bucket_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RollupKey {
    metric: String,
    bucket_ns: u64,
    tags: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct RollupAccumulator {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl RollupAccumulator {
    fn new(value: f64) -> Self {
        Self {
            count: 1,
            sum: value,
            min: value,
            max: value,
        }
    }

    fn push(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    fn value(&self, aggregation: &str) -> f64 {
        match aggregation {
            "sum" => self.sum,
            "min" => self.min,
            "max" => self.max,
            "count" => self.count as f64,
            _ => self.sum / self.count.max(1) as f64,
        }
    }
}

fn materialize_metrics_rollups(
    runtime: &RedDBRuntime,
    raw_collection: &str,
    contract: &crate::physical::CollectionContract,
) -> RedDBResult<()> {
    let policies = metrics_rollup_policies(contract);
    if policies.is_empty() {
        return Ok(());
    }

    let store = runtime.db().store();
    let Some(raw_manager) = store.get_collection(raw_collection) else {
        return Ok(());
    };
    let raw_points =
        raw_manager.query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));

    for policy in policies {
        let rollup_collection = metrics_rollup_collection(raw_collection, &policy.target);
        if store.get_collection(&rollup_collection).is_none() {
            store
                .create_collection(&rollup_collection)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        if let Some(manager) = store.get_collection(&rollup_collection) {
            for entity in manager.query_all(|_| true) {
                store
                    .delete(&rollup_collection, entity.id)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }
        }

        let mut buckets = BTreeMap::<RollupKey, RollupAccumulator>::new();
        for entity in &raw_points {
            let EntityData::TimeSeries(point) = &entity.data else {
                continue;
            };
            let bucket_ns = (point.timestamp_ns / policy.bucket_ns) * policy.bucket_ns;
            let mut tags = point
                .tags
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Vec<_>>();
            tags.sort();
            let key = RollupKey {
                metric: point.metric.clone(),
                bucket_ns,
                tags,
            };
            buckets
                .entry(key)
                .and_modify(|acc| acc.push(point.value))
                .or_insert_with(|| RollupAccumulator::new(point.value));
        }

        let mut entities = Vec::new();
        for (key, accumulator) in buckets {
            let tags = key.tags.into_iter().collect::<HashMap<_, _>>();
            entities.push(UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TimeSeriesPoint(Box::new(crate::storage::TimeSeriesPointKind {
                    series: rollup_collection.clone(),
                    metric: key.metric.clone(),
                })),
                EntityData::TimeSeries(crate::storage::TimeSeriesData {
                    metric: key.metric,
                    timestamp_ns: key.bucket_ns,
                    value: accumulator.value(&policy.aggregation),
                    tags,
                }),
            ));
        }
        if !entities.is_empty() {
            store
                .bulk_insert(&rollup_collection, entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
    }

    Ok(())
}

fn select_metrics_range_collection(
    store: &crate::storage::unified::UnifiedStore,
    contract: &crate::physical::CollectionContract,
    range: PromQueryRange,
) -> String {
    let Some(policy) = metrics_rollup_policies(contract)
        .into_iter()
        .filter(|policy| policy.bucket_ns <= range.step_ns)
        .max_by_key(|policy| policy.bucket_ns)
    else {
        return contract.name.clone();
    };
    let rollup_collection = metrics_rollup_collection(&contract.name, &policy.target);
    if store.get_collection(&rollup_collection).is_some() {
        rollup_collection
    } else {
        contract.name.clone()
    }
}

fn metrics_rollup_policies(
    contract: &crate::physical::CollectionContract,
) -> Vec<MetricsRollupPolicy> {
    contract
        .metrics_rollup_policies
        .iter()
        .filter_map(|spec| {
            let parsed = DownsamplePolicy::parse(spec)?;
            if parsed.source != "raw"
                || !is_supported_metrics_rollup_aggregation(&parsed.aggregation)
            {
                return None;
            }
            Some(MetricsRollupPolicy {
                target: parsed.target,
                aggregation: parsed.aggregation,
                bucket_ns: parsed.bucket_ns,
            })
        })
        .collect()
}

fn is_supported_metrics_rollup_aggregation(aggregation: &str) -> bool {
    matches!(aggregation, "avg" | "sum" | "min" | "max" | "count")
}

fn metrics_rollup_collection(raw_collection: &str, target: &str) -> String {
    let sanitized = target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("red_metrics_rollup_{raw_collection}_{sanitized}")
}

fn metrics_series_key(metric: &str, tags: &HashMap<String, String>) -> String {
    let mut parts = tags
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();
    parts.sort();
    format!("{metric}\n{}", parts.join("\n"))
}

fn decode_labels(labels: Vec<PromLabel>) -> RedDBResult<(String, HashMap<String, String>)> {
    let mut seen = HashSet::new();
    let mut metric = None;
    let mut tags = HashMap::new();

    for label in labels {
        if label.name.is_empty() {
            return Err(RedDBError::Query(
                "remote_write label name cannot be empty".to_string(),
            ));
        }
        if !seen.insert(label.name.clone()) {
            return Err(RedDBError::Query(format!(
                "remote_write duplicate label name '{}'",
                label.name
            )));
        }
        if label.name == "__name__" {
            if !valid_metric_name(&label.value) {
                return Err(RedDBError::Query(format!(
                    "remote_write invalid metric name '{}'",
                    label.value
                )));
            }
            metric = Some(label.value);
        } else {
            if !valid_label_name(&label.name) {
                return Err(RedDBError::Query(format!(
                    "remote_write invalid label name '{}'",
                    label.name
                )));
            }
            tags.insert(label.name, label.value);
        }
    }

    let metric = metric.ok_or_else(|| {
        RedDBError::Query("remote_write series requires __name__ label".to_string())
    })?;
    Ok((metric, tags))
}

fn valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first == ':' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == ':' || ch.is_ascii_alphanumeric())
}

fn valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
