//! Prometheus metrics exporter (MVP 2+3).
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGaugeVec, Opts, Registry, TextEncoder,
};

fn register_all(
    registry: &Registry,
) -> (
    IntCounterVec,
    IntCounterVec,
    IntCounterVec,
    IntCounter,
    IntCounter,
    HistogramVec,
    IntGaugeVec,
    IntGaugeVec,
    IntGaugeVec,
) {
    let gateway_requests_total = IntCounterVec::new(
        Opts::new("gateway_requests_total", "Total requests"),
        &["model", "provider", "tenant", "role", "stream"],
    )
    .unwrap();

    let gateway_tokens_total = IntCounterVec::new(
        Opts::new("gateway_tokens_total", "Total tokens"),
        &["model", "provider", "tenant", "kind"],
    )
    .unwrap();

    let gateway_errors_total = IntCounterVec::new(
        Opts::new("gateway_errors_total", "Total errors"),
        &["model", "provider", "error_type"],
    )
    .unwrap();

    let gateway_cache_hits_total =
        IntCounter::new("gateway_cache_hits_total", "Total cache hits").unwrap();

    let gateway_cache_misses_total =
        IntCounter::new("gateway_cache_misses_total", "Total cache misses").unwrap();

    let gateway_request_duration_seconds = HistogramVec::new(
        HistogramOpts::new(
            "gateway_request_duration_seconds",
            "Request duration in seconds",
        )
        .buckets(vec![
            0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
        ]),
        &["model", "provider"],
    )
    .unwrap();

    let gateway_active_requests = IntGaugeVec::new(
        Opts::new("gateway_active_requests", "In-flight requests"),
        &["provider", "tenant"],
    )
    .unwrap();

    let gateway_circuit_breaker_state = IntGaugeVec::new(
        Opts::new(
            "gateway_circuit_breaker_state",
            "Circuit breaker state (0=closed,1=open,2=half)",
        ),
        &["provider"],
    )
    .unwrap();

    let gateway_rate_limit_remaining = IntGaugeVec::new(
        Opts::new(
            "gateway_rate_limit_remaining",
            "Remaining rate-limit tokens",
        ),
        &["tenant"],
    )
    .unwrap();

    registry
        .register(Box::new(gateway_requests_total.clone()))
        .expect("register requests_total");
    registry
        .register(Box::new(gateway_tokens_total.clone()))
        .expect("register tokens_total");
    registry
        .register(Box::new(gateway_errors_total.clone()))
        .expect("register errors_total");
    registry
        .register(Box::new(gateway_cache_hits_total.clone()))
        .expect("register cache_hits");
    registry
        .register(Box::new(gateway_cache_misses_total.clone()))
        .expect("register cache_misses");
    registry
        .register(Box::new(gateway_request_duration_seconds.clone()))
        .expect("register duration");
    registry
        .register(Box::new(gateway_active_requests.clone()))
        .expect("register active_reqs");
    registry
        .register(Box::new(gateway_circuit_breaker_state.clone()))
        .expect("register circuit_breaker");
    registry
        .register(Box::new(gateway_rate_limit_remaining.clone()))
        .expect("register rate_limit_rem");

    (
        gateway_requests_total,
        gateway_tokens_total,
        gateway_errors_total,
        gateway_cache_hits_total,
        gateway_cache_misses_total,
        gateway_request_duration_seconds,
        gateway_active_requests,
        gateway_circuit_breaker_state,
        gateway_rate_limit_remaining,
    )
}

#[allow(dead_code)]
pub struct PrometheusExporter {
    registry: Registry,
    requests_total: IntCounterVec,
    tokens_total: IntCounterVec,
    errors_total: IntCounterVec,
    cache_hits_total: IntCounter,
    cache_misses_total: IntCounter,
    request_duration_seconds: HistogramVec,
    active_requests: IntGaugeVec,
    circuit_breaker_state: IntGaugeVec,
    rate_limit_remaining: IntGaugeVec,
}

impl PrometheusExporter {
    pub fn new() -> Self {
        let registry = Registry::new();
        let (
            requests_total,
            tokens_total,
            errors_total,
            cache_hits_total,
            cache_misses_total,
            request_duration_seconds,
            active_requests,
            circuit_breaker_state,
            rate_limit_remaining,
        ) = register_all(&registry);

        // Prime vec metrics with default labels so they render even at zero
        requests_total
            .with_label_values(&["_", "_", "_", "_", "false"])
            .inc_by(0);
        tokens_total
            .with_label_values(&["_", "_", "_", "_"])
            .inc_by(0);
        errors_total.with_label_values(&["_", "_", "_"]).inc_by(0);
        request_duration_seconds
            .with_label_values(&["_", "_"])
            .observe(0.0);
        active_requests.with_label_values(&["_", "_"]).set(0);
        circuit_breaker_state.with_label_values(&["_"]).set(0);
        rate_limit_remaining.with_label_values(&["_"]).set(0);

        Self {
            registry,
            requests_total,
            tokens_total,
            errors_total,
            cache_hits_total,
            cache_misses_total,
            request_duration_seconds,
            active_requests,
            circuit_breaker_state,
            rate_limit_remaining,
        }
    }

    pub fn record_request(
        &self,
        model: &str,
        provider: &str,
        tenant: &str,
        role: &str,
        stream: bool,
    ) {
        let s = if stream { "true" } else { "false" };
        self.requests_total
            .with_label_values(&[model, provider, tenant, role, s])
            .inc();
    }

    #[allow(dead_code)]
    pub fn record_tokens(
        &self,
        model: &str,
        provider: &str,
        tenant: &str,
        kind: &str,
        amount: u64,
    ) {
        self.tokens_total
            .with_label_values(&[model, provider, tenant, kind])
            .inc_by(amount);
    }

    #[allow(dead_code)]
    pub fn record_error(&self, model: &str, provider: &str, error_type: &str) {
        self.errors_total
            .with_label_values(&[model, provider, error_type])
            .inc();
    }

    #[allow(dead_code)]
    pub fn record_cache_hit(&self) {
        self.cache_hits_total.inc();
    }
    #[allow(dead_code)]
    pub fn record_cache_miss(&self) {
        self.cache_misses_total.inc();
    }

    #[allow(dead_code)]
    pub fn record_duration(&self, model: &str, provider: &str, secs: f64) {
        self.request_duration_seconds
            .with_label_values(&[model, provider])
            .observe(secs);
    }

    #[allow(dead_code)]
    pub fn set_active_requests(&self, provider: &str, tenant: &str, count: i64) {
        self.active_requests
            .with_label_values(&[provider, tenant])
            .set(count);
    }

    #[allow(dead_code)]
    pub fn set_circuit_breaker_state(&self, provider: &str, state: i64) {
        self.circuit_breaker_state
            .with_label_values(&[provider])
            .set(state);
    }

    #[allow(dead_code)]
    pub fn set_rate_limit_remaining(&self, tenant: &str, remaining: i64) {
        self.rate_limit_remaining
            .with_label_values(&[tenant])
            .set(remaining);
    }

    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buf = Vec::new();
        encoder
            .encode(&metric_families, &mut buf)
            .expect("encode metrics");
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for PrometheusExporter {
    fn default() -> Self {
        Self::new()
    }
}
