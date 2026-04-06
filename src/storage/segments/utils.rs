use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct StringTable {
    entries: Vec<String>,
    map: HashMap<String, u32>,
}

impl StringTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn intern<S: AsRef<str>>(&mut self, value: S) -> u32 {
        let value = value.as_ref();
        if let Some(id) = self.map.get(value) {
            *id
        } else {
            let id = self.entries.len() as u32;
            self.entries.push(value.to_string());
            self.map.insert(value.to_string(), id);
            id
        }
    }

    pub fn get(&self, id: u32) -> &str {
        &self.entries[id as usize]
    }

    pub fn get_id(&self, value: &str) -> Option<u32> {
        self.map.get(value).copied()
    }

    pub fn entries(&self) -> impl Iterator<Item = &String> {
        self.entries.iter()
    }
}
