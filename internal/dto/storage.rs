#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    /// Opaque MTP storage id; pass it back unchanged to address this storage.
    pub id: u64,
    pub name: String,
    pub capacity_bytes: u64,
    pub free_bytes: u64,
    pub is_writable: bool,
}

impl Storage {
    pub fn used_bytes(&self) -> u64 {
        self.capacity_bytes.saturating_sub(self.free_bytes)
    }
}
