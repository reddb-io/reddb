//! One discovery and routing lifecycle for all remote transports.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reddb_wire::routing::routing_endpoint;
use reddb_wire::sql_routing::{sql_route, SqlRoute};
use reddb_wire::{ConnectionAuth, ConnectionTarget};
use tokio::sync::Mutex;

use crate::bookmark_routing::{QueryOptions, ReadConsistency};
use crate::error::{ClientError, ErrorCode, Result};
use crate::params::Value;
use crate::redwire::{Auth, ConnectOptions, RedWireClient};
use crate::router::{HealthAwareRouter, Outcome};
use crate::topology::ClusterMembership;
use crate::transport::{self, DynTransport, Transport};
use crate::types::{BulkInsertResult, InsertResult, JsonValue, KvWatchEvent, QueryResult};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
type SharedTransport = Arc<dyn Transport>;

#[derive(Clone)]
struct ConnectionTemplate {
    protocol: String,
    auth: ConnectionAuth,
    #[cfg(feature = "redwire-tls")]
    tls: crate::redwire::TlsConfig,
}

impl std::fmt::Debug for ConnectionTemplate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionTemplate")
            .field("protocol", &self.protocol)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl ConnectionTemplate {
    async fn open(&self, address: &str) -> Result<SharedTransport> {
        let target = crate::connect::parse(address)?;
        match target {
            #[cfg(feature = "grpc")]
            ConnectionTarget::Grpc { endpoint } => {
                #[cfg(feature = "redwire-tls")]
                let tls = if self.protocol == "grpcs" {
                    let mut config = tonic::transport::ClientTlsConfig::new().with_webpki_roots();
                    if let Some(ca) = &self.tls.ca_pem {
                        config = config.ca_certificate(tonic::transport::Certificate::from_pem(ca));
                    }
                    if let (Some(cert), Some(key)) =
                        (&self.tls.client_cert_pem, &self.tls.client_key_pem)
                    {
                        config = config.identity(tonic::transport::Identity::from_pem(cert, key));
                    }
                    if let Some(servername) = &self.tls.servername {
                        config = config.domain_name(servername);
                    }
                    Some(config)
                } else {
                    None
                };
                #[cfg(not(feature = "redwire-tls"))]
                let tls = None;
                let mut client = crate::grpc::GrpcClient::connect_with_tls(
                    endpoint,
                    crate::grpc::DEFAULT_POOL_SIZE,
                    tls,
                )
                .await?;
                client.authenticate(&self.auth).await?;
                Ok(Arc::from(transport::grpc(client)))
            }
            #[cfg(feature = "http")]
            ConnectionTarget::Http { .. } => {
                let mut options = crate::http::HttpOptions::new(address);
                match &self.auth {
                    ConnectionAuth::Bearer(token) | ConnectionAuth::ApiKey(token) => {
                        options.token = Some(token.clone())
                    }
                    _ => {}
                }
                #[cfg(feature = "redwire-tls")]
                let identity = match (&self.tls.client_cert_pem, &self.tls.client_key_pem) {
                    (Some(cert), Some(key)) => Some([cert.as_slice(), key.as_slice()].concat()),
                    (None, None) => None,
                    _ => {
                        return Err(ClientError::new(
                            ErrorCode::InvalidArgument,
                            "mTLS requires a certificate and private key",
                        ))
                    }
                };
                #[cfg(feature = "redwire-tls")]
                let mut client = crate::http::HttpClient::new_with_pem(
                    options,
                    self.tls.ca_pem.as_deref(),
                    identity.as_deref(),
                )?;
                #[cfg(not(feature = "redwire-tls"))]
                let mut client = crate::http::HttpClient::new(options)?;
                client.health().await?;
                if let ConnectionAuth::Basic { user, pass } = &self.auth {
                    client.login(user, pass).await?;
                }
                Ok(Arc::from(transport::http(client)))
            }
            ConnectionTarget::RedWire { host, port, tls }
            | ConnectionTarget::WsNative { host, port, tls } => {
                let websocket = matches!(self.protocol.as_str(), "ws" | "wss");
                let auth = match &self.auth {
                    ConnectionAuth::Anonymous => Auth::Anonymous,
                    ConnectionAuth::Bearer(token) | ConnectionAuth::ApiKey(token) => {
                        Auth::Bearer(token.clone())
                    }
                    ConnectionAuth::Basic { user, pass } => Auth::Basic {
                        user: user.clone(),
                        pass: pass.clone(),
                    },
                };
                let options = ConnectOptions::new(host, port).with_auth(auth);
                #[cfg(feature = "redwire-tls")]
                let options = if tls {
                    options.with_tls(self.tls.clone())
                } else {
                    options
                };
                #[cfg(not(feature = "redwire-tls"))]
                if tls {
                    return Err(ClientError::feature_disabled("redwire-tls"));
                }
                if websocket {
                    #[cfg(feature = "redwire-websocket")]
                    {
                        let parsed = url::Url::parse(address).map_err(|_| {
                            ClientError::new(ErrorCode::InvalidUri, "invalid WebSocket URI")
                        })?;
                        let url = if !matches!(parsed.path(), "" | "/") {
                            address.to_string()
                        } else {
                            format!("{address}{}", reddb_wire::redwire::REDWIRE_WS_PATH)
                        };
                        return Ok(Arc::from(transport::redwire(
                            RedWireClient::connect_websocket(&url, options).await?,
                        )));
                    }
                    #[cfg(not(feature = "redwire-websocket"))]
                    return Err(ClientError::feature_disabled("redwire-websocket"));
                }
                Ok(Arc::from(transport::redwire(
                    RedWireClient::connect(options).await?,
                )))
            }
            other => Ok(Arc::from(transport::connect(other).await?)),
        }
    }
}

