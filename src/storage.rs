use crate::{
    domain::{Controls, Route, WindowSize},
    error::{Result, invalid},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash([u8; 64]);
impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self::try_from(format!("{:x}", Sha256::digest(bytes)))
            .expect("SHA-256 has 64 hexadecimal digits")
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("content hashes are hexadecimal ASCII")
    }
}
impl TryFrom<String> for ContentHash {
    type Error = crate::error::Error;
    fn try_from(s: String) -> Result<Self> {
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("Invalid cache content hash"));
        }
        Ok(Self(
            s.to_ascii_lowercase()
                .as_bytes()
                .try_into()
                .expect("validated hash length"),
        ))
    }
}
impl From<ContentHash> for String {
    fn from(hash: ContentHash) -> Self {
        hash.as_str().to_owned()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum SettingsDocument {
    #[serde(rename = "1")]
    V1(Settings),
    #[serde(rename = "2")]
    V2(Settings),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub measurements: std::collections::BTreeMap<ContentHash, String>,
    pub room: crate::measurement::Preferences,
    pub route: Option<Route>,
    pub controls: Controls,
    pub active: Option<ContentHash>,
    pub presets: std::collections::BTreeMap<ContentHash, String>,
    /// Recently activated profiles in stable slots, newest entry first.
    pub recent: std::collections::VecDeque<RecentProfile>,
    pub window: WindowSize,
    pub window_mode: crate::appearance::WindowMode,
    pub compact_window: Option<crate::domain::WindowSize>,
    pub appearance: crate::appearance::ThemePreferences,
    pub spectrum: crate::spectrum::Settings,
    pub catalog_filter: crate::catalog::CatalogFilter,
    /// An unset preference inherits the loaded correction's target family.
    pub catalog_target: Option<crate::catalog::CatalogTarget>,
    #[serde(flatten)]
    pub generation: crate::correction::GenerationSettings,
}

/// A profile activated earlier, labelled with its name at activation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentProfile {
    pub hash: ContentHash,
    pub name: String,
}
impl Settings {
    pub const RECENT: usize = 5;
    /// Puts a newly activated profile in the first slot. A profile already
    /// remembered keeps its slot, so the header row does not shift under clicks.
    pub fn remember(&mut self, hash: ContentHash, name: &str) {
        if self.recent.iter().any(|recent| recent.hash == hash) {
            return;
        }
        self.recent.push_front(RecentProfile {
            hash,
            name: name.to_owned(),
        });
        self.recent.truncate(Self::RECENT);
    }
}

fn preset_title(name: &str) -> &str {
    let Some((title_and_mode, rate)) = name.rsplit_once(" / ") else {
        return name;
    };
    if !matches!(rate, "44100 Hz" | "48000 Hz") {
        return name;
    }
    let Some((title, mode)) = title_and_mode.rsplit_once(" / ") else {
        return name;
    };
    if !title.is_empty() && matches!(mode, "AutoEq" | "Harman" | "Reference" | "Flat" | "Custom") {
        title
    } else {
        name
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}
impl Store {
    pub fn open() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("com", "zezic", "IRresponsible")
            .ok_or_else(|| invalid("No application data directory"))?;
        Self::at(dirs.data_dir())
    }
    pub fn at(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("objects"))?;
        Ok(Self {
            root: root.to_owned(),
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn load(&self) -> Result<Settings> {
        match fs::read(self.root.join("settings.json")) {
            Ok(bytes) => {
                let (SettingsDocument::V1(mut settings) | SettingsDocument::V2(mut settings)) =
                    serde_json::from_slice(&bytes)?;
                settings.room.validate()?;
                for name in settings.presets.values_mut() {
                    let length = preset_title(name).len();
                    name.truncate(length);
                }
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, settings: &Settings) -> Result<()> {
        self.write(
            "settings.json",
            &serde_json::to_vec_pretty(&SettingsDocument::V2(settings.clone()))?,
        )
    }
    pub fn write(&self, filename: &str, bytes: &[u8]) -> Result<()> {
        atomic_write(&self.root.join(filename), bytes)
    }
    pub fn put(&self, bytes: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::of(bytes);
        atomic_write(&self.root.join("objects").join(hash.as_str()), bytes)?;
        Ok(hash)
    }
    pub fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.root.join("objects").join(hash.as_str());
        if fs::metadata(&path)?.len() > 64 * 1024 * 1024 {
            return Err(invalid("Cached object exceeds 64 MiB"));
        }
        let bytes = fs::read(path)?;
        if ContentHash::of(&bytes) != *hash {
            return Err(invalid("Cached content failed its SHA-256 integrity check"));
        }
        Ok(bytes)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .expect("application files have a parent directory");
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_profiles_keep_their_slots_and_drop_the_oldest() {
        let mut settings = Settings::default();
        let hashes: Vec<_> = (0..=Settings::RECENT as u8)
            .map(|i| ContentHash::of(&[i]))
            .collect();
        for (i, hash) in hashes[..3].iter().enumerate() {
            settings.remember(*hash, &format!("P{i}"));
        }
        settings.remember(hashes[0], "renamed");
        let order = |s: &Settings| s.recent.iter().map(|r| r.hash).collect::<Vec<_>>();
        assert_eq!(order(&settings), [hashes[2], hashes[1], hashes[0]]);
        assert_eq!(settings.recent[2].name, "P0");
        for hash in &hashes[3..] {
            settings.remember(*hash, "new");
        }
        assert_eq!(settings.recent.len(), Settings::RECENT);
        assert_eq!(
            settings.recent.front().unwrap().hash,
            hashes[Settings::RECENT]
        );
        assert!(!order(&settings).contains(&hashes[0]));
    }
}
