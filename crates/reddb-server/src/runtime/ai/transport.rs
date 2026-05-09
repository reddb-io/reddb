//! Shared async HTTP transport foundation for AI providers.
//!
//! The provider modules still own request/response shaping. This module
//! centralises the outbound HTTP client, retry policy, and contextual
//! errors so providers can migrate off ad hoc blocking calls incrementally.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use crate::runtime::RedDBRuntime;

pub const CONFIG_POOL_SIZE: &str = "runtime.ai.transport_pool_size";
pub const CONFIG_TIMEOUT_MS: &str = "runtime.ai.transport_timeout_ms";
pub const CONFIG_RETRY_MAX_ATTEMPTS: &str = "runtime.ai.transport_retry_max_attempts";
pub const CONFIG_RETRY_BASE_MS: &str = "runtime.ai.transport_retry_base_ms";

pub const DEFAULT_POOL_SIZE: usize = 16;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_RETRY_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_RETRY_BASE_MS: u64 = 500;
pub const DEFAULT_RETRY_CAP_MS: u64 = 10_000;

#[derive(Debug, Clone)]
pub struct AiTransportConfig {
    pub pool_size: usize,
    pub timeout: Duration,
    pub retry: AiRetryConfig,
}

impl Default for AiTransportConfig {
    fn default() -> Self {
        Self {
            pool_size: DEFAULT_POOL_SIZE,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            retry: AiRetryConfig::default(),
        }
    }
}

impl AiTransportConfig {
    pub fn from_runtime(runtime: &RedDBRuntime) -> Self {
        let defaults = Self::default();
        Self {
            pool_size: runtime.config_u64(CONFIG_POOL_SIZE, defaults.pool_size as u64) as usize,
            timeout: Duration::from_millis(
                runtime.config_u64(CONFIG_TIMEOUT_MS, DEFAULT_TIMEOUT_MS),
            ),
            retry: AiRetryConfig {
                max_attempts: runtime
                    .config_u64(CONFIG_RETRY_MAX_ATTEMPTS, DEFAULT_RETRY_MAX_ATTEMPTS as u64)
                    as u32,
                base_delay: Duration::from_millis(
                    runtime.config_u64(CONFIG_RETRY_BASE_MS, DEFAULT_RETRY_BASE_MS),
                ),
                max_delay: defaults.retry.max_delay,
            },
        }
        .normalized()
    }

    pub fn normalized(mut self) -> Self {
        self.pool_size = self.pool_size.max(1);
        if self.timeout.is_zero() {
            self.timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
        }
        self.retry = self.retry.normalized();
        self
    }
}

#[derive(Debug, Clone)]
pub struct AiRetryConfig {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for AiRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            base_delay: Duration::from_millis(DEFAULT_RETRY_BASE_MS),
            max_delay: Duration::from_millis(DEFAULT_RETRY_CAP_MS),
        }
    }
}

