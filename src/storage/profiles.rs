//! Browser profiles: isolated cookies, storage, history and sessions.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::filesystem::FsStore;
use crate::networking::CookieRecord;
use crate::utils::{NxError, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfilePreferences {
    pub homepage: Option<String>,
    pub user_agent: Option<String>,
    pub permissions: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub url: String,
    pub title: Option<String>,
    pub timestamp_ms: u64,
}

pub struct Profile {
    name: String,
    store: FsStore,
}

impl Profile {
    pub fn open(data_dir: &Path, name: &str) -> Result<Self> {
        validate_profile_name(name)?;
        let root = data_dir.join("profiles").join(name);
        let store = FsStore::new(root)?;
        for directory in [
            "cookies",
            "storage/local",
            "storage/session",
            "cache",
            "sessions",
            "downloads",
            "history",
        ] {
            store.ensure_dir(directory)?;
        }
        Ok(Self {
            name: name.to_owned(),
            store,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn preferences(&self) -> Result<ProfilePreferences> {
        Ok(self
            .store
            .read_json::<ProfilePreferences>("preferences.json")?
            .unwrap_or_default())
    }

    pub fn save_preferences(&self, preferences: &ProfilePreferences) -> Result<()> {
        self.store.write_json("preferences.json", preferences)
    }

    pub fn load_cookies(&self) -> Result<Vec<CookieRecord>> {
        Ok(self
            .store
            .read_json::<Vec<CookieRecord>>("cookies/cookies.json")?
            .unwrap_or_default())
    }

    pub fn save_cookies(&self, cookies: &[CookieRecord]) -> Result<()> {
        let records = cookies.to_vec();
        self.store.write_json("cookies/cookies.json", &records)
    }

    pub fn load_local_storage(&self, origin: &str) -> Result<HashMap<String, String>> {
        let path = format!("storage/local/{}", origin_file(origin));
        Ok(self
            .store
            .read_json::<HashMap<String, String>>(&path)?
            .unwrap_or_default())
    }

    pub fn save_local_storage(
        &self,
        origin: &str,
        storage: &HashMap<String, String>,
    ) -> Result<()> {
        let path = format!("storage/local/{}", origin_file(origin));
        if storage.is_empty() {
            self.store.remove_file(&path)
        } else {
            self.store.write_json(&path, storage)
        }
    }

    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        Ok(self
            .store
            .read_json::<Vec<HistoryEntry>>("history/history.json")?
            .unwrap_or_default())
    }

    pub fn append_history(&self, url: &str, title: Option<&str>) -> Result<()> {
        let mut history = self.history()?;
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        history.push(HistoryEntry {
            url: url.to_owned(),
            title: title.map(str::to_owned),
            timestamp_ms,
        });
        if history.len() > 5_000 {
            let excess = history.len() - 5_000;
            history.drain(0..excess);
        }
        self.store.write_json("history/history.json", &history)
    }

    pub fn session_path(&self) -> PathBuf {
        self.store.root().join("sessions/current.json")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.store.root().join("cache")
    }

    pub fn indexeddb_dir(&self, origin: &str) -> Result<PathBuf> {
        let key = origin_file(origin);
        let key = key.trim_end_matches(".json");
        let relative = format!("storage/indexeddb/{key}");
        self.store.ensure_dir(&relative)
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.store.root().join("downloads")
    }
}

pub fn origin_file(origin: &str) -> String {
    let sanitized: String = origin
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    origin.hash(&mut hasher);
    format!("{sanitized}-{:016x}.json", hasher.finish())
}

fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(NxError::Storage(format!("invalid profile name: {name:?}")));
    }
    Ok(())
}
