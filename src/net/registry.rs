//! The history of every network, as a folder of descriptors.
//!
//! ```text
//! networks/
//!   index.json                  what is below, in one file
//!   image_to_omap/              a scope: what the networks are for
//!     tree/0.1.json 0.2.json    one file per version of a network
//!     unet/0.1.json
//! ```
//!
//! The files are the truth and `index.json` is a view of them, rewritten
//! whenever one is added. A new version is written only when the network
//! changed: [`Registry::register`] compares the architecture's
//! [`NetDescriptor::canonical_hash`] with the latest version's and does
//! nothing if they are the same. A version, once written, is never rewritten;
//! [`Registry::verify`] is what notices that one was, by hand or by accident,
//! by hashing every file again and comparing it with what the index recorded
//! when it was added.
//!
//! The folder is tracked in git next to the code, so a version appears in the
//! same commit as the change that made it. A viewer only ever reads it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::net::describe::{Meta, NetDescriptor, SCHEMA_VERSION};

/// The folder a repository keeps its networks in, relative to its root.
pub const DEFAULT_DIR: &str = "networks";

/// The file which lists every version of every network.
pub const INDEX_FILE: &str = "index.json";

/// One version of a network, as the index lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// `major.minor`.
    pub version: String,
    /// The architecture's [`NetDescriptor::canonical_hash`] when it was added.
    pub hash: String,
    /// When it was added.
    #[serde(default)]
    pub generated_at: String,
    /// The commit of the code it was written from, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
}

/// All the versions of one network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexNetwork {
    /// What it is for.
    pub scope: String,
    /// Its name within the scope.
    pub name: String,
    /// The newest version, which is the last of `versions`.
    pub latest: String,
    /// Every version, oldest first.
    pub versions: Vec<VersionEntry>,
}

/// What `index.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// The version of the descriptor format the files are in.
    pub schema_version: u32,
    /// Every network, sorted by scope and name.
    pub networks: Vec<IndexNetwork>,
}

/// What [`Registry::register`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registered {
    /// The network changed, or was new: this is the version written.
    Created(String),
    /// Nothing changed: this is the version it still is.
    UpToDate(String),
}

/// How a descriptor stands against the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The network has no version yet.
    Missing,
    /// The latest version is this network.
    UpToDate(String),
    /// The latest version is a different network.
    Stale(String),
}

/// A folder of versioned descriptors.
pub struct Registry {
    dir: PathBuf,
}

/// `(major, minor)` out of `"0.3"`.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn io<T>(what: &str, path: &Path, result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("cannot {what} {}: {e}", path.display()))
}

/// Writes `text` to `path` through a neighbour and a rename, so that a reader
/// never sees half of it.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    io("write", &temp, fs::write(&temp, text))?;
    io("rename", &temp, fs::rename(&temp, path))
}

