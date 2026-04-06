//! GraphBinary Serialization
//!
//! Binary serialization format for graph traversal components.
//!
//! # Format
//!
//! Each value is serialized as:
//! ```text
//! ┌──────────┬───────────┬────────────┬─────────┐
//! │ type_code│ type_info │ value_flag │  value  │
//! │  (1 byte)│ (optional)│  (1 byte)  │(variable│
//! └──────────┴───────────┴────────────┴─────────┘
//! ```
//!
//! # Type Codes
//!
//! - 0x00: Custom
//! - 0x01: Int32
//! - 0x02: Int64
//! - 0x03: Float
//! - 0x04: Double
//! - 0x05: String
//! - 0x06: List
//! - 0x07: Map
//! - 0x08: Set
//! - 0x09: UUID
//! - 0x0A: Edge
//! - 0x0B: Vertex
//! - 0x0C: Path
//! - 0x0D: Property
//! - 0x0E: Traverser
//! - 0x0F: Bytecode (traversal plan)
//! - 0x10: Binding
//! - 0x11: Step (custom step type)
//!
//! # Value Flags
//!
//! - 0x00: Value present
//! - 0x01: Null value
//! - 0x02: Bulk value (for Traverser)
//!
//! # References
//!
//! - TinkerPop GraphBinary specification
//! - Gremlin bytecode format

/// Type codes for GraphBinary serialization
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TypeCode {
    /// Custom type with type-info
    Custom = 0x00,
    /// 32-bit signed integer
    Int32 = 0x01,
    /// 64-bit signed integer
    Int64 = 0x02,
    /// 32-bit float
    Float = 0x03,
    /// 64-bit double
    Double = 0x04,
    /// UTF-8 string
    String = 0x05,
    /// List of values
    List = 0x06,
    /// Key-value map
    Map = 0x07,
    /// Set of unique values
    Set = 0x08,
    /// UUID (16 bytes)
    Uuid = 0x09,
    /// Graph edge
    Edge = 0x0A,
    /// Graph vertex
    Vertex = 0x0B,
    /// Traversal path
    Path = 0x0C,
    /// Property (key-value)
    Property = 0x0D,
    /// Traverser (value + bulk + path)
    Traverser = 0x0E,
    /// Bytecode (traversal plan)
    Bytecode = 0x0F,
    /// Binding (variable -> value)
    Binding = 0x10,
    /// Step type
    Step = 0x11,
    /// Boolean
    Boolean = 0x12,
    /// Null type
    Null = 0x13,
    /// Bytes
    Bytes = 0x14,
}

impl TypeCode {
    /// Convert from byte
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(TypeCode::Custom),
            0x01 => Some(TypeCode::Int32),
            0x02 => Some(TypeCode::Int64),
            0x03 => Some(TypeCode::Float),
            0x04 => Some(TypeCode::Double),
            0x05 => Some(TypeCode::String),
            0x06 => Some(TypeCode::List),
            0x07 => Some(TypeCode::Map),
            0x08 => Some(TypeCode::Set),
            0x09 => Some(TypeCode::Uuid),
            0x0A => Some(TypeCode::Edge),
            0x0B => Some(TypeCode::Vertex),
            0x0C => Some(TypeCode::Path),
            0x0D => Some(TypeCode::Property),
            0x0E => Some(TypeCode::Traverser),
            0x0F => Some(TypeCode::Bytecode),
            0x10 => Some(TypeCode::Binding),
            0x11 => Some(TypeCode::Step),
            0x12 => Some(TypeCode::Boolean),
            0x13 => Some(TypeCode::Null),
            0x14 => Some(TypeCode::Bytes),
            _ => None,
        }
    }

    /// Get type name
    pub fn name(&self) -> &'static str {
        match self {
            TypeCode::Custom => "Custom",
            TypeCode::Int32 => "Int32",
            TypeCode::Int64 => "Int64",
            TypeCode::Float => "Float",
            TypeCode::Double => "Double",
            TypeCode::String => "String",
            TypeCode::List => "List",
            TypeCode::Map => "Map",
            TypeCode::Set => "Set",
            TypeCode::Uuid => "UUID",
            TypeCode::Edge => "Edge",
            TypeCode::Vertex => "Vertex",
            TypeCode::Path => "Path",
            TypeCode::Property => "Property",
            TypeCode::Traverser => "Traverser",
            TypeCode::Bytecode => "Bytecode",
            TypeCode::Binding => "Binding",
            TypeCode::Step => "Step",
            TypeCode::Boolean => "Boolean",
            TypeCode::Null => "Null",
            TypeCode::Bytes => "Bytes",
        }
    }
}

