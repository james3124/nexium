//! Control-plane hardening primitives: token registry, rate limiting and the
//! bounded audit ring buffer.
//!
//! Everything in this module is deliberately free of token logging: the
//! registry never exposes raw secrets through [`TokenInfo`], and audit entries
//! only carry request metadata (never tokens, headers or bodies).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::service::ApiError;

/// Metadata for a registered token. The raw token is never included.
#[derive(Clone, Debug, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub agent: String,
    pub created_ms: u64,
}

/// A freshly minted token returned once by rotation/creation.
#[derive(Clone, Debug, Serialize)]
pub struct IssuedToken {
    pub id: String,
    pub agent: String,
    pub token: String,
    pub created_ms: u64,
}

struct TokenRecord {
    id: String,
    agent: String,
    token: String,
    created_ms: u64,
}

/// In-memory bearer token registry mapping tokens to agents.
#[derive(Default)]
pub struct TokenRegistry {
    records: HashMap<String, TokenRecord>,
    by_token: HashMap<String, String>,
    counter: u64,
}

impl TokenRegistry {
    pub fn from_config(config: &crate::config::Config) -> Self {
        let mut registry = Self::default();
        for entry in &config.api_tokens {
            let token = entry.token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some(id) = registry.by_token.get(token).cloned() {
                if let Some(record) = registry.records.get_mut(&id) {
                    record.agent = normalize_agent(Some(&entry.agent));
                }
                continue;
            }
            let agent = normalize_agent(Some(&entry.agent));
            registry.insert(token.to_owned(), agent);
        }
        registry
    }

    fn insert(&mut self, token: String, agent: String) -> String {
        self.counter += 1;
        let id = format!("tok-{}", self.counter);
        self.by_token.insert(token.clone(), id.clone());
        self.records.insert(
            id.clone(),
            TokenRecord {
                id: id.clone(),
                agent,
                token,
                created_ms: now_ms(),
            },
        );
        id
    }

    /// Resolve a provided token to its mapped agent (constant-time compare).
    pub fn lookup(&self, token: &str) -> Option<String> {
        let provided = token.as_bytes();
        let mut matched: Option<&TokenRecord> = None;
        for record in self.records.values() {
            if constant_time_eq(provided, record.token.as_bytes()) {
                matched = Some(record);
            }
        }
        matched.map(|record| record.agent.clone())
    }

    pub fn list(&self) -> Vec<TokenInfo> {
        let mut tokens: Vec<TokenInfo> = self
            .records
            .values()
            .map(|record| TokenInfo {
                id: record.id.clone(),
                agent: record.agent.clone(),
                created_ms: record.created_ms,
            })
            .collect();
        tokens.sort_by(|a, b| a.id.cmp(&b.id));
        tokens
    }

    /// Rotate an existing token (`token_id`) or mint a new one for `agent`.
    /// The previous secret stops working immediately.
    pub fn rotate(
        &mut self,
        token_id: Option<&str>,
        agent: Option<&str>,
    ) -> Result<IssuedToken, ApiError> {
        match token_id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => {
                let record = self
                    .records
                    .get_mut(id)
                    .ok_or_else(|| ApiError::not_found(format!("token not found: {id}")))?;
                let token = random_token();
                self.by_token.remove(&record.token);
                self.by_token.insert(token.clone(), id.to_owned());
                record.token = token.clone();
                record.created_ms = now_ms();
                Ok(IssuedToken {
                    id: record.id.clone(),
                    agent: record.agent.clone(),
                    token,
                    created_ms: record.created_ms,
                })
            }
            None => {
                let agent = agent
                    .map(str::trim)
                    .filter(|agent| !agent.is_empty())
                    .ok_or_else(|| {
                        ApiError::bad_request("agent is required when token_id is omitted")
                    })?;
                let token = random_token();
                let id = self.insert(token.clone(), agent.to_owned());
                Ok(IssuedToken {
                    id,
                    agent: agent.to_owned(),
                    token,
                    created_ms: now_ms(),
                })
            }
        }
    }

    pub fn revoke(&mut self, id: &str) -> bool {
        let Some(record) = self.records.remove(id) else {
            return false;
        };
        self.by_token.remove(&record.token);
        true
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// Sliding-window rate limiter keyed by agent (or agent/session).
pub struct RateLimiter {
    windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }
}

impl RateLimiter {
    /// Record a request and return `Err(retry_after_ms)` when over budget.
    /// `limit == 0` disables the limiter.
    pub fn check(&self, key: &str, limit: u32, window: Duration) -> Result<(), u64> {
        if limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let Ok(mut windows) = self.windows.lock() else {
            return Ok(());
        };
        if windows.len() > 4096 {
            windows.retain(|_, hits| {
                hits.back()
                    .is_some_and(|last| now.duration_since(*last) < window)
            });
        }
        let hits = windows.entry(key.to_owned()).or_default();
        while hits
            .front()
            .is_some_and(|first| now.duration_since(*first) >= window)
        {
            hits.pop_front();
        }
        let budget = limit as usize;
        if hits.len() >= budget {
            let retry_after = hits
                .front()
                .map(|first| window.saturating_sub(now.duration_since(*first)))
                .unwrap_or(window);
            return Err(retry_after.as_millis().max(1) as u64);
        }
        hits.push_back(now);
        Ok(())
    }
}

/// One audited API operation. Never contains tokens, headers or bodies.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp_ms: u64,
    pub agent: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
}

/// Bounded ring buffer of recent API operations.
pub struct AuditLog {
    capacity: usize,
    entries: Mutex<VecDeque<AuditEntry>>,
    total: AtomicU64,
}

impl AuditLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::new()),
            total: AtomicU64::new(0),
        }
    }

    pub fn record(
        &self,
        agent: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
        status: u16,
        duration_ms: u64,
    ) {
        let entry = AuditEntry {
            timestamp_ms: now_ms(),
            agent: agent.into(),
            method: method.into(),
            path: path.into(),
            status,
            duration_ms,
        };
        self.total.fetch_add(1, Ordering::SeqCst);
        if self.capacity == 0 {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// Newest-first snapshot, capped by `limit` and the ring capacity.
    pub fn snapshot(&self, limit: usize) -> Vec<AuditEntry> {
        let limit = limit.min(self.capacity);
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries.iter().rev().take(limit).cloned().collect()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::SeqCst)
    }
}

pub fn normalize_agent(agent: Option<&str>) -> String {
    agent
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .unwrap_or("anonymous")
        .to_owned()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut file) = std::fs::File::open("/dev/urandom")
            && file.read_exact(&mut bytes).is_ok()
        {
            return hex_encode(&bytes);
        }
    }
    // Fallback entropy: wall clock, a process-wide counter and stack noise.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let marker = 0u8;
    let seed = now_ms()
        ^ ((&marker as *const u8 as usize) as u64).rotate_left(17)
        ^ COUNTER.fetch_add(1, Ordering::SeqCst).rotate_left(33);
    let mut state = seed | 1;
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = state.to_le_bytes();
        chunk.copy_from_slice(&value[..chunk.len()]);
    }
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
