pub mod dns;
pub mod hosts;
pub mod http;
pub mod iocs;
pub mod mitre;
pub mod playbooks;
pub mod ports;
pub mod proxy;
pub mod subdomains;
pub mod tls;
pub mod utils;
pub mod whois;

// Pentest Workflow Segments
pub mod exploits;
pub mod fingerprints;
pub mod sessions;
pub mod vuln;

// Pentest Intelligence Segments
pub mod graph;
pub mod intelligence;
pub mod loot;
pub mod playbook_defs;

// Unified Intelligence Layer
pub mod actions;
pub mod convert;
pub mod state;

// Discovery Engine Segments
pub mod learned_words;
pub mod scan_state;
