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

impl RedDBServer {
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