/// Value flags for null/bulk handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValueFlag {
    /// Value is present
    Present = 0x00,
    /// Value is null
    Null = 0x01,
    /// Value has bulk count (for Traverser)
    Bulk = 0x02,
}

impl ValueFlag {
    /// Convert from byte
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x01 => ValueFlag::Null,
            0x02 => ValueFlag::Bulk,
            _ => ValueFlag::Present,
        }
    }
}

/// Type information for custom types
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// Custom type name
    pub name: String,
    /// Schema version
    pub version: u16,
}

impl TypeInfo {
    /// Create type info
    pub fn new(name: impl Into<String>, version: u16) -> Self {
        Self {
            name: name.into(),
            version,
        }
    }

    /// Serialize type info
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        // Length-prefixed name
        let name_bytes = self.name.as_bytes();
        buf.extend(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend(name_bytes);
        // Version
        buf.extend(&self.version.to_le_bytes());
        buf
    }

    /// Deserialize type info
    pub fn from_bytes(data: &[u8]) -> Option<(Self, usize)> {
        if data.len() < 4 {
            return None;
        }

        let name_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 4 + name_len {
            return None;
        }

        let name = String::from_utf8(data[2..2 + name_len].to_vec()).ok()?;
        let version = u16::from_le_bytes([data[2 + name_len], data[3 + name_len]]);

        Some((Self { name, version }, 4 + name_len))
    }
}

/// Binary value representation
#[derive(Debug, Clone)]
pub enum BinaryValue {
    /// Null value
    Null,
    /// Boolean
    Boolean(bool),
    /// 32-bit integer
    Int32(i32),
    /// 64-bit integer
    Int64(i64),
    /// Float
    Float(f32),
    /// Double
    Double(f64),
    /// String
    String(String),
    /// Bytes
    Bytes(Vec<u8>),
    /// List of values
    List(Vec<BinaryValue>),
    /// Map of key-value pairs
    Map(Vec<(BinaryValue, BinaryValue)>),
    /// UUID (16 bytes)
    Uuid([u8; 16]),
    /// Vertex with ID and label
    Vertex { id: Box<BinaryValue>, label: String },
    /// Edge with ID, label, in/out vertices
    Edge {
        id: Box<BinaryValue>,
        label: String,
        in_v: Box<BinaryValue>,
        out_v: Box<BinaryValue>,
    },
    /// Property (key, value)
    Property {
        key: String,
        value: Box<BinaryValue>,
    },
    /// Traverser (value, bulk, path)
    Traverser { value: Box<BinaryValue>, bulk: u64 },
    /// Bytecode instruction
    Bytecode(Bytecode),
    /// Binding (name -> value)
    Binding {
        name: String,
        value: Box<BinaryValue>,
    },
    /// Custom type
    Custom { type_info: TypeInfo, data: Vec<u8> },
}

impl BinaryValue {
    /// Get type code for this value
    pub fn type_code(&self) -> TypeCode {
        match self {
            BinaryValue::Null => TypeCode::Null,
            BinaryValue::Boolean(_) => TypeCode::Boolean,
            BinaryValue::Int32(_) => TypeCode::Int32,
            BinaryValue::Int64(_) => TypeCode::Int64,
            BinaryValue::Float(_) => TypeCode::Float,
            BinaryValue::Double(_) => TypeCode::Double,
            BinaryValue::String(_) => TypeCode::String,
            BinaryValue::Bytes(_) => TypeCode::Bytes,
            BinaryValue::List(_) => TypeCode::List,
            BinaryValue::Map(_) => TypeCode::Map,
            BinaryValue::Uuid(_) => TypeCode::Uuid,
            BinaryValue::Vertex { .. } => TypeCode::Vertex,
            BinaryValue::Edge { .. } => TypeCode::Edge,
            BinaryValue::Property { .. } => TypeCode::Property,
            BinaryValue::Traverser { .. } => TypeCode::Traverser,
            BinaryValue::Bytecode(_) => TypeCode::Bytecode,
            BinaryValue::Binding { .. } => TypeCode::Binding,
            BinaryValue::Custom { .. } => TypeCode::Custom,
        }
    }

    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, BinaryValue::Null)
    }
}

