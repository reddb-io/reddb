//! Issue #829 — replication mTLS peer identity and ack attribution.

use std::sync::Arc;
use std::time::Duration;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::{Empty, JsonPayloadRequest};
use reddb::replication::ReplicationConfig;
use reddb::runtime::RedDBRuntime;
use reddb::{GrpcServerOptions, GrpcTlsOptions, RedDBGrpcServer, RedDBOptions};
use rustls::pki_types::CertificateDer;
use tonic::transport::{Certificate as TonicCertificate, ClientTlsConfig, Endpoint, Identity};
use tonic::Code;

fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn wait_for_port(port: u16, max_ms: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gRPC server never came up on port {port}");
}

struct TestCa {
    cert: Certificate,
    issuer: Issuer<'static, KeyPair>,
}

struct TestCert {
    cert_pem: String,
    key_pem: String,
    der: Vec<u8>,
}

fn test_ca() -> TestCa {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "reddb-test-ca");
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    params.key_usages.push(KeyUsagePurpose::CrlSign);

    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let issuer = Issuer::new(params, key_pair);
    TestCa { cert, issuer }
}

fn signed_cert(ca: &Issuer<'static, KeyPair>, common_name: &str, server: bool) -> TestCert {
    let sans = if server {
        vec!["localhost".to_string()]
    } else {
        Vec::new()
    };
    let mut params = CertificateParams::new(sans).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.use_authority_key_identifier_extension = true;
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params.extended_key_usages.push(if server {
        ExtendedKeyUsagePurpose::ServerAuth
    } else {
        ExtendedKeyUsagePurpose::ClientAuth
    });

    let key_pair = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key_pair, ca).unwrap();
    TestCert {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
        der: cert.der().to_vec(),
    }
}

fn install_policy(store: &AuthStore, username: &str, id: &str, actions: &[&str]) {
    let actions_json = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<_>>()
        .join(",");
    let policy_json = format!(
        r#"{{"id":"{id}","version":1,"statements":[{{"effect":"allow","actions":[{actions_json}],"resources":["cluster:replication"]}}]}}"#
    );
    store
        .put_policy(Policy::from_json_str(&policy_json).expect("policy parses"))
        .expect("put policy");
    store
        .attach_policy(PrincipalRef::User(UserId::platform(username)), id)
        .expect("attach policy");
}

async fn connect_mtls_client(
    port: u16,
    ca_pem: &str,
    client: Option<&TestCert>,
) -> RedDbClient<tonic::transport::Channel> {
    let mut tls = ClientTlsConfig::new()
        .domain_name("localhost")
        .ca_certificate(TonicCertificate::from_pem(ca_pem.as_bytes()));
    if let Some(client) = client {
        tls = tls.identity(Identity::from_pem(
            client.cert_pem.as_bytes(),
            client.key_pem.as_bytes(),
        ));
    }
    let endpoint = Endpoint::from_shared(format!("https://localhost:{port}"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(5))
        .tls_config(tls)
        .unwrap();
    RedDbClient::new(endpoint.connect().await.expect("mTLS connect"))
}

fn pull_request(replica_id: &str) -> tonic::Request<JsonPayloadRequest> {
    tonic::Request::new(JsonPayloadRequest {
        payload_json: format!(r#"{{"replica_id":"{replica_id}","since_lsn":0,"max_count":10}}"#),
    })
}

fn ack_request(replica_id: &str, applied_lsn: u64) -> tonic::Request<JsonPayloadRequest> {
    tonic::Request::new(JsonPayloadRequest {
        payload_json: format!(
            r#"{{"replica_id":"{replica_id}","applied_lsn":{applied_lsn},"durable_lsn":{applied_lsn}}}"#
        ),
    })
}

#[tokio::test]
async fn replication_mtls_subject_identity_binds_ack_attribution() {
    let ca = test_ca();
    let server_cert = signed_cert(&ca.issuer, "primary", true);
    let replica_cert = signed_cert(&ca.issuer, "replica_a", false);
    let peer_identity = reddb::cluster::NodeIdentity::from_peer_certificate_der(
        &CertificateDer::from(replica_cert.der.clone()),
    )
    .expect("client cert subject becomes node identity");
    let voter_identity =
        reddb::cluster::ClusterVoterIdentity::from_certificate_subject(peer_identity.as_str())
            .expect("witness voting uses the same node identity model");
    assert_eq!(voter_identity, peer_identity);
    assert_eq!(peer_identity.as_str(), "CN=replica_a");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store
        .create_user(peer_identity.as_str(), "unused", Role::Read)
        .unwrap();
    install_policy(
        &store,
        peer_identity.as_str(),
        "p_replica_a_cert",
        &["cluster:replication:stream", "cluster:replication:ack"],
    );

    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let server = RedDBGrpcServer::with_options(
        runtime.clone(),
        GrpcServerOptions {
            bind_addr: bind,
            tls: Some(GrpcTlsOptions {
                cert_pem: server_cert.cert_pem.into_bytes(),
                key_pem: server_cert.key_pem.into_bytes(),
                client_ca_pem: Some(ca.cert.pem().into_bytes()),
            }),
        },
        store,
    );
    let server_handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut no_cert_client = connect_mtls_client(port, &ca.cert.pem(), None).await;
    let no_cert = no_cert_client
        .health(tonic::Request::new(Empty {}))
        .await
        .expect_err("mTLS-required replication listener rejects clients without certs");
    assert_ne!(no_cert.code(), Code::Ok);

    let mut client = connect_mtls_client(port, &ca.cert.pem(), Some(&replica_cert)).await;
    client
        .pull_wal_records(pull_request(peer_identity.as_str()))
        .await
        .expect("cert-identified replica can stream WAL records");

    let forged = client
        .ack_replica_lsn(ack_request("CN=replica_b", 25))
        .await
        .expect_err("client cert subject cannot ack as another replica");
    assert_eq!(forged.code(), Code::PermissionDenied);

    let ack = client
        .ack_replica_lsn(ack_request(peer_identity.as_str(), 25))
        .await
        .expect("client cert subject can ack as itself")
        .into_inner();
    let body: serde_json::Value = serde_json::from_str(&ack.payload).expect("ack reply JSON");
    assert_eq!(body["replica_id"], peer_identity.as_str());

    let replica = runtime
        .primary_replica_snapshots()
        .into_iter()
        .find(|replica| replica.id == peer_identity.as_str())
        .expect("replica registered by cert identity");
    assert_eq!(replica.last_acked_lsn, 25);

    server_handle.abort();
}
