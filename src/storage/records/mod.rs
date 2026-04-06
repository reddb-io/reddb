//! Schema definitions for compact storage
//! Each data type has optimized binary format

// Submodules
mod dns;
mod helpers;
mod http;
mod intel;
mod network;
mod pentest;
mod port_health;
mod proxy;
mod threat;
mod tls;
mod types;

// Re-export types module
pub use types::{RecordType, WhoisRecord};

// Re-export network types
pub use network::{PortScanRecord, PortStatus, SubdomainRecord, SubdomainSource};

// Re-export TLS types
pub use tls::{
    TlsCertRecord, TlsCipherRecord, TlsCipherStrength, TlsScanRecord, TlsSeverity,
    TlsVersionRecord, TlsVulnerabilityRecord,
};

// Re-export HTTP types
pub use http::{HttpHeadersRecord, HttpTlsSnapshot};

// Re-export DNS types
pub use dns::{DnsRecordData, DnsRecordType};

// Re-export intel types
pub use intel::{FingerprintRecord, HostIntelRecord, ServiceIntelRecord};

// Re-export pentest workflow types
pub use pentest::{
    ExploitAttemptRecord, ExploitStatus, PlaybookRunRecord, PlaybookStatus, SessionRecord,
    SessionStatus, StepResult, VulnerabilityRecord,
};

// Re-export threat intel types
pub use threat::{IocRecord, IocType, MitreAttackRecord};

// Re-export port health types
pub use port_health::{PortHealthRecord, PortStateChange};

// Re-export proxy types
pub use proxy::{
    ProxyConnectionRecord, ProxyHttpRequestRecord, ProxyHttpResponseRecord, ProxyWebSocketRecord,
};