fn subdirs(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in io("read", dir, fs::read_dir(dir))? {
        let entry = io("read", dir, entry)?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

impl Registry {
    /// The registry in `dir`, which need not exist yet.
    pub fn new(dir: impl Into<PathBuf>) -> Registry {
        Registry { dir: dir.into() }
    }

    /// The folder it keeps its files in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, scope: &str, name: &str, version: &str) -> PathBuf {
        self.dir
            .join(scope)
            .join(name)
            .join(format!("{version}.json"))
    }

    /// Reads one version's descriptor.
    pub fn read(&self, scope: &str, name: &str, version: &str) -> Result<NetDescriptor, String> {
        let path = self.file(scope, name, version);
        let text = io("read", &path, fs::read_to_string(&path))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every version of `scope/name` on disk, oldest first, each with the
    /// hash of what is *in the file now*.
    pub fn versions(&self, scope: &str, name: &str) -> Result<Vec<VersionEntry>, String> {
        let folder = self.dir.join(scope).join(name);
        if !folder.is_dir() {
            return Ok(Vec::new());
        }
        let mut found: Vec<((u32, u32), VersionEntry)> = Vec::new();
        for entry in io("read", &folder, fs::read_dir(&folder))? {
            let path = io("read", &folder, entry)?.path();
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            let Some(order) = parse_version(stem) else {
                continue;
            };
            let descriptor = self.read(scope, name, stem)?;
            found.push((
                order,
                VersionEntry {
                    version: stem.to_string(),
                    hash: descriptor.canonical_hash(),
                    generated_at: descriptor.meta.generated_at,
                    git_commit: descriptor.meta.git_commit,
                },
            ));
        }
        found.sort_by_key(|(order, _)| *order);
        Ok(found.into_iter().map(|(_, entry)| entry).collect())
    }

    /// How `descriptor` stands against the latest version of its network.
    pub fn status(&self, descriptor: &NetDescriptor) -> Result<Status, String> {
        let versions = self.versions(&descriptor.scope, &descriptor.name)?;
        Ok(match versions.last() {
            None => Status::Missing,
            Some(latest) if latest.hash == descriptor.canonical_hash() => {
                Status::UpToDate(latest.version.clone())
            }
            Some(latest) => Status::Stale(latest.version.clone()),
        })
    }

    /// Adds `descriptor` as the next version of its network if it differs
    /// from the latest, and rewrites the index; does nothing if it does not.
    ///
    /// The first version of a network is `0.1`. After it the minor number
    /// goes up, or with `bump_major` the major does and the minor is `0`.
    pub fn register(
        &self,
        mut descriptor: NetDescriptor,
        bump_major: bool,
        meta: Meta,
    ) -> Result<Registered, String> {
        let problems = descriptor.problems();
        if !problems.is_empty() {
            return Err(format!(
                "{}/{} is not a sound descriptor:\n  {}",
                descriptor.scope,
                descriptor.name,
                problems.join("\n  ")
            ));
        }
        let versions = self.versions(&descriptor.scope, &descriptor.name)?;
        let next = match versions.last() {
            None => "0.1".to_string(),
            Some(latest) if latest.hash == descriptor.canonical_hash() => {
                return Ok(Registered::UpToDate(latest.version.clone()));
            }
            Some(latest) => {
                let (major, minor) = parse_version(&latest.version)
                    .ok_or_else(|| format!("{:?} is not a version", latest.version))?;
                if bump_major {
                    format!("{}.0", major + 1)
                } else {
                    format!("{major}.{}", minor + 1)
                }
            }
        };
        descriptor.version = next.clone();
        descriptor.meta = meta;
        let path = self.file(&descriptor.scope, &descriptor.name, &next);
        if let Some(parent) = path.parent() {
            io("create", parent, fs::create_dir_all(parent))?;
        }
        let mut text = serde_json::to_string_pretty(&descriptor)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        text.push('\n');
        write_atomic(&path, &text)?;
        self.write_index()?;
        Ok(Registered::Created(next))
    }

    /// The index the files on disk add up to.
    pub fn build_index(&self) -> Result<Index, String> {
        let mut networks = Vec::new();
        if self.dir.is_dir() {
            for scope in subdirs(&self.dir)? {
                for name in subdirs(&self.dir.join(&scope))? {
                    let versions = self.versions(&scope, &name)?;
                    let Some(last) = versions.last() else {
                        continue;
                    };
                    networks.push(IndexNetwork {
                        scope: scope.clone(),
                        name,
                        latest: last.version.clone(),
                        versions,
                    });
                }
            }
        }
        Ok(Index {
            schema_version: SCHEMA_VERSION,
            networks,
        })
    }

    /// Rewrites `index.json` from the files.
    pub fn write_index(&self) -> Result<(), String> {
        let index = self.build_index()?;
        let mut text = serde_json::to_string_pretty(&index)
            .map_err(|e| format!("cannot write the index: {e}"))?;
        text.push('\n');
        io("create", &self.dir, fs::create_dir_all(&self.dir))?;
        write_atomic(&self.dir.join(INDEX_FILE), &text)
    }

    /// What is wrong with the history, one line each: a file which is not the
    /// version its name says or which is not a sound descriptor, and any way
    /// the files and `index.json` disagree — which is what an edit to a
    /// version already written looks like.
    pub fn verify(&self) -> Result<Vec<String>, String> {
        let mut problems = Vec::new();
        let built = self.build_index()?;
        for network in &built.networks {
            for entry in &network.versions {
                let descriptor = self.read(&network.scope, &network.name, &entry.version)?;
                let at = format!("{}/{}/{}", network.scope, network.name, entry.version);
                if descriptor.version != entry.version {
                    problems.push(format!(
                        "{at}: the file says it is version {:?}",
                        descriptor.version
                    ));
                }
                if descriptor.scope != network.scope || descriptor.name != network.name {
                    problems.push(format!(
                        "{at}: the file says it is {}/{}",
                        descriptor.scope, descriptor.name
                    ));
                }
                for problem in descriptor.problems() {
                    problems.push(format!("{at}: {problem}"));
                }
            }
        }
        let path = self.dir.join(INDEX_FILE);
        match fs::read_to_string(&path) {
            Err(_) if built.networks.is_empty() => {}
            Err(_) => problems.push(format!("{} is missing", path.display())),
            Ok(text) => match serde_json::from_str::<Index>(&text) {
                Err(e) => problems.push(format!("{}: {e}", path.display())),
                Ok(recorded) => {
                    for network in &built.networks {
                        let there = recorded
                            .networks
                            .iter()
                            .find(|n| n.scope == network.scope && n.name == network.name);
                        for entry in &network.versions {
                            let said = there.and_then(|n| {
                                n.versions.iter().find(|v| v.version == entry.version)
                            });
                            match said {
                                None => problems.push(format!(
                                    "{}/{}/{} is not in the index",
                                    network.scope, network.name, entry.version
                                )),
                                Some(said) if said.hash != entry.hash => problems.push(format!(
                                    "{}/{}/{} was changed after it was registered",
                                    network.scope, network.name, entry.version
                                )),
                                Some(_) => {}
                            }
                        }
                    }
                    for network in &recorded.networks {
                        for entry in &network.versions {
                            let still_there = built.networks.iter().any(|n| {
                                n.scope == network.scope
                                    && n.name == network.name
                                    && n.versions.iter().any(|v| v.version == entry.version)
                            });
                            if !still_there {
                                problems.push(format!(
                                    "{}/{}/{} is in the index and its file is gone",
                                    network.scope, network.name, entry.version
                                ));
                            }
                        }
                    }
                }
            },
        }
        Ok(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::describe::{layer, Node};
    use std::collections::BTreeMap;

    fn net(width: usize) -> NetDescriptor {
        let root = Node::new("net", "model", "Net")
            .child(layer::conv2d("conv", 3, width, 3, false))
            .edge("@in", "conv", &["B", "3", "H", "W"])
            .edge("conv", "@out", &["B", &width.to_string(), "H", "W"]);
        NetDescriptor::new("image_to_omap", "tiny", BTreeMap::new(), root)
    }

    fn meta() -> Meta {
        Meta {
            generated_at: "2026-11-18T00:00:00Z".to_string(),
            git_commit: Some("abc1234".to_string()),
        }
    }

    #[test]
    fn the_first_version_is_0_1_and_the_same_network_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        assert_eq!(registry.status(&net(8)).unwrap(), Status::Missing);

        let first = registry.register(net(8), false, meta()).unwrap();
        assert_eq!(first, Registered::Created("0.1".into()));
        let again = registry.register(net(8), false, meta()).unwrap();
        assert_eq!(again, Registered::UpToDate("0.1".into()));

        assert_eq!(
            registry.status(&net(8)).unwrap(),
            Status::UpToDate("0.1".into())
        );
        assert_eq!(registry.versions("image_to_omap", "tiny").unwrap().len(), 1);
    }

    #[test]
    fn a_changed_network_is_the_next_version_and_the_old_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry.register(net(8), false, meta()).unwrap();

        assert_eq!(
            registry.status(&net(16)).unwrap(),
            Status::Stale("0.1".into())
        );
        let second = registry.register(net(16), false, meta()).unwrap();
        assert_eq!(second, Registered::Created("0.2".into()));
        let major = registry.register(net(32), true, meta()).unwrap();
        assert_eq!(major, Registered::Created("1.0".into()));

        let versions: Vec<String> = registry
            .versions("image_to_omap", "tiny")
            .unwrap()
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(versions, ["0.1", "0.2", "1.0"]);
        // The first is still the first network.
        let old = registry.read("image_to_omap", "tiny", "0.1").unwrap();
        assert_eq!(old.canonical_hash(), net(8).canonical_hash());
        assert_eq!(old.version, "0.1");
        assert!(registry.verify().unwrap().is_empty());
    }

    #[test]
    fn versions_sort_as_numbers_not_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        for width in 1..=11 {
            registry.register(net(width), false, meta()).unwrap();
        }
        let versions = registry.versions("image_to_omap", "tiny").unwrap();
        assert_eq!(versions.last().unwrap().version, "0.11");
        assert_eq!(versions[1].version, "0.2");
    }

    #[test]
    fn the_index_lists_every_network_and_its_latest() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry.register(net(8), false, meta()).unwrap();
        registry.register(net(16), false, meta()).unwrap();
        let mut other = net(8);
        other.name = "other".to_string();
        registry.register(other, false, meta()).unwrap();

        let text = fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        let index: Index = serde_json::from_str(&text).unwrap();
        let names: Vec<(&str, &str)> = index
            .networks
            .iter()
            .map(|n| (n.name.as_str(), n.latest.as_str()))
            .collect();
        assert_eq!(names, [("other", "0.1"), ("tiny", "0.2")]);
    }

    #[test]
    fn an_edited_version_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry.register(net(8), false, meta()).unwrap();
        assert!(registry.verify().unwrap().is_empty());

        let path = dir.path().join("image_to_omap/tiny/0.1.json");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("\"out_channels\": 8", "\"out_channels\": 9"),
        )
        .unwrap();

        let problems = registry.verify().unwrap();
        assert!(
            problems.iter().any(|p| p.contains("changed after")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_file_added_by_hand_and_a_missing_index_are_caught() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry.register(net(8), false, meta()).unwrap();

        fs::remove_file(dir.path().join(INDEX_FILE)).unwrap();
        assert!(registry
            .verify()
            .unwrap()
            .iter()
            .any(|p| p.contains("missing")));

        registry.write_index().unwrap();
        fs::copy(
            dir.path().join("image_to_omap/tiny/0.1.json"),
            dir.path().join("image_to_omap/tiny/0.2.json"),
        )
        .unwrap();
        let problems = registry.verify().unwrap();
        assert!(
            problems.iter().any(|p| p.contains("not in the index")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("says it is version")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_unsound_descriptor_is_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        let mut bad = net(8);
        bad.root = bad.root.edge("conv", "ghost", &["B"]);
        assert!(registry.register(bad, false, meta()).is_err());
        assert!(registry
            .versions("image_to_omap", "tiny")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_empty_registry_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path().join("nothing_yet"));
        assert!(registry.verify().unwrap().is_empty());
        assert!(registry.build_index().unwrap().networks.is_empty());
    }
}
