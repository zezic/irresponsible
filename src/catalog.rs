use crate::{
    correction::{Correction, Curve, GenerationSettings, Origin, generated_impulse},
    domain::{Rate, Target},
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

mod targets;
pub use targets::{MeasurementReference, ReferenceTarget, TargetName};

pub const UPSTREAM_REVISION: &str = "7ae0f56d53074872b028649617a22bbb4232feb7";
const RAW: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq";

/// Files published for an exact result entry at the catalog revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Asset {
    AutoEq(Rate),
    Measurements,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogFilter {
    #[default]
    Any,
    AutoEq,
    Measurements,
    Both,
}
impl CatalogFilter {
    pub const ALL: [Self; 4] = [Self::Any, Self::AutoEq, Self::Measurements, Self::Both];
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "All entries",
            Self::AutoEq => "Require AutoEq",
            Self::Measurements => "Require measurements",
            Self::Both => "Require both",
        }
    }
    pub fn matches(self, entry: &Entry) -> bool {
        match self {
            Self::Any => true,
            Self::AutoEq => entry.has_autoeq(),
            Self::Measurements => entry.has_measurements(),
            Self::Both => entry.has_autoeq() && entry.has_measurements(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogTarget {
    #[default]
    AutoEq,
    Reference,
    Flat,
}
impl CatalogTarget {
    pub fn resolve(self, entry: &Entry, rate: Rate) -> Option<Target> {
        match self {
            Self::AutoEq if entry.assets.is_none() || entry.supports(Target::AutoEq, rate) => {
                Some(Target::AutoEq)
            }
            Self::Reference => [Target::Harman, Target::Reference]
                .into_iter()
                .find(|target| entry.supports(*target, rate)),
            Self::Flat if entry.supports(Target::Flat, rate) => Some(Target::Flat),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::AutoEq => "AutoEq",
            Self::Reference => "reference target",
            Self::Flat => "Literal Flat",
        }
    }
    pub fn includes(self, target: Target) -> bool {
        matches!(
            (self, target),
            (Self::AutoEq, Target::AutoEq)
                | (Self::Reference, Target::Harman | Target::Reference)
                | (Self::Flat, Target::Flat)
        )
    }
}

type Inventory = BTreeMap<String, BTreeSet<Asset>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub source: String,
    pub recommended: bool,
    /// None until the upstream file inventory has been retrieved.
    #[serde(default)]
    pub assets: Option<BTreeSet<Asset>>,
    #[serde(default)]
    pub reference: Option<MeasurementReference>,
}
impl Entry {
    pub fn has_autoeq(&self) -> bool {
        self.assets
            .as_ref()
            .is_some_and(|assets| assets.iter().any(|asset| matches!(asset, Asset::AutoEq(_))))
    }
    pub fn has_measurements(&self) -> bool {
        self.assets
            .as_ref()
            .is_some_and(|assets| assets.contains(&Asset::Measurements))
    }
    pub fn supports(&self, target: Target, rate: Rate) -> bool {
        match target {
            Target::AutoEq => self
                .assets
                .as_ref()
                .is_some_and(|assets| assets.contains(&Asset::AutoEq(rate))),
            Target::Harman | Target::Reference => {
                self.has_measurements()
                    && self
                        .reference
                        .as_ref()
                        .is_some_and(|reference| reference.target(target).is_some())
            }
            Target::Flat => self.has_measurements() && self.reference.is_some(),
            Target::Custom => false,
        }
    }

    pub fn reference(&self) -> Result<&MeasurementReference> {
        self.reference.as_ref().ok_or_else(|| invalid("AutoEq does not document a compatible target for this measurement reference. Use the published AutoEq filter."))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub revision: String,
    pub entries: Vec<Entry>,
    targets: Vec<ReferenceTarget>,
}
impl Catalog {
    pub fn parse(index: &str, recommended: &str, revision: &str) -> Result<Self> {
        let recommended: BTreeSet<_> = recommended
            .lines()
            .filter_map(link)
            .map(|(_, path)| path.to_owned())
            .collect();
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for line in index.lines().filter(|line| line.starts_with("- [")) {
            let (name, path) = link(line).ok_or_else(|| invalid("Unsupported AutoEq index row"))?;
            if !safe_path(path) || !seen.insert(path.to_owned()) {
                return Err(invalid("Invalid or duplicate AutoEq result path"));
            }
            let source = line
                .rsplit_once(") by ")
                .map(|(_, source)| source)
                .unwrap_or("Unknown source");
            entries.push(Entry {
                name: name.to_owned(),
                path: path.to_owned(),
                source: source.to_owned(),
                recommended: recommended.contains(path),
                assets: None,
                reference: None,
            });
        }
        if entries.is_empty() || entries.len() > 50000 {
            return Err(invalid("AutoEq index is empty or exceeds 50000 entries"));
        }
        entries.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(b.recommended.cmp(&a.recommended))
                .then(a.source.cmp(&b.source))
        });
        let catalog = Self {
            revision: revision.to_owned(),
            entries,
            targets: Vec::new(),
        };
        if revision == UPSTREAM_REVISION {
            catalog.with_targets(include_bytes!("../assets/autoeq-targets.json"))
        } else {
            Ok(catalog)
        }
    }
    pub fn with_targets(mut self, bytes: &[u8]) -> Result<Self> {
        self.targets = ReferenceTarget::parse(bytes)?;
        targets::assign(&mut self.entries, &self.targets);
        Ok(self)
    }
    pub fn target(&self, entry: &Entry, kind: Target) -> Result<&ReferenceTarget> {
        let name = entry.reference()?.target(kind).ok_or_else(|| {
            invalid("This target is not compatible with the entry's measurement reference")
        })?;
        Ok(self
            .targets
            .iter()
            .find(|target| target.name() == name)
            .expect("assigned target belongs to catalog"))
    }
    pub fn metadata_known(&self) -> bool {
        !self.targets.is_empty()
    }
    pub fn with_inventory(mut self, bytes: &[u8]) -> Result<Self> {
        self.apply_inventory(&serde_json::from_slice(bytes)?)?;
        Ok(self)
    }
    pub fn inventory_known(&self) -> bool {
        self.entries.iter().all(|entry| entry.assets.is_some())
    }
    fn apply_inventory(&mut self, inventory: &Inventory) -> Result<()> {
        for entry in &mut self.entries {
            let decoded = percent_encoding::percent_decode_str(&entry.path)
                .decode_utf8()
                .map_err(|e| invalid(e.to_string()))?;
            entry.assets = Some(inventory.get(decoded.as_ref()).cloned().unwrap_or_default());
        }
        Ok(())
    }
    pub fn filtered<'a>(
        &'a self,
        query: &str,
        filter: CatalogFilter,
    ) -> impl Iterator<Item = (usize, &'a Entry)> + 'a {
        self.search(query)
            .filter(move |(_, entry)| filter.matches(entry))
    }
    pub fn search<'a>(&'a self, query: &str) -> impl Iterator<Item = (usize, &'a Entry)> + 'a {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries.iter().enumerate().filter(move |(_, e)| {
            let name = e.name.to_lowercase();
            let source = e.source.to_lowercase();
            terms.iter().all(|term| {
                name.contains(term)
                    || (!term.bytes().any(|b| b.is_ascii_digit()) && source.contains(term))
            })
        })
    }
}

fn link(line: &str) -> Option<(&str, &str)> {
    let text = line.strip_prefix("- [")?;
    let (name, rest) = text.split_once("](")?;
    let rest = rest.strip_prefix("./")?;
    let path = rest
        .rsplit_once(") by ")
        .map(|(path, _)| path)
        .or_else(|| rest.strip_suffix(')'))?;
    Some((name, path))
}
fn safe_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains("..")
        && !path.contains(['?', '#', '\\', '\n', '\r'])
        && path.split('/').count() == 3
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TreeKind {
    Tree,
    Blob,
    Commit,
}
#[derive(Deserialize)]
struct GitTreeEntry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: TreeKind,
}
#[derive(Deserialize)]
struct GitTree {
    truncated: bool,
    tree: Vec<GitTreeEntry>,
}
impl GitTree {
    fn inventory(&self) -> Result<Inventory> {
        if self.truncated {
            return Err(invalid("Incomplete upstream asset inventory"));
        }
        let mut inventory = Inventory::new();
        for entry in self
            .tree
            .iter()
            .filter(|entry| entry.kind == TreeKind::Blob)
        {
            let Some((directory, filename)) = entry.path.rsplit_once('/') else {
                continue;
            };
            let name = directory
                .rsplit('/')
                .next()
                .expect("result directory has a name");
            let asset = if filename == format!("{name}.csv") {
                Asset::Measurements
            } else if filename == format!("{name} minimum phase 44100Hz.wav") {
                Asset::AutoEq(Rate::Hz44100)
            } else if filename == format!("{name} minimum phase 48000Hz.wav") {
                Asset::AutoEq(Rate::Hz48000)
            } else {
                continue;
            };
            inventory
                .entry(directory.to_owned())
                .or_default()
                .insert(asset);
        }
        Ok(inventory)
    }
}

