use super::*;

impl LootSegment {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            index: HashMap::new(),
            sorted: true,
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Insert or update a loot entry by key
    pub fn insert(&mut self, entry: LootEntry) {
        let key = entry.key.clone();
        match self.index.get(&key).cloned() {
            Some(idx) => {
                self.records[idx] = entry;
            }
            None => {
                let idx = self.records.len();
                self.records.push(entry);
                self.index.insert(key, idx);
            }
        }
        self.sorted = false;
    }

    /// Alias for insert
    pub fn push(&mut self, entry: LootEntry) {
        self.insert(entry);
    }

    /// Get a loot entry by key
    pub fn get(&mut self, key: &str) -> Option<LootEntry> {
        self.ensure_index();
        self.index.get(key).map(|&idx| self.records[idx].clone())
    }

    /// Get all loot entries
    pub fn all(&mut self) -> Vec<LootEntry> {
        self.ensure_index();
        self.records.clone()
    }

    /// Filter entries by category
    pub fn by_category(&mut self, category: LootCategory) -> Vec<LootEntry> {
        self.ensure_index();
        self.records
            .iter()
            .filter(|e| e.category == category)
            .cloned()
            .collect()
    }

    /// Filter entries by target IP
    pub fn by_target(&mut self, ip: IpAddr) -> Vec<LootEntry> {
        self.ensure_index();
        self.records
            .iter()
            .filter(|e| e.target == Some(ip))
            .cloned()
            .collect()
    }

    /// Filter entries by status
    pub fn by_status(&mut self, status: LootStatus) -> Vec<LootEntry> {
        self.ensure_index();
        self.records
            .iter()
            .filter(|e| e.status == status)
            .cloned()
            .collect()
    }

    /// Delete a loot entry by key
    pub fn delete(&mut self, key: &str) -> Option<LootEntry> {
        if let Some(&idx) = self.index.get(key) {
            let entry = self.records.remove(idx);
            self.index.remove(key);
            // Rebuild index since indices shifted
            self.sorted = false;
            self.index.clear();
            for (i, record) in self.records.iter().enumerate() {
                self.index.insert(record.key.clone(), i);
            }
            Some(entry)
        } else {
            None
        }
    }

    /// Alias for delete
    pub fn remove(&mut self, key: &str) -> Option<LootEntry> {
        self.delete(key)
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }
        self.records.sort_by(|a, b| a.key.cmp(&b.key));
        self.index.clear();
        for (idx, record) in self.records.iter().enumerate() {
            self.index.insert(record.key.clone(), idx);
        }
        self.sorted = true;
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_index();
        let mut directory = Vec::with_capacity(self.records.len());
        let mut payload = Vec::new();

        for record in &self.records {
            let key_hash = hash_key(&record.key);
            let start_offset = payload.len() as u64;
            let bytes = record.to_bytes();
            write_varu32(&mut payload, bytes.len() as u32);
            payload.extend_from_slice(&bytes);
            let block_len = payload.len() as u64 - start_offset;
            directory.push(LootDirEntry {
                key_hash,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let directory_len = (directory.len() * LootDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let header = LootSegmentHeader {
            record_count: self.records.len() as u32,
            directory_len,
            payload_len,
        };

        out.clear();
        out.reserve(LootSegmentHeader::SIZE + directory.len() * LootDirEntry::SIZE + payload.len());
        header.write(out);
        LootDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < LootSegmentHeader::SIZE {
            return Err(DecodeError("loot segment too small"));
        }
        let header = LootSegmentHeader::read(bytes)?;

        let mut offset = LootSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("loot directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("loot directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("loot payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("loot payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];

        let directory = LootDirEntry::read_all(directory_bytes, header.record_count as usize)?;

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(header.record_count as usize);

        for entry in directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("loot payload slice out of bounds"));
            }
            let len = read_varu32(payload_bytes, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("loot record length mismatch"));
            }
            let record = LootEntry::from_bytes(&payload_bytes[cursor..cursor + len])?;
            cursor += len;
            if cursor != end {
                return Err(DecodeError("loot payload length mismatch"));
            }
            let key = record.key.clone();
            let idx = records.len();
            records.push(record);
            index.insert(key, idx);
        }

        Ok(Self {
            records,
            index,
            sorted: true,
        })
    }
}
