//! TeamSpeak identities, kept apart from the settings in
//! ~/.config/wisp/identities.json (private keys, mode 600). Each bookmark can
//! use its own; without a choice the first one is used.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail};
use base64::prelude::*;
use serde::{Deserialize, Serialize};
use tsproto::Identity;
use tsproto_types::crypto::EccKeyPrivP256;

use crate::config;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredIdentity {
    pub id: String,
    pub name: String,
    /// Private key, base64 of the 32 raw bytes.
    pub key: String,
    /// Hash cash counter (the security level comes from it).
    pub counter: u64,
    /// Unique id, for showing.
    pub uid: String,
}

impl StoredIdentity {
    fn from_identity(name: &str, id: &Identity) -> Self {
        Self {
            id: config::new_id(),
            name: name.to_string(),
            key: BASE64_STANDARD.encode(id.key().to_short()),
            counter: id.counter(),
            uid: id.key().to_pub().get_uid(),
        }
    }

    pub fn identity(&self) -> Result<Identity> {
        let key = EccKeyPrivP256::from_short(&BASE64_STANDARD.decode(&self.key)?)?;
        Ok(Identity::new(key, self.counter))
    }

    pub fn level(&self) -> u8 {
        self.identity().map(|i| i.level()).unwrap_or(0)
    }

    /// TeamSpeak 3's export format, which its client can import.
    pub fn to_ini(&self) -> Result<String> {
        let id = self.identity()?;
        Ok(format!(
            "[Identity]\nid={}\nidentity=\"{}V{}\"\nnickname={}\n",
            self.name,
            id.counter(),
            id.key().to_ts_obfuscated(),
            self.name
        ))
    }
}

fn file() -> std::path::PathBuf {
    config::config_dir().join("identities.json")
}

pub fn load_all() -> Vec<StoredIdentity> {
    let mut list: Vec<StoredIdentity> =
        std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if list.is_empty() {
        // the single identity from before (counter:base64 key)
        let legacy = config::config_dir().join("identity");
        if let Some((counter, key)) = std::fs::read_to_string(&legacy).ok().and_then(|s| {
            let (c, k) = s.trim().split_once(':')?;
            Some((c.parse::<u64>().ok()?, k.to_string()))
        }) {
            if let Ok(k) = BASE64_STANDARD.decode(&key).map_err(anyhow::Error::from).and_then(|b| Ok(EccKeyPrivP256::from_short(&b)?)) {
                list.push(StoredIdentity::from_identity("Default", &Identity::new(k, counter)));
                save_all(&list);
            }
        }
    }
    list
}

pub fn save_all(list: &[StoredIdentity]) {
    let _ = std::fs::create_dir_all(config::config_dir());
    let Ok(json) = serde_json::to_string_pretty(list) else { return };
    let tmp = file().with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, json.as_bytes()));
    if written.is_ok() {
        let _ = std::fs::rename(tmp, file());
    }
}

/// The identity for a bookmark (by id), else the first one; a new "Default"
/// if there are none yet.
pub fn for_bookmark(id: Option<&str>) -> Result<StoredIdentity> {
    let mut list = load_all();
    if let Some(found) = id.and_then(|id| list.iter().find(|i| i.id == id)) {
        return Ok(found.clone());
    }
    if let Some(first) = list.first() {
        return Ok(first.clone());
    }
    let new = StoredIdentity::from_identity("Default", &Identity::create());
    list.push(new.clone());
    save_all(&list);
    Ok(new)
}

/// Keep a better counter (tsclientlib raised the level for a server).
pub fn update_counter(id: &str, counter: u64) {
    let mut list = load_all();
    if let Some(i) = list.iter_mut().find(|i| i.id == id && i.counter != counter) {
        i.counter = counter;
        save_all(&list);
    }
}

pub fn create(name: &str) -> StoredIdentity {
    let new = StoredIdentity::from_identity(name, &Identity::create());
    let mut list = load_all();
    list.push(new.clone());
    save_all(&list);
    new
}

/// Import a TeamSpeak 3 export (.ini) or a bare identity string
/// (`<counter>V<key>`, obfuscated or not).
pub fn import(text: &str, fallback_name: &str) -> Result<StoredIdentity> {
    let (name, identity) = parse(text, fallback_name)?;
    let mut list = load_all();
    let new = StoredIdentity::from_identity(&name, &identity);
    if list.iter().any(|i| i.uid == new.uid) {
        bail!("you already have this identity");
    }
    list.push(new.clone());
    save_all(&list);
    Ok(new)
}

fn parse(text: &str, fallback_name: &str) -> Result<(String, Identity)> {
    let mut name = fallback_name.to_string();
    let mut value = text.trim().to_string();
    if text.contains("identity=") {
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("identity=") {
                value = v.trim_matches('"').to_string();
            } else if let Some(v) = line.strip_prefix("id=") {
                name = v.trim().to_string();
            }
        }
    }
    let identity = match value.split_once('V') {
        Some((counter, key)) if counter.chars().all(|c| c.is_ascii_digit()) && !counter.is_empty() => {
            let counter: u64 = counter.parse()?;
            let key = EccKeyPrivP256::from_ts_obfuscated(key)
                .or_else(|_| EccKeyPrivP256::import_str(key))
                .context("that doesn't look like a TeamSpeak identity")?;
            Identity::new(key, counter)
        }
        _ => Identity::new_from_str(&value).context("that doesn't look like a TeamSpeak identity")?,
    };
    Ok((name, identity))
}

pub fn rename(id: &str, name: &str) {
    let mut list = load_all();
    if let Some(i) = list.iter_mut().find(|i| i.id == id) {
        i.name = name.to_string();
        save_all(&list);
    }
}

pub fn delete(id: &str) {
    let mut list = load_all();
    list.retain(|i| i.id != id);
    save_all(&list);
}

/// Search for a counter that reaches `target`, from the current one. Runs on
/// the calling thread; `tried` counts the attempts, `cancel` stops it.
pub fn raise(stored: &StoredIdentity, target: u8, tried: &Arc<AtomicU64>, cancel: &Arc<AtomicBool>) -> Option<u64> {
    let id = stored.identity().ok()?;
    let omega = id.key().to_pub().to_ts();
    let mut counter = stored.counter;
    loop {
        if tsproto::algorithms::get_hash_cash_level(&omega, counter) >= target {
            return Some(counter);
        }
        counter = counter.checked_add(1)?;
        if counter % 4096 == 0 {
            tried.store(counter - stored.counter, Ordering::Relaxed);
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_round_trip() {
        let id = Identity::create();
        let stored = StoredIdentity::from_identity("Test", &id);
        let ini = stored.to_ini().unwrap();
        let (name, back) = parse(&ini, "x").unwrap();
        assert_eq!(name, "Test");
        assert_eq!(back.key().to_pub().get_uid(), stored.uid);
        assert_eq!(back.counter(), stored.counter);
        // the bare "<counter>V<key>" form works too
        let line = ini.lines().find(|l| l.starts_with("identity=")).unwrap();
        let (_, bare) = parse(line.trim_start_matches("identity=").trim_matches('"'), "x").unwrap();
        assert_eq!(bare.key().to_pub().get_uid(), stored.uid);
    }
}
