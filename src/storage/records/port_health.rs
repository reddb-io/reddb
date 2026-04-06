//! Port health monitoring record types

/// Port state change type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStateChange {
    /// Port is still open (no change)
    StillOpen,
    /// Port is still closed (no change)
    StillClosed,
    /// Port was closed, now open
    Opened,
    /// Port was open, now closed
    Closed,
    /// First time seeing this port
    New,
}

impl PortStateChange {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortStateChange::StillOpen => "still_open",
            PortStateChange::StillClosed => "still_closed",
            PortStateChange::Opened => "opened",
            PortStateChange::Closed => "closed",
            PortStateChange::New => "new",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "still_open" => PortStateChange::StillOpen,
            "still_closed" => PortStateChange::StillClosed,
            "opened" => PortStateChange::Opened,
            "closed" => PortStateChange::Closed,
            "new" => PortStateChange::New,
            _ => PortStateChange::New,
        }
    }

    pub fn to_byte(&self) -> u8 {
        match self {
            PortStateChange::StillOpen => 0,
            PortStateChange::StillClosed => 1,
            PortStateChange::Opened => 2,
            PortStateChange::Closed => 3,
            PortStateChange::New => 4,
        }
    }

    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => PortStateChange::StillOpen,
            1 => PortStateChange::StillClosed,
            2 => PortStateChange::Opened,
            3 => PortStateChange::Closed,
            _ => PortStateChange::New,
        }
    }
}

/// Port health check record - tracks port state changes over time
#[derive(Debug, Clone)]
pub struct PortHealthRecord {
    /// Target host IP or hostname
    pub host: String,
    /// Port number
    pub port: u16,
    /// Current state (open/closed)
    pub is_open: bool,
    /// State change type from last check
    pub change: PortStateChange,
    /// Response time in milliseconds (0 if closed)
    pub response_time_ms: u32,
    /// Service detected (if any)
    pub service: Option<String>,
    /// Previous check timestamp
    pub previous_check: u32,
    /// Current check timestamp
    pub checked_at: u32,
    /// Number of consecutive checks with same state
    pub consecutive_same_state: u16,
}

impl PortHealthRecord {
    pub fn new(host: String, port: u16, is_open: bool) -> Self {
        Self {
            host,
            port,
            is_open,
            change: PortStateChange::New,
            response_time_ms: 0,
            service: None,
            previous_check: 0,
            checked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32,
            consecutive_same_state: 1,
        }
    }

    /// Convert to bytes for storage
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Host (length-prefixed)
        let host_bytes = self.host.as_bytes();
        bytes.extend_from_slice(&(host_bytes.len() as u16).to_le_bytes());
        bytes.extend_from_slice(host_bytes);

        // Port
        bytes.extend_from_slice(&self.port.to_le_bytes());

        // is_open
        bytes.push(self.is_open as u8);

        // change
        bytes.push(self.change.to_byte());

        // response_time_ms
        bytes.extend_from_slice(&self.response_time_ms.to_le_bytes());

        // Service (length-prefixed, 0 if None)
        if let Some(ref service) = self.service {
            let service_bytes = service.as_bytes();
            bytes.extend_from_slice(&(service_bytes.len() as u16).to_le_bytes());
            bytes.extend_from_slice(service_bytes);
        } else {
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }

        // previous_check
        bytes.extend_from_slice(&self.previous_check.to_le_bytes());

        // checked_at
        bytes.extend_from_slice(&self.checked_at.to_le_bytes());

        // consecutive_same_state
        bytes.extend_from_slice(&self.consecutive_same_state.to_le_bytes());

        bytes
    }

    /// Create from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 20 {
            return None;
        }

        let mut offset = 0;

        // Host
        let host_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + host_len > data.len() {
            return None;
        }
        let host = String::from_utf8(data[offset..offset + host_len].to_vec()).ok()?;
        offset += host_len;

        // Port
        if offset + 2 > data.len() {
            return None;
        }
        let port = u16::from_le_bytes([data[offset], data[offset + 1]]);
        offset += 2;

        // is_open
        if offset >= data.len() {
            return None;
        }
        let is_open = data[offset] != 0;
        offset += 1;

        // change
        if offset >= data.len() {
            return None;
        }
        let change = PortStateChange::from_byte(data[offset]);
        offset += 1;

        // response_time_ms
        if offset + 4 > data.len() {
            return None;
        }
        let response_time_ms = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // Service
        if offset + 2 > data.len() {
            return None;
        }
        let service_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        let service = if service_len > 0 {
            if offset + service_len > data.len() {
                return None;
            }
            Some(String::from_utf8(data[offset..offset + service_len].to_vec()).ok()?)
        } else {
            None
        };
        offset += service_len;

        // previous_check
        if offset + 4 > data.len() {
            return None;
        }
        let previous_check = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // checked_at
        if offset + 4 > data.len() {
            return None;
        }
        let checked_at = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        offset += 4;

        // consecutive_same_state
        if offset + 2 > data.len() {
            return None;
        }
        let consecutive_same_state = u16::from_le_bytes([data[offset], data[offset + 1]]);

        Some(Self {
            host,
            port,
            is_open,
            change,
            response_time_ms,
            service,
            previous_check,
            checked_at,
            consecutive_same_state,
        })
    }
}