// Re-export string helpers
pub use helpers::{read_string_u16, write_string_u16};

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::{SystemTime, UNIX_EPOCH};

    // ==================== PortScanRecord Tests ====================

    #[test]
    fn test_port_scan_serialization() {
        let record = PortScanRecord {
            ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            port: 80,
            status: PortStatus::Open,
            service_id: 1,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32,
        };

        let bytes = record.to_bytes();
        println!("PortScan size: {} bytes", bytes.len());

        let decoded = PortScanRecord::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.port, 80);
    }

    #[test]
    fn test_port_scan_new() {
        let record = PortScanRecord::new(0xC0A80101, 443, 0, 2); // 192.168.1.1
        assert_eq!(record.port, 443);
        assert!(matches!(record.status, PortStatus::Open));
        assert_eq!(record.service_id, 2);
    }

    #[test]
    fn test_port_scan_all_statuses() {
        for (status_byte, expected) in [
            (0, PortStatus::Open),
            (1, PortStatus::Closed),
            (2, PortStatus::Filtered),
            (3, PortStatus::OpenFiltered),
        ] {
            let record = PortScanRecord::new(0x7F000001, 22, status_byte, 0);
            assert!(
                matches!(record.status, _ if std::mem::discriminant(&record.status) == std::mem::discriminant(&expected))
            );
        }
    }

    #[test]
    fn test_port_scan_ipv4_roundtrip() {
        let record = PortScanRecord {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: 22,
            status: PortStatus::Open,
            service_id: 5,
            timestamp: 1700000000,
        };

        let bytes = record.to_bytes();
        let decoded = PortScanRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(decoded.port, 22);
        assert!(matches!(decoded.status, PortStatus::Open));
        assert_eq!(decoded.service_id, 5);
        assert_eq!(decoded.timestamp, 1700000000);
    }

    #[test]
    fn test_port_scan_ipv6_roundtrip() {
        let record = PortScanRecord {
            ip: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            port: 8080,
            status: PortStatus::Filtered,
            service_id: 10,
            timestamp: 1600000000,
        };

        let bytes = record.to_bytes();
        assert_eq!(bytes.len(), 25); // 1 + 16 + 2 + 1 + 1 + 4

        let decoded = PortScanRecord::from_bytes(&bytes).unwrap();
        assert!(matches!(decoded.ip, IpAddr::V6(_)));
        assert_eq!(decoded.port, 8080);
    }

    #[test]
    fn test_port_scan_from_bytes_empty() {
        assert!(PortScanRecord::from_bytes(&[]).is_none());
    }

    #[test]
    fn test_port_scan_from_bytes_invalid_ip_version() {
        let buf = vec![99, 0, 0, 0, 0]; // Invalid IP version
        assert!(PortScanRecord::from_bytes(&buf).is_none());
    }

    #[test]
    fn test_port_scan_from_bytes_truncated() {
        let buf = vec![4, 192, 168]; // Incomplete IPv4
        assert!(PortScanRecord::from_bytes(&buf).is_none());
    }

    // ==================== SubdomainRecord Tests ====================

    #[test]
    fn test_subdomain_serialization() {
        let record = SubdomainRecord {
            subdomain: "api.example.com".to_string(),
            ips: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))],
            source: SubdomainSource::DnsBruteforce,
            timestamp: 1234567890,
        };

        let bytes = record.to_bytes();
        println!("Subdomain size: {} bytes", bytes.len());

        let decoded = SubdomainRecord::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.subdomain, "api.example.com");
    }

    #[test]
    fn test_subdomain_all_sources() {
        for source in [
            SubdomainSource::DnsBruteforce,
            SubdomainSource::CertTransparency,
            SubdomainSource::SearchEngine,
            SubdomainSource::WebCrawl,
        ] {
            let record = SubdomainRecord {
                subdomain: "test.example.com".to_string(),
                ips: vec![],
                source,
                timestamp: 1000,
            };

            let bytes = record.to_bytes();
            let decoded = SubdomainRecord::from_bytes(&bytes).unwrap();
            assert!(std::mem::discriminant(&decoded.source) == std::mem::discriminant(&source));
        }
    }

    #[test]
    fn test_subdomain_multiple_ips() {
        let record = SubdomainRecord {
            subdomain: "multi.example.com".to_string(),
            ips: vec![
                IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            ],
            source: SubdomainSource::CertTransparency,
            timestamp: 1700000000,
        };

        let bytes = record.to_bytes();
        let decoded = SubdomainRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ips.len(), 3);
        assert_eq!(decoded.ips[0], IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(matches!(decoded.ips[2], IpAddr::V6(_)));
    }

    #[test]
    fn test_subdomain_empty_ips() {
        let record = SubdomainRecord {
            subdomain: "noip.example.com".to_string(),
            ips: vec![],
            source: SubdomainSource::WebCrawl,
            timestamp: 1500000000,
        };

        let bytes = record.to_bytes();
        let decoded = SubdomainRecord::from_bytes(&bytes).unwrap();

        assert!(decoded.ips.is_empty());
        assert_eq!(decoded.subdomain, "noip.example.com");
    }

    #[test]
    fn test_subdomain_from_bytes_empty() {
        assert!(SubdomainRecord::from_bytes(&[]).is_none());
    }

    #[test]
    fn test_subdomain_from_bytes_truncated() {
        let buf = vec![5, b'h', b'e', b'l']; // Incomplete subdomain
        assert!(SubdomainRecord::from_bytes(&buf).is_none());
    }

    // ==================== HostIntelRecord Tests ====================

    #[test]
    fn test_host_intel_roundtrip() {
        let record = HostIntelRecord {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            os_family: Some("Linux".to_string()),
            confidence: 0.85,
            last_seen: 1700000000,
            services: vec![
                ServiceIntelRecord {
                    port: 22,
                    service_name: Some("SSH".to_string()),
                    banner: Some("OpenSSH 8.2".to_string()),
                    os_hints: vec!["Ubuntu".to_string()],
                },
                ServiceIntelRecord {
                    port: 80,
                    service_name: Some("HTTP".to_string()),
                    banner: None,
                    os_hints: vec![],
                },
            ],
        };

        let bytes = record.to_bytes();
        let decoded = HostIntelRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(decoded.os_family, Some("Linux".to_string()));
        assert!((decoded.confidence - 0.85).abs() < 0.001);
        assert_eq!(decoded.services.len(), 2);
        assert_eq!(decoded.services[0].port, 22);
        assert_eq!(decoded.services[0].banner, Some("OpenSSH 8.2".to_string()));
    }

    #[test]
    fn test_host_intel_ipv6() {
        let record = HostIntelRecord {
            ip: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            os_family: None,
            confidence: 0.0,
            last_seen: 1600000000,
            services: vec![],
        };

        let bytes = record.to_bytes();
        let decoded = HostIntelRecord::from_bytes(&bytes).unwrap();

        assert!(matches!(decoded.ip, IpAddr::V6(_)));
        assert!(decoded.os_family.is_none());
        assert!(decoded.services.is_empty());
    }

    #[test]
    fn test_host_intel_empty() {
        assert!(HostIntelRecord::from_bytes(&[]).is_err());
    }

    #[test]
    fn test_host_intel_invalid_ip_version() {
        let buf = vec![99]; // Invalid IP version
        assert!(HostIntelRecord::from_bytes(&buf).is_err());
    }

    // ==================== ServiceIntelRecord Tests ====================

    #[test]
    fn test_service_intel_roundtrip() {
        let record = ServiceIntelRecord {
            port: 443,
            service_name: Some("HTTPS".to_string()),
            banner: Some("nginx/1.18.0".to_string()),
            os_hints: vec!["Debian".to_string(), "Ubuntu".to_string()],
        };

        let bytes = record.to_bytes();
        let decoded = ServiceIntelRecord::from_slice(&bytes).unwrap();

        assert_eq!(decoded.port, 443);
        assert_eq!(decoded.service_name, Some("HTTPS".to_string()));
        assert_eq!(decoded.banner, Some("nginx/1.18.0".to_string()));
        assert_eq!(decoded.os_hints.len(), 2);
    }

    #[test]
    fn test_service_intel_minimal() {
        let record = ServiceIntelRecord {
            port: 8080,
            service_name: None,
            banner: None,
            os_hints: vec![],
        };

        let bytes = record.to_bytes();
        let decoded = ServiceIntelRecord::from_slice(&bytes).unwrap();

        assert_eq!(decoded.port, 8080);
        assert!(decoded.service_name.is_none());
        assert!(decoded.banner.is_none());
        assert!(decoded.os_hints.is_empty());
    }

    // ==================== String Helper Tests ====================

    #[test]
    fn test_write_read_string_u16() {
        let mut buf = Vec::new();
        write_string_u16(&mut buf, "Hello, World!");

        let mut offset = 0;
        let result = read_string_u16(&buf, &mut offset).unwrap();
        assert_eq!(result, "Hello, World!");
        assert_eq!(offset, buf.len());
    }

    #[test]
    fn test_write_read_string_u16_empty() {
        let mut buf = Vec::new();
        write_string_u16(&mut buf, "");

        let mut offset = 0;
        let result = read_string_u16(&buf, &mut offset).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_write_read_string_u16_unicode() {
        let mut buf = Vec::new();
        write_string_u16(&mut buf, "日本語テスト 🚀");

        let mut offset = 0;
        let result = read_string_u16(&buf, &mut offset).unwrap();
        assert_eq!(result, "日本語テスト 🚀");
    }

    #[test]
    fn test_read_string_u16_truncated() {
        let buf = vec![0x05, 0x00, b'h', b'e']; // Says 5 bytes but only 2
        let mut offset = 0;
        assert!(read_string_u16(&buf, &mut offset).is_none());
    }

    // ==================== DnsRecordType Tests ====================

    #[test]
    fn test_dns_record_type_values() {
        assert_eq!(DnsRecordType::A as u8, 1);
        assert_eq!(DnsRecordType::AAAA as u8, 2);
        assert_eq!(DnsRecordType::MX as u8, 3);
        assert_eq!(DnsRecordType::NS as u8, 4);
        assert_eq!(DnsRecordType::TXT as u8, 5);
        assert_eq!(DnsRecordType::CNAME as u8, 6);
    }

    // ==================== TLS Types Tests ====================

    #[test]
    fn test_tls_cipher_strength_values() {
        assert_eq!(TlsCipherStrength::Weak as u8, 0);
        assert_eq!(TlsCipherStrength::Medium as u8, 1);
        assert_eq!(TlsCipherStrength::Strong as u8, 2);
    }

    #[test]
    fn test_tls_severity_values() {
        assert_eq!(TlsSeverity::Low as u8, 0);
        assert_eq!(TlsSeverity::Medium as u8, 1);
        assert_eq!(TlsSeverity::High as u8, 2);
        assert_eq!(TlsSeverity::Critical as u8, 3);
    }

    #[test]
    fn test_http_tls_snapshot_default() {
        let snapshot = HttpTlsSnapshot::default();
        assert!(snapshot.authority.is_none());
        assert!(snapshot.tls_version.is_none());
        assert!(snapshot.peer_subjects.is_empty());
    }

    // ==================== RecordType Tests ====================

    #[test]
    fn test_record_type_port_scan() {
        let record = RecordType::PortScan(PortScanRecord::new(0x7F000001, 80, 0, 1));
        assert!(matches!(record, RecordType::PortScan(_)));
    }

    #[test]
    fn test_record_type_subdomain() {
        let record = RecordType::Subdomain(SubdomainRecord {
            subdomain: "test.com".to_string(),
            ips: vec![],
            source: SubdomainSource::DnsBruteforce,
            timestamp: 0,
        });
        assert!(matches!(record, RecordType::Subdomain(_)));
    }

    #[test]
    fn test_record_type_key_value() {
        let record = RecordType::KeyValue(b"key".to_vec(), b"value".to_vec());
        if let RecordType::KeyValue(k, v) = record {
            assert_eq!(k, b"key");
            assert_eq!(v, b"value");
        } else {
            panic!("Expected KeyValue");
        }
    }

    // ==================== Proxy Record Tests ====================

    #[test]
    fn test_proxy_connection_record_roundtrip_ipv4() {
        let record = ProxyConnectionRecord {
            connection_id: 12345,
            src_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            src_port: 54321,
            dst_host: "example.com".to_string(),
            dst_port: 443,
            protocol: 0, // TCP
            started_at: 1700000000,
            ended_at: 1700000060,
            bytes_sent: 1024,
            bytes_received: 4096,
            tls_intercepted: true,
        };

        let bytes = record.to_bytes();
        let decoded = ProxyConnectionRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.connection_id, 12345);
        assert_eq!(decoded.src_ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(decoded.src_port, 54321);
        assert_eq!(decoded.dst_host, "example.com");
        assert_eq!(decoded.dst_port, 443);
        assert_eq!(decoded.protocol, 0);
        assert_eq!(decoded.started_at, 1700000000);
        assert_eq!(decoded.ended_at, 1700000060);
        assert_eq!(decoded.bytes_sent, 1024);
        assert_eq!(decoded.bytes_received, 4096);
        assert!(decoded.tls_intercepted);
    }

    #[test]
    fn test_proxy_connection_record_roundtrip_ipv6() {
        let record = ProxyConnectionRecord {
            connection_id: 99999,
            src_ip: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            src_port: 12345,
            dst_host: "ipv6.example.org".to_string(),
            dst_port: 80,
            protocol: 1, // UDP
            started_at: 1600000000,
            ended_at: 0,
            bytes_sent: 256,
            bytes_received: 512,
            tls_intercepted: false,
        };

        let bytes = record.to_bytes();
        let decoded = ProxyConnectionRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.connection_id, 99999);
        assert!(matches!(decoded.src_ip, IpAddr::V6(_)));
        assert_eq!(decoded.dst_host, "ipv6.example.org");
        assert!(!decoded.tls_intercepted);
    }

    #[test]
    fn test_proxy_connection_record_empty() {
        assert!(ProxyConnectionRecord::from_bytes(&[]).is_err());
    }

    #[test]
    fn test_proxy_http_request_record_roundtrip() {
        let record = ProxyHttpRequestRecord {
            connection_id: 1000,
            request_seq: 1,
            method: "GET".to_string(),
            path: "/api/users?page=1".to_string(),
            http_version: "HTTP/1.1".to_string(),
            host: "api.example.com".to_string(),
            headers: vec![
                ("user-agent".to_string(), "Mozilla/5.0".to_string()),
                ("accept".to_string(), "application/json".to_string()),
            ],
            body: vec![],
            timestamp: 1700000000,
            client_addr: Some("192.168.1.100:54321".to_string()),
        };

        let bytes = record.to_bytes();
        let decoded = ProxyHttpRequestRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.connection_id, 1000);
        assert_eq!(decoded.request_seq, 1);
        assert_eq!(decoded.method, "GET");
        assert_eq!(decoded.path, "/api/users?page=1");
        assert_eq!(decoded.http_version, "HTTP/1.1");
        assert_eq!(decoded.host, "api.example.com");
        assert_eq!(decoded.headers.len(), 2);
        assert_eq!(decoded.headers[0].0, "user-agent");
        assert!(decoded.body.is_empty());
        assert_eq!(decoded.client_addr, Some("192.168.1.100:54321".to_string()));
    }

    #[test]
    fn test_proxy_http_request_record_with_body() {
        let record = ProxyHttpRequestRecord {
            connection_id: 2000,
            request_seq: 3,
            method: "POST".to_string(),
            path: "/api/login".to_string(),
            http_version: "HTTP/1.1".to_string(),
            host: "auth.example.com".to_string(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: b"{\"username\":\"test\"}".to_vec(),
            timestamp: 1700000100,
            client_addr: None,
        };

        let bytes = record.to_bytes();
        let decoded = ProxyHttpRequestRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.method, "POST");
        assert_eq!(decoded.body, b"{\"username\":\"test\"}");
        assert!(decoded.client_addr.is_none());
    }

    #[test]
    fn test_proxy_http_response_record_roundtrip() {
        let record = ProxyHttpResponseRecord {
            connection_id: 1000,
            request_seq: 1,
            status_code: 200,
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("content-length".to_string(), "42".to_string()),
            ],
            body: b"{\"users\":[{\"id\":1,\"name\":\"Test\"}]}".to_vec(),
            timestamp: 1700000001,
            content_type: Some("application/json".to_string()),
        };

        let bytes = record.to_bytes();
        let decoded = ProxyHttpResponseRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.connection_id, 1000);
        assert_eq!(decoded.request_seq, 1);
        assert_eq!(decoded.status_code, 200);
        assert_eq!(decoded.status_text, "OK");
        assert_eq!(decoded.headers.len(), 2);
        assert!(!decoded.body.is_empty());
        assert_eq!(decoded.content_type, Some("application/json".to_string()));
    }

    #[test]
    fn test_proxy_http_response_record_error() {
        let record = ProxyHttpResponseRecord {
            connection_id: 3000,
            request_seq: 5,
            status_code: 404,
            status_text: "Not Found".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: b"Page not found".to_vec(),
            timestamp: 1700000200,
            content_type: Some("text/plain".to_string()),
        };

        let bytes = record.to_bytes();
        let decoded = ProxyHttpResponseRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.status_code, 404);
        assert_eq!(decoded.status_text, "Not Found");
    }

    #[test]
    fn test_proxy_websocket_record_roundtrip() {
        let record = ProxyWebSocketRecord {
            connection_id: 5000,
            frame_seq: 42,
            direction: 0, // client -> server
            opcode: 1,    // text
            payload: b"Hello WebSocket!".to_vec(),
            timestamp: 1700000300,
        };

        let bytes = record.to_bytes();
        let decoded = ProxyWebSocketRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.connection_id, 5000);
        assert_eq!(decoded.frame_seq, 42);
        assert_eq!(decoded.direction, 0);
        assert_eq!(decoded.opcode, 1);
        assert_eq!(decoded.payload, b"Hello WebSocket!");
        assert_eq!(decoded.timestamp, 1700000300);
    }

    #[test]
    fn test_proxy_websocket_record_binary() {
        let record = ProxyWebSocketRecord {
            connection_id: 6000,
            frame_seq: 100,
            direction: 1, // server -> client
            opcode: 2,    // binary
            payload: vec![0x00, 0x01, 0x02, 0x03, 0xFF],
            timestamp: 1700000400,
        };

        let bytes = record.to_bytes();
        let decoded = ProxyWebSocketRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.opcode, 2);
        assert_eq!(decoded.payload, vec![0x00, 0x01, 0x02, 0x03, 0xFF]);
    }

    #[test]
    fn test_proxy_websocket_record_ping_pong() {
        for (opcode, name) in [(9, "ping"), (10, "pong")] {
            let record = ProxyWebSocketRecord {
                connection_id: 7000,
                frame_seq: 0,
                direction: 0,
                opcode,
                payload: vec![],
                timestamp: 1700000500,
            };

            let bytes = record.to_bytes();
            let decoded = ProxyWebSocketRecord::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.opcode, opcode, "Failed for {}", name);
        }
    }

    #[test]
    fn test_proxy_websocket_record_empty() {
        assert!(ProxyWebSocketRecord::from_bytes(&[]).is_err());
    }

    #[test]
    fn test_proxy_websocket_record_truncated() {
        // Only 10 bytes, needs at least 18
        let buf = vec![0u8; 10];
        assert!(ProxyWebSocketRecord::from_bytes(&buf).is_err());
    }
}
