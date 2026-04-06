#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentKind {
    Ports = 1,
    Subdomains = 2,
    Whois = 3,
    Tls = 4,
    Dns = 5,
    Http = 6,
    Host = 7,
    Proxy = 8,
    Mitre = 9,
    Ioc = 10,
    Vuln = 11,
    Sessions = 12,
    Playbooks = 13,
    Actions = 14,
    Traces = 15,
    Loot = 16,
}

impl SegmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SegmentKind::Ports => "ports",
            SegmentKind::Subdomains => "subdomains",
            SegmentKind::Whois => "whois",
            SegmentKind::Tls => "tls",
            SegmentKind::Dns => "dns",
            SegmentKind::Http => "http",
            SegmentKind::Host => "hosts",
            SegmentKind::Proxy => "proxy",
            SegmentKind::Mitre => "mitre",
            SegmentKind::Ioc => "ioc",
            SegmentKind::Vuln => "vuln",
            SegmentKind::Sessions => "sessions",
            SegmentKind::Playbooks => "playbooks",
            SegmentKind::Actions => "actions",
            SegmentKind::Traces => "traces",
            SegmentKind::Loot => "loot",
        }
    }
}