pub(crate) async fn connect(uri: &str) -> Result<DynTransport> {
    let spec = reddb_wire::parse_with_auth(uri).map_err(|error| {
        ClientError::new(
            if error.kind == reddb_wire::ParseErrorKind::UnsupportedScheme {
                ErrorCode::UnsupportedScheme
            } else {
                ErrorCode::InvalidUri
            },
            error.message,
        )
    })?;
    if matches!(
        spec.target,
        ConnectionTarget::Memory | ConnectionTarget::File { .. }
    ) {
        return transport::connect(spec.target).await;
    }
    let mut parsed = url::Url::parse(match &spec.target {
        ConnectionTarget::GrpcCluster { primary, .. } => primary,
        _ => uri,
    })
    .map_err(|_| ClientError::new(ErrorCode::InvalidUri, "invalid remote URI"))?;
    if matches!(spec.target, ConnectionTarget::GrpcCluster { .. }) {
        parsed.set_query(
            uri.split_once('?')
                .map(|(_, query)| query.split('#').next().unwrap_or_default()),
        );
    }
    let scheme = uri
        .trim()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let protocol = match scheme.as_str() {
        "red+ws" => "ws",
        "red+wss" => "wss",
        "red"
            if parsed
                .query_pairs()
                .any(|(key, value)| key == "tls" && matches!(value.as_ref(), "true" | "1")) =>
        {
            "reds"
        }
        other => other,
    }
    .to_string();
    let mut auth = spec.auth;
    if matches!(auth, ConnectionAuth::Anonymous) {
        if let Some((_, token)) = parsed
            .query_pairs()
            .find(|(key, _)| matches!(key.as_ref(), "token" | "apiKey" | "api_key"))
        {
            auth = ConnectionAuth::Bearer(token.into_owned());
        }
    }
    let mut replicas = Vec::new();
    let mut seed = match spec.target {
        ConnectionTarget::Grpc { endpoint } => grpc_address(&endpoint, &protocol),
        ConnectionTarget::GrpcCluster {
            primary,
            replicas: addresses,
            ..
        } => {
            replicas = addresses
                .iter()
                .map(|address| grpc_address(address, &protocol))
                .collect();
            grpc_address(&primary, &protocol)
        }
        ConnectionTarget::Http { base_url } => base_url,
        ConnectionTarget::RedWire { host, port, .. }
        | ConnectionTarget::WsNative { host, port, .. } => format!("{protocol}://{host}:{port}"),
        _ => unreachable!("embedded targets returned before remote parsing"),
    };
    if matches!(protocol.as_str(), "http" | "https" | "ws" | "wss")
        && !matches!(parsed.path(), "" | "/")
    {
        seed.push_str(parsed.path());
    }
    let force_primary = parsed.query_pairs().any(|(key, value)| {
        key.eq_ignore_ascii_case("route") && value.eq_ignore_ascii_case("primary")
    });
    #[cfg(feature = "redwire-tls")]
    let tls = {
        let parameter = |name: &str| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        let ca = parameter("ca");
        let cert = parameter("cert");
        let key = parameter("key");
        let mut tls = crate::redwire::TlsConfig::from_files(
            ca.as_deref().map(std::path::Path::new),
            cert.as_deref().map(std::path::Path::new),
            key.as_deref().map(std::path::Path::new),
        )?;
        tls.servername = parameter("servername");
        tls
    };
    let template = ConnectionTemplate {
        protocol,
        auth,
        #[cfg(feature = "redwire-tls")]
        tls,
    };
    let client = template.open(&seed).await?;
    let membership = ClusterMembership::from_uri_addresses(seed.clone(), replicas);
    let health = HealthAwareRouter::new(membership.clone());
    let router = RemoteRouter {
        template,
        force_primary,
        state: Mutex::new(RoutingState {
            seed: seed.clone(),
            membership,
            clients: BTreeMap::from([(seed, client)]),
            last_refresh: None,
            health,
            transaction: None,
            session: None,
            closed: false,
        }),
    };
    {
        let mut state = router.state.lock().await;
        router.refresh(&mut state).await;
    }
    Ok(Box::new(router))
}