impl AiRetryConfig {
    pub fn normalized(mut self) -> Self {
        self.max_attempts = self.max_attempts.max(1);
        if self.base_delay.is_zero() {
            self.base_delay = Duration::from_millis(DEFAULT_RETRY_BASE_MS);
        }
        if self.max_delay < self.base_delay {
            self.max_delay = self.base_delay;
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiHttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone)]
pub struct AiHttpRequest {
    pub provider: String,
    pub model: Option<String>,
    pub method: AiHttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl AiHttpRequest {
    pub fn post_json(provider: impl Into<String>, url: impl Into<String>, body: String) -> Self {
        Self {
            provider: provider.into(),
            model: None,
            method: AiHttpMethod::Post,
            url: url.into(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "application/json".to_string()),
            ],
            body: Some(body),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiHttpResponse {
    pub status_code: u16,
    pub body: String,
    pub attempt_count: u32,
    pub total_wait_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTransportError {
    pub provider: String,
    pub status_code: Option<u16>,
    pub attempt_count: u32,
    pub total_wait_ms: u64,
    pub message: String,
}

impl fmt::Display for AiTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AI transport error provider={} status_code={} attempt_count={} total_wait_ms={}: {}",
            self.provider,
            self.status_code
                .map(|status| status.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.attempt_count,
            self.total_wait_ms,
            self.message
        )
    }
}

impl std::error::Error for AiTransportError {}

#[derive(Clone)]
pub struct AiTransport {
    agent: ureq::Agent,
    config: AiTransportConfig,
}

impl AiTransport {
    pub fn new(config: AiTransportConfig) -> Self {
        let config = config.normalized();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_idle_connections(config.pool_size)
            .max_idle_connections_per_host(config.pool_size)
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent, config }
    }

    pub fn from_runtime(runtime: &RedDBRuntime) -> Self {
        Self::new(AiTransportConfig::from_runtime(runtime))
    }

    pub fn config(&self) -> &AiTransportConfig {
        &self.config
    }

    pub async fn request(
        &self,
        request: AiHttpRequest,
    ) -> Result<AiHttpResponse, AiTransportError> {
        let mut attempt = 0;
        let mut total_wait = Duration::ZERO;
        let started = Instant::now();
        let provider = request.provider.clone();
        let model = request
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or("unknown")
            .to_string();

        loop {
            attempt += 1;
            match self.try_request_once(request.clone()).await {
                Ok(mut response) if response.status_code < 400 => {
                    let duration_ms = millis_u64(started.elapsed());
                    crate::runtime::ai::metrics::record_provider_request(
                        &provider,
                        &model,
                        "ok",
                        duration_ms,
                    );
                    response.attempt_count = attempt;
                    response.total_wait_ms = millis_u64(total_wait);
                    return Ok(response);
                }
                Ok(response) => {
                    let status_code = Some(response.status_code);
                    let message = format!("HTTP status {}", response.status_code);
                    let retryable = is_retryable_status(response.status_code);
                    let error = AiTransportError {
                        provider: request.provider.clone(),
                        status_code,
                        attempt_count: attempt,
                        total_wait_ms: millis_u64(total_wait),
                        message,
                    };
                    if !retryable || attempt >= self.config.retry.max_attempts {
                        let status = http_status_label(response.status_code);
                        crate::runtime::ai::metrics::record_provider_request(
                            &provider,
                            &model,
                            status,
                            millis_u64(started.elapsed()),
                        );
                        tracing::warn!(
                            target: "reddb::developer",
                            provider = %provider,
                            model = %model,
                            status_code = response.status_code,
                            attempt_count = attempt,
                            total_wait_ms = millis_u64(total_wait),
                            "ai provider request failed"
                        );
                        return Err(error);
                    }
                    let reason = retry_reason_for_status(response.status_code);
                    crate::runtime::ai::metrics::record_provider_retry(&provider, reason);
                    tracing::debug!(
                        target: "reddb::developer",
                        provider = %provider,
                        model = %model,
                        status_code = response.status_code,
                        attempt_count = attempt,
                        reason = reason,
                        "ai provider request retry scheduled"
                    );
                }
                Err(error) => {
                    let retryable = error.retryable;
                    let error = AiTransportError {
                        provider: request.provider.clone(),
                        status_code: None,
                        attempt_count: attempt,
                        total_wait_ms: millis_u64(total_wait),
                        message: error.message,
                    };
                    if !retryable || attempt >= self.config.retry.max_attempts {
                        crate::runtime::ai::metrics::record_provider_request(
                            &provider,
                            &model,
                            "transport_error",
                            millis_u64(started.elapsed()),
                        );
                        tracing::warn!(
                            target: "reddb::developer",
                            provider = %provider,
                            model = %model,
                            status_code = tracing::field::Empty,
                            attempt_count = attempt,
                            total_wait_ms = millis_u64(total_wait),
                            "ai provider request failed"
                        );
                        return Err(error);
                    }
                    crate::runtime::ai::metrics::record_provider_retry(
                        &provider,
                        "transport_error",
                    );
                    tracing::debug!(
                        target: "reddb::developer",
                        provider = %provider,
                        model = %model,
                        attempt_count = attempt,
                        reason = "transport_error",
                        "ai provider request retry scheduled"
                    );
                }
            }

            let delay = backoff_delay(&self.config.retry, attempt);
            total_wait += delay;
            tokio::time::sleep(delay).await;
        }
    }

    async fn try_request_once(
        &self,
        request: AiHttpRequest,
    ) -> Result<AiHttpResponse, TransportAttemptError> {
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || send_blocking(agent, request))
            .await
            .map_err(|err| TransportAttemptError {
                retryable: false,
                message: format!("request worker failed: {err}"),
            })?
    }
}

#[derive(Debug)]
struct TransportAttemptError {
    retryable: bool,
    message: String,
}

fn send_blocking(
    agent: ureq::Agent,
    request: AiHttpRequest,
) -> Result<AiHttpResponse, TransportAttemptError> {
    let result = match request.method {
        AiHttpMethod::Get => {
            let mut builder = agent.get(&request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            builder.call()
        }
        AiHttpMethod::Post => {
            let mut builder = agent.post(&request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            builder.send(request.body.unwrap_or_default())
        }
    };

    match result {
        Ok(mut response) => {
            let status_code = response.status().as_u16();
            let body =
                response
                    .body_mut()
                    .read_to_string()
                    .map_err(|err| TransportAttemptError {
                        retryable: is_retryable_ureq_error(&err),
                        message: format!("failed to read response body: {err}"),
                    })?;
            Ok(AiHttpResponse {
                status_code,
                body,
                attempt_count: 1,
                total_wait_ms: 0,
            })
        }
        Err(err) => Err(TransportAttemptError {
            retryable: is_retryable_ureq_error(&err),
            message: err.to_string(),
        }),
    }
}

fn backoff_delay(config: &AiRetryConfig, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(31);
    let multiplier = 1u32 << shift;
    config
        .base_delay
        .saturating_mul(multiplier)
        .min(config.max_delay)
}

fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn retry_reason_for_status(status: u16) -> &'static str {
    if status == 429 {
        "http_429"
    } else if (500..=599).contains(&status) {
        "http_5xx"
    } else {
        "http_error"
    }
}

fn http_status_label(status: u16) -> &'static str {
    if status == 429 {
        "http_429"
    } else if (400..=499).contains(&status) {
        "http_4xx"
    } else if (500..=599).contains(&status) {
        "http_5xx"
    } else {
        "http_error"
    }
}

fn is_retryable_ureq_error(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed => true,
        ureq::Error::Io(err) => is_retryable_io_error(err),
        _ => false,
    }
}

fn is_retryable_io_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
    )
}

fn millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