/// Bytecode representation for traversal plans
#[derive(Debug, Clone)]
pub struct Bytecode {
    /// Source instructions (start of traversal)
    pub sources: Vec<Instruction>,
    /// Step instructions
    pub steps: Vec<Instruction>,
}

impl Bytecode {
    /// Create empty bytecode
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// Add source instruction (e.g., V(), E())
    pub fn add_source(&mut self, name: &str, args: Vec<BinaryValue>) {
        self.sources.push(Instruction::new(name, args));
    }

    /// Add step instruction
    pub fn add_step(&mut self, name: &str, args: Vec<BinaryValue>) {
        self.steps.push(Instruction::new(name, args));
    }
}

impl Default for Bytecode {
    fn default() -> Self {
        Self::new()
    }
}

/// Single bytecode instruction
#[derive(Debug, Clone)]
pub struct Instruction {
    /// Step name (e.g., "V", "out", "has", "select")
    pub name: String,
    /// Arguments
    pub args: Vec<BinaryValue>,
}

impl Instruction {
    /// Create new instruction
    pub fn new(name: impl Into<String>, args: Vec<BinaryValue>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// Binary serializer
pub struct BinarySerializer;

impl BinarySerializer {
    /// Serialize value to bytes
    pub fn serialize(value: &BinaryValue) -> Vec<u8> {
        let mut buf = Vec::new();
        Self::write_value(&mut buf, value);
        buf
    }

    /// Write value to buffer
    fn write_value(buf: &mut Vec<u8>, value: &BinaryValue) {
        // Write type code
        buf.push(value.type_code() as u8);

        match value {
            BinaryValue::Null => {
                buf.push(ValueFlag::Null as u8);
            }
            BinaryValue::Boolean(b) => {
                buf.push(ValueFlag::Present as u8);
                buf.push(if *b { 1 } else { 0 });
            }
            BinaryValue::Int32(i) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&i.to_le_bytes());
            }
            BinaryValue::Int64(i) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&i.to_le_bytes());
            }
            BinaryValue::Float(f) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&f.to_le_bytes());
            }
            BinaryValue::Double(d) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&d.to_le_bytes());
            }
            BinaryValue::String(s) => {
                buf.push(ValueFlag::Present as u8);
                Self::write_string(buf, s);
            }
            BinaryValue::Bytes(b) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&(b.len() as u32).to_le_bytes());
                buf.extend(b);
            }
            BinaryValue::List(items) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&(items.len() as u32).to_le_bytes());
                for item in items {
                    Self::write_value(buf, item);
                }
            }
            BinaryValue::Map(pairs) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(&(pairs.len() as u32).to_le_bytes());
                for (k, v) in pairs {
                    Self::write_value(buf, k);
                    Self::write_value(buf, v);
                }
            }
            BinaryValue::Uuid(bytes) => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(bytes);
            }
            BinaryValue::Vertex { id, label } => {
                buf.push(ValueFlag::Present as u8);
                Self::write_value(buf, id);
                Self::write_string(buf, label);
            }
            BinaryValue::Edge {
                id,
                label,
                in_v,
                out_v,
            } => {
                buf.push(ValueFlag::Present as u8);
                Self::write_value(buf, id);
                Self::write_string(buf, label);
                Self::write_value(buf, in_v);
                Self::write_value(buf, out_v);
            }
            BinaryValue::Property { key, value } => {
                buf.push(ValueFlag::Present as u8);
                Self::write_string(buf, key);
                Self::write_value(buf, value);
            }
            BinaryValue::Traverser { value, bulk } => {
                buf.push(ValueFlag::Bulk as u8);
                buf.extend(&bulk.to_le_bytes());
                Self::write_value(buf, value);
            }
            BinaryValue::Bytecode(bc) => {
                buf.push(ValueFlag::Present as u8);
                Self::write_bytecode(buf, bc);
            }
            BinaryValue::Binding { name, value } => {
                buf.push(ValueFlag::Present as u8);
                Self::write_string(buf, name);
                Self::write_value(buf, value);
            }
            BinaryValue::Custom { type_info, data } => {
                buf.push(ValueFlag::Present as u8);
                buf.extend(type_info.to_bytes());
                buf.extend(&(data.len() as u32).to_le_bytes());
                buf.extend(data);
            }
        }
    }

    /// Write length-prefixed string
    fn write_string(buf: &mut Vec<u8>, s: &str) {
        let bytes = s.as_bytes();
        buf.extend(&(bytes.len() as u32).to_le_bytes());
        buf.extend(bytes);
    }

    /// Write bytecode
    fn write_bytecode(buf: &mut Vec<u8>, bc: &Bytecode) {
        // Sources count
        buf.extend(&(bc.sources.len() as u32).to_le_bytes());
        for inst in &bc.sources {
            Self::write_instruction(buf, inst);
        }

        // Steps count
        buf.extend(&(bc.steps.len() as u32).to_le_bytes());
        for inst in &bc.steps {
            Self::write_instruction(buf, inst);
        }
    }

    /// Write single instruction
    fn write_instruction(buf: &mut Vec<u8>, inst: &Instruction) {
        Self::write_string(buf, &inst.name);
        buf.extend(&(inst.args.len() as u32).to_le_bytes());
        for arg in &inst.args {
            Self::write_value(buf, arg);
        }
    }
}