fn grpc_address(address: &str, protocol: &str) -> String {
    format!(
        "{protocol}://{}",
        address
            .strip_prefix("http://")
            .or_else(|| address.strip_prefix("https://"))
            .unwrap_or(address)
    )
}

#[derive(Debug)]
struct RoutingState {
    seed: String,
    membership: ClusterMembership,
    clients: BTreeMap<String, SharedTransport>,
    last_refresh: Option<Instant>,
    health: HealthAwareRouter,
    transaction: Option<String>,
    session: Option<String>,
    closed: bool,
}

#[derive(Debug)]
struct RemoteRouter {
    template: ConnectionTemplate,
    force_primary: bool,
    state: Mutex<RoutingState>,
}

impl RemoteRouter {
    async fn endpoint(&self, state: &mut RoutingState, address: &str) -> Result<SharedTransport> {
        if state.closed {
            return Err(ClientError::new(
                ErrorCode::ClientClosed,
                "connection is closed",
            ));
        }
        if let Some(client) = state.clients.get(address) {
            return Ok(Arc::clone(client));
        }
        let client = tokio::time::timeout(Duration::from_secs(5), self.template.open(address))
            .await
            .map_err(|_| ClientError::new(ErrorCode::Network, "endpoint connection timed out"))??;
        state
            .clients
            .insert(address.to_string(), Arc::clone(&client));
        Ok(client)
    }

