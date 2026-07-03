use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand::RngCore;

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
        let mut mac = HmacSha256::new_from_slice(&salt.0)
            .expect("HMAC can take key of any size");
        mac.update(key.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    pub fn verify(&self, plaintext_key: &str) -> bool {
        let computed = Self::compute_hash(&self.salt, plaintext_key);
        // fallback: naive compare; upgrade to subtle in MVP 1
        computed == self.hash
    }
}

/// Manages API key lifecycle in memory.
pub struct ApiKeyStore {
    entries: Vec<ApiKeyEntry>,
}

impl ApiKeyStore {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    pub fn from_plaintext_keys(keys: &[String]) -> Self {
        let entries = keys
            .iter()
            .map(|k| ApiKeyEntry::new(k, "default", "developer"))
            .collect();
        Self { entries }
    }

    /// Build from structured (key, tenant, role) tuples.
    pub fn from_structured_keys(keys: &[(String, String, String)]) -> Self {
        let entries = keys
            .iter()
            .map(|(k, t, r)| ApiKeyEntry::new(k, t, r))
            .collect();
        Self { entries }
    }

    pub fn add(&mut self, entry: ApiKeyEntry) {
        self.entries.push(entry);
    }

    pub fn verify(&self, plaintext_key: &str) -> Option<&ApiKeyEntry> {
        self.entries.iter().find(|e| e.verify(plaintext_key))
    }

    /// Look up an entry by its key_id fingerprint.
    pub fn verify_by_id(&self, key_id: &str) -> Option<&ApiKeyEntry> {
        self.entries.iter().find(|e| e.key_id == key_id)
    }

    pub fn remove_by_id(&mut self, key_id: &str) {
        self.entries.retain(|e| e.key_id != key_id)
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
