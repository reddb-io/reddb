//! DNS record types

/// DNS record
#[derive(Debug, Clone)]
pub struct DnsRecordData {
    pub domain: String,
    pub record_type: DnsRecordType,
    pub value: String,
    pub ttl: u32,
    pub timestamp: u32,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DnsRecordType {
    A = 1,
    AAAA = 2,
    MX = 3,
    NS = 4,
    TXT = 5,
    CNAME = 6,
}