    async fn refresh(&self, state: &mut RoutingState) {
        if state.closed || state.transaction.is_some() || state.session.is_some() {
            return;
        }
        state.last_refresh = Some(Instant::now());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.refresh_membership(state)).await;
    }

    async fn refresh_membership(&self, state: &mut RoutingState) {
        let mut candidates = vec![state.membership.primary.addr.clone(), state.seed.clone()];
        candidates.extend(state.membership.replica_addrs());
        let mut index = 0;
        while index < candidates.len().min(258) {
            let address = candidates[index].clone();
            index += 1;
            // Discovery owns a separate connection: a cancelled refresh must
            // never leave a topology response queued ahead of a user query.
            let Ok(client) = self.template.open(&address).await else {
                continue;
            };
            let Ok(Some(mut membership)) = client.topology(&self.template.protocol).await else {
                continue;
            };
            let Some(primary) = routing_endpoint(&self.template.protocol, &membership.primary.addr)
            else {
                continue;
            };
            membership.primary.addr = primary.clone();
            membership.replicas.retain_mut(|replica| {
                if let Some(address) = routing_endpoint(&self.template.protocol, &replica.addr) {
                    replica.addr = address;
                    true
                } else {
                    false
                }
            });
            if primary != address && !candidates.contains(&primary) {
                candidates.push(primary.clone());
            }
            // Connect the next writer before publishing its membership.
            if self.endpoint(state, &primary).await.is_err() {
                continue;
            }
            state.health.update_membership(membership.clone());
            state.membership = membership;
            state.clients.retain(|address, client| {
                address == &state.seed
                    || address == &state.membership.primary.addr
                    || state
                        .membership
                        .replicas
                        .iter()
                        .any(|replica| &replica.addr == address)
                    || Arc::strong_count(client) > 1
            });
            if primary == address {
                for probe in state.health.endpoints_due_for_probe() {
                    let success = match self.template.open(&probe.url).await {
                        Ok(client) => client.topology(&self.template.protocol).await.is_ok(),
                        Err(_) => false,
                    };
                    state.health.record_probe_result(probe.index, success);
                }
                break;
            }
        }
    }

    async fn select(
        &self,
        route: SqlRoute,
        options: Option<&QueryOptions>,
    ) -> Result<(String, SharedTransport)> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(ClientError::new(
                ErrorCode::ClientClosed,
                "connection is closed",
            ));
        }
        if state
            .last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL)
        {
            self.refresh(&mut state).await;
        }
        let mut address = state
            .transaction
            .clone()
            .or_else(|| state.session.clone())
            .unwrap_or_else(|| state.membership.primary.addr.clone());
        if route == SqlRoute::Replica
            && state.transaction.is_none()
            && state.session.is_none()
            && !self.force_primary
        {
            let consistency = options.map(|options| &options.consistency);
            let mut replicas: Vec<_> = state
                .membership
                .replicas
                .iter()
                .filter(|replica| {
                    if !replica.healthy || replica.rebootstrapping {
                        return false;
                    }
                    match consistency {
                        Some(ReadConsistency::Strong) => false,
                        Some(ReadConsistency::Causal { bookmark, .. }) => {
                            replica.last_applied_lsn >= bookmark.commit_lsn()
                        }
                        Some(ReadConsistency::BoundedStaleness { max_lag }) => {
                            u128::from(replica.lag_ms) <= max_lag.as_millis()
                        }
                        _ => true,
                    }
                })
                .map(|replica| replica.addr.clone())
                .collect();
            if let Some(ReadConsistency::Causal { opts, .. }) = consistency {
                if let Some(region) = &opts.preferred_region {
                    let preferred: Vec<_> = state
                        .membership
                        .replicas
                        .iter()
                        .filter(|replica| {
                            &replica.region == region && replicas.contains(&replica.addr)
                        })
                        .map(|replica| replica.addr.clone())
                        .collect();
                    if !preferred.is_empty() {
                        replicas = preferred;
                    }
                }
            }
            if !replicas.is_empty() {
                let index = state.health.pick_read_index();
                if let Some(replica) = index
                    .checked_sub(1)
                    .and_then(|index| state.membership.replicas.get(index))
                {
                    address = if replicas.contains(&replica.addr) {
                        replica.addr.clone()
                    } else {
                        replicas[0].clone()
                    };
                }
            }
        }
        if route == SqlRoute::Begin {
            state.transaction = Some(address.clone());
        }
        if route == SqlRoute::Session {
            state.session = Some(address.clone());
        }
        let endpoint = match self.endpoint(&mut state, &address).await {
            Ok(endpoint) => endpoint,
            Err(_)
                if route == SqlRoute::Replica
                    && state.transaction.is_none()
                    && state.session.is_none() =>
            {
                state.last_refresh = None;
                state.health.observe_url(&address, Outcome::Timeout);
                address = state.membership.primary.addr.clone();
                self.endpoint(&mut state, &address).await?
            }
            Err(error) => return Err(error),
        };
        Ok((address, endpoint))
    }

    async fn write_result<T>(&self, address: &str, result: Result<T>) -> Result<T> {
        if result
            .as_ref()
            .is_err_and(|error| matches!(error.code, ErrorCode::Network | ErrorCode::IoError))
        {
            let mut state = self.state.lock().await;
            state.last_refresh = None;
            if state.transaction.is_none() && state.session.is_none() {
                state.clients.remove(address);
            }
        }
        result
    }

    async fn query_routed(
        &self,
        sql: &str,
        params: &[Value],
        options: Option<&QueryOptions>,
    ) -> Result<QueryResult> {
        let route = match sql_route(sql) {
            SqlRoute::Replica if options.is_none() => SqlRoute::Primary,
            route => route,
        };
        if route == SqlRoute::Replica {
            self.wait_for_causal(options).await;
        }
        let (address, endpoint) = self.select(route, options).await?;
        let started = Instant::now();
        let result = endpoint.query_with(sql, params).await;
        match result {
            Ok(result) => {
                let mut state = self.state.lock().await;
                state
                    .health
                    .observe_url(&address, Outcome::Rtt(started.elapsed()));
                if route == SqlRoute::End {
                    state.transaction = None;
                }
                Ok(result)
            }
            Err(error) if matches!(error.code, ErrorCode::Network | ErrorCode::IoError) => {
                let mut state = self.state.lock().await;
                state.last_refresh = None;
                state.health.observe_url(&address, Outcome::Timeout);
                if state.transaction.is_some() || state.session.is_some() {
                    return Err(error);
                }
                state.clients.remove(&address);
                if route != SqlRoute::Replica {
                    return Err(error);
                }
                state
                    .membership
                    .replicas
                    .retain(|replica| replica.addr != address);
                self.refresh(&mut state).await;
                let primary = state.membership.primary.addr.clone();
                let endpoint = self.endpoint(&mut state, &primary).await?;
                drop(state);
                endpoint.query_with(sql, params).await
            }
            Err(error) => Err(error),
        }
    }

    async fn wait_for_causal(&self, options: Option<&QueryOptions>) {
        let Some(QueryOptions {
            consistency: ReadConsistency::Causal { bookmark, opts },
        }) = options
        else {
            return;
        };
        if self.force_primary {
            return;
        }
        let started = Instant::now();
        loop {
            let mut state = self.state.lock().await;
            if state.closed
                || state.transaction.is_some()
                || state.session.is_some()
                || state.membership.replicas.is_empty()
                || state.membership.replicas.iter().any(|replica| {
                    replica.healthy
                        && !replica.rebootstrapping
                        && replica.last_applied_lsn >= bookmark.commit_lsn()
                })
            {
                return;
            }
            let Some(remaining) = opts
                .deadline
                .checked_sub(started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                return;
            };
            drop(state);
            tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
            let Some(remaining) = opts
                .deadline
                .checked_sub(started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                return;
            };
            state = self.state.lock().await;
            let _ = tokio::time::timeout(remaining, self.refresh(&mut state)).await;
        }
    }
}

