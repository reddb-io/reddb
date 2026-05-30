//! Per-node cluster identities shared by replication and voting.

use rustls::pki_types::CertificateDer;

/// Stable identity for a node participating in cluster protocols.
///
/// The value is the X.509 subject distinguished name from a validated
/// node certificate. Replication peers and cluster voters intentionally
/// use this same type so acknowledgements and votes cannot drift into
/// separate identity namespaces.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIdentity(String);

/// Replication and witness-voting identities are the same cluster node identity.
pub type ReplicationPeerIdentity = NodeIdentity;
pub type ClusterVoterIdentity = NodeIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeIdentityError {
    EmptySubject,
    CertificateParse(String),
}

impl std::fmt::Display for NodeIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySubject => write!(f, "certificate subject is empty"),
            Self::CertificateParse(err) => write!(f, "certificate parse error: {err}"),
        }
    }
}

impl std::error::Error for NodeIdentityError {}

impl NodeIdentity {
    pub fn from_certificate_subject(subject: impl AsRef<str>) -> Result<Self, NodeIdentityError> {
        let subject = subject.as_ref().trim();
        if subject.is_empty() {
            return Err(NodeIdentityError::EmptySubject);
        }
        Ok(Self(subject.to_string()))
    }

    pub fn from_peer_certificate_der(cert: &CertificateDer<'_>) -> Result<Self, NodeIdentityError> {
        let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
            .map_err(|err| NodeIdentityError::CertificateParse(format!("{err:?}")))?;
        Self::from_certificate_subject(parsed.subject().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_identity_rejects_empty_certificate_subjects() {
        assert_eq!(
            NodeIdentity::from_certificate_subject("   ").unwrap_err(),
            NodeIdentityError::EmptySubject
        );
    }

    #[test]
    fn cluster_voter_and_replication_peer_share_node_identity() {
        let voter = ClusterVoterIdentity::from_certificate_subject("CN=node-a").unwrap();
        let replica = ReplicationPeerIdentity::from_certificate_subject("CN=node-a").unwrap();

        assert_eq!(voter, replica);
        assert_eq!(voter.as_str(), "CN=node-a");
    }
}
