use super::*;
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
}

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
}

#[derive(Debug)]
struct PromSelector {
    metric: String,
    matchers: Vec<PromLabelMatcher>,
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
        let selector = match parse_prom_selector(&raw_query) {
            Ok(selector) => selector,
            Err(err) => return prometheus_error_response(422, "bad_data", err),
        };

        match self.prometheus_instant_vector(selector) {
            Ok(samples) => prometheus_vector_response(samples),
            Err(err) => prometheus_error_response(500, "internal", err.to_string()),
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
        let selector = match parse_prom_selector(&raw_query) {
            Ok(selector) => selector,
            Err(err) => return prometheus_error_response(422, "bad_data", err),
        };
        let range = match parse_prom_query_range(query, body.as_deref()) {
            Ok(range) => range,
            Err(err) => return prometheus_error_response(400, "bad_data", err),
        };

        match self.prometheus_range_matrix(selector, range) {
            Ok(series) => prometheus_matrix_response(series),
            Err(err) => prometheus_error_response(500, "internal", err.to_string()),
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
        let batch = match decode_metric_batch(&collection, &contract, request) {
            Ok(batch) => batch,
            Err(err) => {
                self.runtime
                    .record_metrics_ingest(0, 0, rejected_samples, rejected_series);
                return Err(err);
            }
        };
        if batch.entities.is_empty() {
            return Ok(());
        }

        self.runtime
            .db()
            .store()
            .bulk_insert(&collection, batch.entities)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.runtime
            .record_metrics_ingest(batch.accepted_samples, batch.accepted_series, 0, 0);
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

    for series in request.timeseries {
        let (metric, mut tags) = decode_labels(series.labels)?;
        tags.insert("__tenant_id".to_string(), tenant.clone());
        tags.insert("__namespace".to_string(), namespace.clone());
        let kind = if metric.ends_with("_total") {
            "counter"
        } else {
            "gauge"
        };
        tags.insert("__reddb_kind".to_string(), kind.to_string());

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
    })
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