#[async_trait]
impl Transport for RemoteRouter {
    fn name(&self) -> &'static str {
        "remote"
    }
    async fn query(&self, sql: &str) -> Result<QueryResult> {
        self.query_routed(sql, &[], None).await
    }
    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.query_routed(sql, params, None).await
    }
    async fn query_with_options(&self, sql: &str, options: QueryOptions) -> Result<QueryResult> {
        self.query_routed(sql, &[], Some(&options)).await
    }
    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        let (address, endpoint) = self.select(SqlRoute::Primary, None).await?;
        self.write_result(&address, endpoint.insert(collection, payload).await)
            .await
    }
    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        let (address, endpoint) = self.select(SqlRoute::Primary, None).await?;
        self.write_result(&address, endpoint.bulk_insert(collection, payloads).await)
            .await
    }
    async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        let (address, endpoint) = self.select(SqlRoute::Primary, None).await?;
        self.write_result(&address, endpoint.delete(collection, rid).await)
            .await
    }
    async fn watch_kv(
        &self,
        collection: &str,
        key: &str,
        from_lsn: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        self.select(SqlRoute::Primary, None)
            .await?
            .1
            .watch_kv(collection, key, from_lsn)
            .await
    }
    async fn close(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.closed = true;
        let clients = std::mem::take(&mut state.clients);
        drop(state);
        let mut result = Ok(());
        for client in clients.into_values() {
            if let Err(error) = client.close().await {
                result = Err(error);
            }
        }
        result
    }
}
