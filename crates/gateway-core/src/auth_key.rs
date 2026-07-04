use std::collections::HashMap;

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Per-key secret salt stored as hex (16 bytes).
#[derive(Debug, Clone)]
pub struct Salt(pub [u8; 16]);

impl Salt {
    pub fn random() -> Self {
        let mut buf = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut buf);
        Self(buf)
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        let v = hex::decode(s).ok()?;
        let mut out = [0u8; 16];
        out.copy_from_slice(&v);
        Some(Self(out))
    }
}

/// Single key record: stored hash + salt, no plaintext key retained.
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    /// HMAC-SHA256(salt, key) stored as hex
    pub hash: String,
    pub salt: Salt,
    /// Tenant id (MVP 1 will drive allocation; default "default")
    pub tenant_id: String,
    /// Role (default "developer")
    pub role: String,
    /// Human-readable id (key fingerprint, first 8 chars of hash)
    pub key_id: String,
}

impl ApiKeyEntry {
    pub fn new(plaintext_key: &str, tenant_id: &str, role: &str) -> Self {
        let salt = Salt::random();
        let hash = Self::compute_hash(&salt, plaintext_key);
        let key_id = format!("key_{}", &hash[..8]);
        Self {
            hash,
            salt,
            tenant_id: tenant_id.into(),
            role: role.into(),
            key_id,
        }
    }

    pub fn compute_hash(salt: &Salt, key: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&salt.0).expect("HMAC can take key of any size");
        mac.update(key.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    pub fn verify(&self, plaintext_key: &str) -> bool {
        let computed = Self::compute_hash(&self.salt, plaintext_key);
        // Constant-time comparison to prevent timing side-channel attacks.
        use subtle::ConstantTimeEq;
        computed.as_bytes().ct_eq(self.hash.as_bytes()).into()
    }
}

/// Manages API key lifecycle in memory.
///
/// Lookups by `key_id` (the HMAC fingerprint stored in each entry) are
/// O(1) via the `by_id` index. The brute-force `verify(plaintext_key)`
/// path is still O(N) because each candidate must be HMAC'd before
/// comparison — that cost is inherent, not a data-structure problem.
pub struct ApiKeyStore {
    entries: Vec<ApiKeyEntry>,
    /// Reverse index: key_id → position in `entries`. Kept in sync
    /// with `entries` by all mutation methods.
    by_id: HashMap<String, usize>,
}

impl ApiKeyStore {
    pub fn new() -> Self {
        Self {
            entries: vec![],
            by_id: HashMap::new(),
        }
    }

    pub fn from_plaintext_keys(keys: &[String]) -> Self {
        let entries = keys
            .iter()
            .map(|k| ApiKeyEntry::new(k, "default", "developer"))
            .collect();
        Self::from_entries(entries)
    }

    /// Build from structured (key, tenant, role) tuples.
    pub fn from_structured_keys(keys: &[(String, String, String)]) -> Self {
        let entries = keys
            .iter()
            .map(|(k, t, r)| ApiKeyEntry::new(k, t, r))
            .collect();
        Self::from_entries(entries)
    }

    /// Internal helper: build a store + index from a list of entries.
    fn from_entries(entries: Vec<ApiKeyEntry>) -> Self {
        let mut by_id = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_id.insert(e.key_id.clone(), i);
        }
        Self { entries, by_id }
    }

    pub fn add(&mut self, entry: ApiKeyEntry) {
        let i = self.entries.len();
        self.by_id.insert(entry.key_id.clone(), i);
        self.entries.push(entry);
    }

    pub fn verify(&self, plaintext_key: &str) -> Option<&ApiKeyEntry> {
        self.entries.iter().find(|e| e.verify(plaintext_key))
    }

    /// Look up an entry by its key_id fingerprint. O(1).
    pub fn verify_by_id(&self, key_id: &str) -> Option<&ApiKeyEntry> {
        self.by_id.get(key_id).and_then(|&i| self.entries.get(i))
    }

    pub fn remove_by_id(&mut self, key_id: &str) {
        if self.by_id.remove(key_id).is_some() {
            self.entries.retain(|e| e.key_id != key_id);
            // Rebuild the index because `retain` shifted positions.
            self.by_id.clear();
            for (i, e) in self.entries.iter().enumerate() {
                self.by_id.insert(e.key_id.clone(), i);
            }
        }
    }

    pub fn list_ids(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key_id.clone()).collect()
    }

    /// List all entries (for admin UI).
    pub fn list_entries(&self) -> &Vec<ApiKeyEntry> {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_verify_valid() {
        let entry = ApiKeyEntry::new("sk-abc123", "tenant-1", "developer");
        assert!(entry.verify("sk-abc123"));
        assert!(!entry.verify("sk-wrong"));
    }

    #[test]
    fn test_different_keys_different_hashes() {
        let a = ApiKeyEntry::new("key-a", "t1", "dev");
        let b = ApiKeyEntry::new("key-b", "t1", "dev");
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn test_store_verify_and_revoke() {
        let mut store = ApiKeyStore::new();
        let entry = ApiKeyEntry::new("secret", "tenant-1", "developer");
        let id = entry.key_id.clone();
        store.add(entry);
        assert!(store.verify("secret").is_some());
        store.remove_by_id(&id);
        assert!(store.verify("secret").is_none());
    }
}