/// Binary deserializer
pub struct BinaryDeserializer;

impl BinaryDeserializer {
    /// Deserialize value from bytes
    pub fn deserialize(data: &[u8]) -> Option<(BinaryValue, usize)> {
        Self::read_value(data)
    }

    /// Read value from buffer
    fn read_value(data: &[u8]) -> Option<(BinaryValue, usize)> {
        if data.len() < 2 {
            return None;
        }

        let type_code = TypeCode::from_byte(data[0])?;
        let flag = ValueFlag::from_byte(data[1]);

        // Handle null
        if flag == ValueFlag::Null {
            return Some((BinaryValue::Null, 2));
        }

        let mut offset = 2;

        let value = match type_code {
            TypeCode::Null => BinaryValue::Null,
            TypeCode::Boolean => {
                if data.len() < offset + 1 {
                    return None;
                }
                let b = data[offset] != 0;
                offset += 1;
                BinaryValue::Boolean(b)
            }
            TypeCode::Int32 => {
                if data.len() < offset + 4 {
                    return None;
                }
                let i = i32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
                offset += 4;
                BinaryValue::Int32(i)
            }
            TypeCode::Int64 => {
                if data.len() < offset + 8 {
                    return None;
                }
                let i = i64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
                offset += 8;
                BinaryValue::Int64(i)
            }
            TypeCode::Float => {
                if data.len() < offset + 4 {
                    return None;
                }
                let f = f32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
                offset += 4;
                BinaryValue::Float(f)
            }
            TypeCode::Double => {
                if data.len() < offset + 8 {
                    return None;
                }
                let d = f64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
                offset += 8;
                BinaryValue::Double(d)
            }
            TypeCode::String => {
                let (s, len) = Self::read_string(&data[offset..])?;
                offset += len;
                BinaryValue::String(s)
            }
            TypeCode::Bytes => {
                if data.len() < offset + 4 {
                    return None;
                }
                let len = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
                offset += 4;
                if data.len() < offset + len {
                    return None;
                }
                let bytes = data[offset..offset + len].to_vec();
                offset += len;
                BinaryValue::Bytes(bytes)
            }
            TypeCode::List => {
                if data.len() < offset + 4 {
                    return None;
                }
                let count = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
                offset += 4;

                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    let (item, len) = Self::read_value(&data[offset..])?;
                    offset += len;
                    items.push(item);
                }
                BinaryValue::List(items)
            }
            TypeCode::Map => {
                if data.len() < offset + 4 {
                    return None;
                }
                let count = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
                offset += 4;

                let mut pairs = Vec::with_capacity(count);
                for _ in 0..count {
                    let (key, len1) = Self::read_value(&data[offset..])?;
                    offset += len1;
                    let (val, len2) = Self::read_value(&data[offset..])?;
                    offset += len2;
                    pairs.push((key, val));
                }
                BinaryValue::Map(pairs)
            }
            TypeCode::Uuid => {
                if data.len() < offset + 16 {
                    return None;
                }
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&data[offset..offset + 16]);
                offset += 16;
                BinaryValue::Uuid(bytes)
            }
            TypeCode::Traverser => {
                // Bulk count
                if data.len() < offset + 8 {
                    return None;
                }
                let bulk = u64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
                offset += 8;
                // Value
                let (value, len) = Self::read_value(&data[offset..])?;
                offset += len;
                BinaryValue::Traverser {
                    value: Box::new(value),
                    bulk,
                }
            }
            TypeCode::Bytecode => {
                let (bc, len) = Self::read_bytecode(&data[offset..])?;
                offset += len;
                BinaryValue::Bytecode(bc)
            }
            _ => return None, // Other types not fully implemented
        };

        Some((value, offset))
    }

    /// Read length-prefixed string
    fn read_string(data: &[u8]) -> Option<(String, usize)> {
        if data.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        if data.len() < 4 + len {
            return None;
        }
        let s = String::from_utf8(data[4..4 + len].to_vec()).ok()?;
        Some((s, 4 + len))
    }

    /// Read bytecode
    fn read_bytecode(data: &[u8]) -> Option<(Bytecode, usize)> {
        let mut offset = 0;

        // Sources
        if data.len() < 4 {
            return None;
        }
        let source_count = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        offset += 4;

        let mut sources = Vec::with_capacity(source_count);
        for _ in 0..source_count {
            let (inst, len) = Self::read_instruction(&data[offset..])?;
            offset += len;
            sources.push(inst);
        }

        // Steps
        if data.len() < offset + 4 {
            return None;
        }
        let step_count = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;

        let mut steps = Vec::with_capacity(step_count);
        for _ in 0..step_count {
            let (inst, len) = Self::read_instruction(&data[offset..])?;
            offset += len;
            steps.push(inst);
        }

        Some((Bytecode { sources, steps }, offset))
    }

    /// Read single instruction
    fn read_instruction(data: &[u8]) -> Option<(Instruction, usize)> {
        let mut offset = 0;

        let (name, len) = Self::read_string(data)?;
        offset += len;

        if data.len() < offset + 4 {
            return None;
        }
        let arg_count = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;

        let mut args = Vec::with_capacity(arg_count);
        for _ in 0..arg_count {
            let (arg, len) = Self::read_value(&data[offset..])?;
            offset += len;
            args.push(arg);
        }

        Some((Instruction::new(name, args), offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_code_roundtrip() {
        for code in [
            TypeCode::Int32,
            TypeCode::Int64,
            TypeCode::String,
            TypeCode::List,
            TypeCode::Null,
        ] {
            assert_eq!(TypeCode::from_byte(code as u8), Some(code));
        }
    }

    #[test]
    fn test_serialize_int32() {
        let value = BinaryValue::Int32(42);
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        match decoded {
            BinaryValue::Int32(i) => assert_eq!(i, 42),
            _ => panic!("Expected Int32"),
        }
    }

    #[test]
    fn test_serialize_string() {
        let value = BinaryValue::String("hello".to_string());
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        match decoded {
            BinaryValue::String(s) => assert_eq!(s, "hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_serialize_list() {
        let value = BinaryValue::List(vec![
            BinaryValue::Int32(1),
            BinaryValue::Int32(2),
            BinaryValue::Int32(3),
        ]);
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        match decoded {
            BinaryValue::List(items) => {
                assert_eq!(items.len(), 3);
            }
            _ => panic!("Expected List"),
        }
    }

    #[test]
    fn test_serialize_null() {
        let value = BinaryValue::Null;
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        assert!(decoded.is_null());
    }

    #[test]
    fn test_serialize_traverser() {
        let value = BinaryValue::Traverser {
            value: Box::new(BinaryValue::String("vertex1".to_string())),
            bulk: 5,
        };
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        match decoded {
            BinaryValue::Traverser { bulk, .. } => assert_eq!(bulk, 5),
            _ => panic!("Expected Traverser"),
        }
    }

    #[test]
    fn test_bytecode_serialization() {
        let mut bc = Bytecode::new();
        bc.add_source("V", vec![]);
        bc.add_step("out", vec![BinaryValue::String("knows".to_string())]);
        bc.add_step(
            "has",
            vec![
                BinaryValue::String("name".to_string()),
                BinaryValue::String("alice".to_string()),
            ],
        );

        let value = BinaryValue::Bytecode(bc);
        let bytes = BinarySerializer::serialize(&value);

        let (decoded, _) = BinaryDeserializer::deserialize(&bytes).unwrap();
        match decoded {
            BinaryValue::Bytecode(bc) => {
                assert_eq!(bc.sources.len(), 1);
                assert_eq!(bc.steps.len(), 2);
                assert_eq!(bc.sources[0].name, "V");
                assert_eq!(bc.steps[0].name, "out");
                assert_eq!(bc.steps[1].name, "has");
            }
            _ => panic!("Expected Bytecode"),
        }
    }

    #[test]
    fn test_type_info() {
        let info = TypeInfo::new("MyCustomType", 1);
        let bytes = info.to_bytes();

        let (decoded, _) = TypeInfo::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.name, "MyCustomType");
        assert_eq!(decoded.version, 1);
    }
}
