# Network Types

Purpose-built types for storing network-related data with validation and efficient binary storage.

## IpAddr

IPv4 or IPv6 address. Automatically detects version from string input.

```sql
CREATE TABLE hosts (ip IpAddr NOT NULL)
```

```rust
Value::IpAddr("10.0.0.1".parse()?)      // IPv4
Value::IpAddr("::1".parse()?)            // IPv6
```

Storage: 4 bytes (IPv4) or 16 bytes (IPv6).

## Ipv4

IPv4 address specifically (4 bytes).

```rust
Value::Ipv4("192.168.1.1".parse()?)
```

## Ipv6

IPv6 address specifically (16 bytes).

```rust
Value::Ipv6("2001:db8::1".parse()?)
```

## MacAddr

6-byte MAC address.

```sql
CREATE TABLE devices (mac MacAddr NOT NULL)
```

```rust
Value::MacAddr([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
```

## Cidr

CIDR notation combining an IPv4 address with a prefix length. Stored as 5 bytes (4 for IP + 1 for prefix).

```sql
CREATE TABLE subnets (network Cidr NOT NULL)
```

```rust
Value::Cidr("10.0.0.0", 24)  // 10.0.0.0/24
```

## Subnet

Network subnet with IP and mask (8 bytes total).

```rust
Value::Subnet("10.0.0.0", "255.255.255.0")
```

## Port

TCP/UDP port number (u16, 2 bytes). Range: 0-65535.

```sql
CREATE TABLE services (port Port NOT NULL)
```

```rust
Value::Port(443)
```

## Example: Network Inventory

```sql
CREATE TABLE network_inventory (
  hostname Text NOT NULL,
  ip IpAddr NOT NULL,
  mac MacAddr,
  subnet Cidr,
  ssh_port Port DEFAULT 22,
  mgmt_ip Ipv4
)
```

```bash
curl -X POST http://127.0.0.1:8080/collections/network_inventory/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "hostname": "web-01",
      "ip": "10.0.1.10",
      "mac": "AA:BB:CC:DD:EE:FF",
      "subnet": "10.0.1.0/24",
      "ssh_port": 22
    }
  }'
```
