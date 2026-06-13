#![allow(dead_code)]

#[path = "grouped/http_grpc_auth/e2e_issue_547_cross_transport_envelope.rs"]
mod e2e_issue_547_cross_transport_envelope;

#[path = "grouped/http_grpc_auth/grpc_batch_insert.rs"]
mod grpc_batch_insert;

#[path = "grouped/http_grpc_auth/grpc_oauth_smoke.rs"]
mod grpc_oauth_smoke;

#[path = "grouped/http_grpc_auth/grpc_tls_smoke.rs"]
mod grpc_tls_smoke;

#[path = "grouped/http_grpc_auth/http_oauth_smoke.rs"]
mod http_oauth_smoke;

#[path = "grouped/http_grpc_auth/http_principal_inflight_cap.rs"]
mod http_principal_inflight_cap;

#[path = "grouped/http_grpc_auth/http_tls_smoke.rs"]
mod http_tls_smoke;

#[path = "grouped/http_grpc_auth/lease_atomic_http_opt_in.rs"]
mod lease_atomic_http_opt_in;

#[path = "grouped/http_grpc_auth/oauth_jwks_server.rs"]
mod oauth_jwks_server;
