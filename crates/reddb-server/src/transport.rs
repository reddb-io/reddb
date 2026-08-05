//! Transport listener descriptors and readiness state.
//!
//! These contracts are shared by process bootstrap and the HTTP health
//! surface. They intentionally live outside both so neither layer imports
//! the other's implementation details.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Router,
    Http,
    Https,
    Grpc,
    GrpcTls,
    Wire,
    WireTls,
    Postgres,
}

impl TransportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Router => "router",
            Self::Http => "http",
            Self::Https => "https",
            Self::Grpc => "grpc",
            Self::GrpcTls => "grpc-tls",
            Self::Wire => "wire",
            Self::WireTls => "wire-tls",
            Self::Postgres => "postgres",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportDescriptor {
    pub kind: TransportKind,
    pub bind_addr: String,
    pub explicit: bool,
}

impl TransportDescriptor {
    pub fn new(kind: TransportKind, bind_addr: String, explicit: bool) -> Self {
        Self {
            kind,
            bind_addr,
            explicit,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportSet {
    descriptors: Vec<TransportDescriptor>,
}

impl TransportSet {
    pub fn new(descriptors: Vec<TransportDescriptor>) -> Self {
        Self { descriptors }
    }

    pub fn descriptors(&self) -> &[TransportDescriptor] {
        &self.descriptors
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportListenerState {
    pub transport: String,
    pub bind_addr: String,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportListenerFailure {
    pub transport: String,
    pub bind_addr: String,
    pub explicit: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportReadiness {
    pub active: Vec<TransportListenerState>,
    pub failed: Vec<TransportListenerFailure>,
}

impl TransportReadiness {
    pub(crate) fn active(&mut self, transport: &str, bind_addr: &str, explicit: bool) {
        self.active.push(TransportListenerState {
            transport: transport.to_string(),
            bind_addr: bind_addr.to_string(),
            explicit,
        });
    }

    pub(crate) fn failed(
        &mut self,
        transport: &str,
        bind_addr: &str,
        explicit: bool,
        reason: String,
    ) {
        self.failed.push(TransportListenerFailure {
            transport: transport.to_string(),
            bind_addr: bind_addr.to_string(),
            explicit,
            reason,
        });
    }
}