pub struct Source {
    client: Client,
    store: Store,
}
#[derive(Serialize, Deserialize)]
struct CatalogCache {
    revision: String,
    index: ContentHash,
    recommended: ContentHash,
    #[serde(default)]
    inventory: Option<ContentHash>,
    #[serde(default)]
    targets: Option<ContentHash>,
}
impl Source {
    pub fn new(store: Store) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(25))
                .connect_timeout(Duration::from_secs(8))
                .user_agent("IRresponsible/0.1")
                .build()?,
            store,
        })
    }
    pub fn cached_catalog(&self) -> Result<Catalog> {
        let manifest: CatalogCache =
            serde_json::from_slice(&std::fs::read(self.store.root().join("catalog.json"))?)?;
        let index = self.store.get(&manifest.index)?;
        let recommended = self.store.get(&manifest.recommended)?;
        let mut catalog = Catalog::parse(
            std::str::from_utf8(&index).map_err(|e| invalid(e.to_string()))?,
            std::str::from_utf8(&recommended).map_err(|e| invalid(e.to_string()))?,
            &manifest.revision,
        )?;
        if let Some(hash) = manifest.inventory {
            catalog = catalog.with_inventory(&self.store.get(&hash)?)?;
        }
        if manifest.revision != UPSTREAM_REVISION
            && let Some(hash) = manifest.targets
        {
            catalog = catalog.with_targets(&self.store.get(&hash)?)?;
        }
        Ok(catalog)
    }
    pub fn refresh(&self, cancel: &AtomicBool) -> Result<Catalog> {
        if cancel.load(Ordering::Relaxed) {
            return Err(invalid("Cancelled"));
        }
        #[derive(Deserialize)]
        struct Commit {
            sha: String,
        }
        let revision: Commit = self
            .client
            .get("https://api.github.com/repos/jaakkopasanen/AutoEq/commits/master")
            .send()?
            .error_for_status()?
            .json()?;
        if revision.sha.len() != 40 || !revision.sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("Invalid upstream revision"));
        }
        let index = self.download(&format!("{RAW}/{}/results/INDEX.md", revision.sha), cancel)?;
        let recommended =
            self.download(&format!("{RAW}/{}/results/README.md", revision.sha), cancel)?;
        let mut catalog = Catalog::parse(
            std::str::from_utf8(&index).map_err(|e| invalid(e.to_string()))?,
            std::str::from_utf8(&recommended).map_err(|e| invalid(e.to_string()))?,
            &revision.sha,
        )?;
        let root = self.tree(&revision.sha, false, cancel)?;
        let results = root
            .tree
            .iter()
            .find(|entry| entry.path == "results" && entry.kind == TreeKind::Tree)
            .ok_or_else(|| invalid("Upstream results tree is missing"))?;
        let providers = self.tree(&results.sha, false, cancel)?;
        let mut inventory = Inventory::new();
        for provider in providers
            .tree
            .iter()
            .filter(|entry| entry.kind == TreeKind::Tree)
        {
            for (path, assets) in self.inventory(&provider.sha, cancel)? {
                inventory.insert(format!("{}/{path}", provider.path), assets);
            }
        }
        catalog.apply_inventory(&inventory)?;
        let target_bytes = if revision.sha == UPSTREAM_REVISION {
            include_bytes!("../assets/autoeq-targets.json").to_vec()
        } else {
            self.asset(&revision.sha, "webapp/data/targets.json", cancel)?
        };
        catalog = catalog.with_targets(&target_bytes)?;
        let manifest = CatalogCache {
            revision: revision.sha,
            index: self.store.put(&index)?,
            recommended: self.store.put(&recommended)?,
            inventory: Some(self.store.put(&serde_json::to_vec(&inventory)?)?),
            targets: Some(self.store.put(&target_bytes)?),
        };
        self.store
            .write("catalog.json", &serde_json::to_vec(&manifest)?)?;
        Ok(catalog)
    }
    fn tree(&self, sha: &str, recursive: bool, cancel: &AtomicBool) -> Result<GitTree> {
        let query = if recursive { "?recursive=1" } else { "" };
        let bytes = self.download_limited(
            &format!("https://api.github.com/repos/jaakkopasanen/AutoEq/git/trees/{sha}{query}"),
            cancel,
            32 * 1024 * 1024,
        )?;
        let tree: GitTree = serde_json::from_slice(&bytes)?;
        for entry in &tree.tree {
            if entry.sha.len() != 40
                || !entry.sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                || entry
                    .path
                    .split('/')
                    .any(|part| matches!(part, "" | "." | ".."))
            {
                return Err(invalid("Invalid upstream tree entry"));
            }
        }
        Ok(tree)
    }
    fn complete_tree(&self, sha: &str, cancel: &AtomicBool) -> Result<GitTree> {
        let tree = self.tree(sha, true, cancel)?;
        if !tree.truncated {
            return Ok(tree);
        }
        let children = self.tree(sha, false, cancel)?;
        let mut files = Vec::new();
        for child in children.tree {
            match child.kind {
                TreeKind::Tree => {
                    for mut file in self.complete_tree(&child.sha, cancel)?.tree {
                        file.path = format!("{}/{}", child.path, file.path);
                        files.push(file);
                    }
                }
                TreeKind::Blob => files.push(child),
                TreeKind::Commit => {}
            }
        }
        Ok(GitTree {
            truncated: false,
            tree: files,
        })
    }
    fn inventory(&self, sha: &str, cancel: &AtomicBool) -> Result<Inventory> {
        let filename = format!("inventory-{sha}.json");
        match std::fs::read(self.store.root().join(&filename)) {
            Ok(bytes) => {
                return Ok(serde_json::from_slice(
                    &self
                        .store
                        .get(&serde_json::from_slice::<ContentHash>(&bytes)?)?,
                )?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let inventory = self.complete_tree(sha, cancel)?.inventory()?;
        let hash = self.store.put(&serde_json::to_vec(&inventory)?)?;
        self.store.write(&filename, &serde_json::to_vec(&hash)?)?;
        Ok(inventory)
    }
    fn download(&self, url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
        self.download_limited(url, cancel, 8 * 1024 * 1024)
    }
    fn download_limited(&self, url: &str, cancel: &AtomicBool, limit: usize) -> Result<Vec<u8>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(invalid("Cancelled"));
        }
        let mut response = self.client.get(url).send()?.error_for_status()?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(invalid("Cancelled"));
            }
            let count = response.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > limit {
                return Err(invalid("Upstream response exceeds its size limit"));
            }
        }
        Ok(bytes)
    }
    fn asset(&self, revision: &str, path: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
        let key = ContentHash::of(format!("{revision}/{path}").as_bytes());
        let manifest_path = self
            .store
            .root()
            .join(format!("asset-{}.json", key.as_str()));
        match std::fs::read(&manifest_path) {
            Ok(bytes) => {
                return self
                    .store
                    .get(&serde_json::from_slice::<ContentHash>(&bytes)?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let bytes = self.download(&format!("{RAW}/{revision}/{path}"), cancel)?;
        let hash = self.store.put(&bytes)?;
        crate::storage::atomic_write(&manifest_path, &serde_json::to_vec(&hash)?)?;
        Ok(bytes)
    }
    pub fn prepare(
        &self,
        catalog: &Catalog,
        entry: &Entry,
        target: Target,
        rate: Rate,
        generation: GenerationSettings,
        cancel: &AtomicBool,
    ) -> Result<Correction> {
        if entry.assets.is_some() && !entry.supports(target, rate) {
            return Err(invalid(
                "This entry does not publish the assets needed for this target and sample rate.",
            ));
        }
        match target {
            Target::AutoEq => {
                let filename = entry
                    .path
                    .rsplit('/')
                    .next()
                    .expect("validated result path");
                let asset = format!(
                    "results/{}/{filename}%20minimum%20phase%20{}Hz.wav",
                    entry.path,
                    rate.hz()
                );
                let bytes = self.asset(&catalog.revision, &asset, cancel).map_err(|error| {
                    if matches!(&error, crate::error::Error::Network(e) if e.status() == Some(reqwest::StatusCode::NOT_FOUND)) {
                        invalid(format!("AutoEq's published {} Hz filter for {} is unavailable upstream. Refresh the library or choose another provider.", rate.hz(), entry.name))
                    } else { error }
                })?;
                return Ok(
                    Correction::decode_wav(&bytes, &entry.name, rate)?.into_published(
                        entry.name.clone(),
                        catalog.revision.clone(),
                        entry.path.clone(),
                        entry.source.clone(),
                    ),
                );
            }
            Target::Custom => return Err(invalid("Custom corrections require a WAV import")),
            Target::Harman | Target::Reference | Target::Flat => {}
        }
        entry.reference()?;
        let filename = entry.path.rsplit('/').next().expect("validated path");
        let measurement_bytes = self.asset(&catalog.revision, &format!("results/{}/{filename}.csv", entry.path), cancel).map_err(|error| {
            if matches!(&error, crate::error::Error::Network(e) if e.status() == Some(reqwest::StatusCode::NOT_FOUND)) {
                invalid("This entry publishes an AutoEq filter without a raw measurement CSV. Choose AutoEq to load it; Harman/Flat regeneration needs the measurement data.")
            } else { error }
        })?;
        let data = match target {
            Target::Flat => TargetData::Flat,
            Target::Harman | Target::Reference => {
                TargetData::Reference(catalog.target(entry, target)?)
            }
            Target::AutoEq | Target::Custom => {
                unreachable!("WAV corrections are handled before generation")
            }
        };
        generate(
            entry,
            &catalog.revision,
            &measurement_bytes,
            data,
            rate,
            generation,
        )
    }
}

pub enum TargetData<'a> {
    Reference(&'a ReferenceTarget),
    Flat,
}

pub fn generate(
    entry: &Entry,
    revision: &str,
    measurement_bytes: &[u8],
    data: TargetData<'_>,
    rate: Rate,
    generation: GenerationSettings,
) -> Result<Correction> {
    entry.reference()?;
    let reference = crate::correction::measurement_reference(&entry.path, generation.alignment())?;
    let mut measured = parse_csv(measurement_bytes, "raw")?;
    measured.align(generation.alignment());
    let (target, mut target_curve, target_name) = match data {
        TargetData::Flat => (
            Target::Flat,
            Curve::new(measured.frequency.clone(), vec![0.0; measured.db.len()])?,
            "Literal horizontal response in source measurement reference".to_owned(),
        ),
        TargetData::Reference(target) => (target.kind(), target.curve()?, target.description()),
    };
    let target_hash = ContentHash::of(&serde_json::to_vec(&target_curve)?);
    target_curve.align(generation.alignment());
    let coefficients = generated_impulse(&measured, &target_curve, rate, generation);
    let ir = Correction {
        name: entry.name.clone(),
        rate,
        channels: [coefficients.clone(), coefficients],
        target,
        origin: Origin::AutoEq {
            revision: revision.to_owned(),
            path: entry.path.clone(),
            reference,
            target: target_name,
            measurement_hash: ContentHash::of(measurement_bytes).as_str().into(),
            target_hash: target_hash.as_str().into(),
            policy: generation.policy(),
            generation,
        },
        measured: Some(measured),
        target_curve: Some(target_curve),
    };
    ir.validate()?;
    Ok(ir)
}

pub fn parse_csv(bytes: &[u8], column: &str) -> Result<Curve> {
    let text = std::str::from_utf8(bytes).map_err(|e| invalid(e.to_string()))?;
    let mut lines = text.lines();
    let header: Vec<_> = lines
        .next()
        .ok_or_else(|| invalid("Empty CSV"))?
        .trim_start_matches('\u{feff}')
        .split(',')
        .map(str::trim)
        .collect();
    let f_column = header
        .iter()
        .position(|&s| s == "frequency")
        .ok_or_else(|| invalid("Missing frequency column"))?;
    let value_column = header
        .iter()
        .position(|&s| s == column)
        .ok_or_else(|| invalid(format!("Missing {column} column")))?;
    let mut frequencies = Vec::new();
    let mut values = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let row: Vec<_> = line.split(',').collect();
        let parse = |column: usize| -> Result<f32> {
            row.get(column)
                .ok_or_else(|| invalid("Incomplete CSV row"))?
                .trim()
                .parse()
                .map_err(|_| invalid("Invalid CSV number"))
        };
        frequencies.push(parse(f_column)?);
        values.push(parse(value_column)?);
    }
    Curve::new(frequencies, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_inventory_distinguishes_measurements_from_autoeq_filters() {
        let tree: GitTree =
            serde_json::from_slice(include_bytes!("../tests/fixtures/autoeq-tree.json")).unwrap();
        let inventory = tree.inventory().unwrap();
        let catalog = Catalog::parse(
            include_str!("../tests/fixtures/autoeq-index.md"),
            include_str!("../tests/fixtures/autoeq-recommended.md"),
            UPSTREAM_REVISION,
        )
        .unwrap()
        .with_inventory(&serde_json::to_vec(&inventory).unwrap())
        .unwrap();
        let published = catalog
            .search("DT 990 Pro (250 Ohm) crinacle")
            .next()
            .unwrap()
            .1;
        assert!(published.supports(Target::AutoEq, Rate::Hz44100));
        assert!(published.supports(Target::AutoEq, Rate::Hz48000));
        assert!(!published.supports(Target::Harman, Rate::Hz44100));
        assert!(!published.supports(Target::Flat, Rate::Hz48000));
        let measured = catalog.search("DT 990 Pro oratory").next().unwrap().1;
        assert!(measured.supports(Target::Harman, Rate::Hz44100));
        assert!(measured.supports(Target::Flat, Rate::Hz48000));
        assert_eq!(catalog.filtered("DT 990", CatalogFilter::Both).count(), 1);
        assert_eq!(
            catalog
                .filtered("DT 990", CatalogFilter::Measurements)
                .count(),
            1
        );
        assert_eq!(catalog.filtered("DT 990", CatalogFilter::AutoEq).count(), 2);
        let incomplete = GitTree {
            truncated: true,
            ..tree
        };
        assert!(incomplete.inventory().is_err());
    }

    #[test]
    fn catalog_requirements_cover_asset_combinations_and_unknown_inventory() {
        let mut entry = Entry {
            name: "Example".into(),
            path: "oratory1990/over-ear/Example".into(),
            source: "oratory1990".into(),
            recommended: false,
            assets: None,
            reference: None,
        };
        assert!(CatalogFilter::Any.matches(&entry));
        assert!(!CatalogFilter::AutoEq.matches(&entry));
        for (assets, expected) in [
            (vec![], [true, false, false, false]),
            (
                vec![Asset::AutoEq(Rate::Hz44100)],
                [true, true, false, false],
            ),
            (vec![Asset::Measurements], [true, false, true, false]),
            (
                vec![Asset::AutoEq(Rate::Hz44100), Asset::Measurements],
                [true, true, true, true],
            ),
        ] {
            entry.assets = Some(assets.into_iter().collect());
            assert_eq!(
                CatalogFilter::ALL.map(|filter| filter.matches(&entry)),
                expected
            );
            assert!(!entry.supports(Target::AutoEq, Rate::Hz48000));
        }
    }
}
