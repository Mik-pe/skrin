//! Test-only filesystem persistence projection beneath the production protocol.
//!
//! Successful file sync persists contents; successful directory sync persists
//! names/inode identities. Bounded workloads explore all subsets of current
//! unsynced namespace differences, observed write/truncate contents, and every
//! append prefix after synchronized bytes. Acknowledgment/publication markers
//! are recorded only after successful production operations.
//! This is not a model of torn sectors, hardware caches or all filesystem rules.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

type Identity = (u64, u64);
#[derive(Clone, PartialEq, Eq)]
struct Entry {
    id: Identity,
    directory: bool,
}
#[derive(Clone)]
enum Node {
    File(Arc<[u8]>),
    Directory(BTreeMap<OsString, Entry>),
}
#[derive(Clone)]
struct Tree {
    root: Identity,
    nodes: BTreeMap<Identity, Node>,
}

fn identity(metadata: &fs::Metadata) -> Identity {
    (metadata.dev(), metadata.ino())
}
fn names(path: &Path) -> BTreeMap<OsString, Entry> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            assert!(
                meta.is_file() || meta.is_dir(),
                "model workloads exclude links/special files"
            );
            (
                entry.file_name(),
                Entry {
                    id: identity(&meta),
                    directory: meta.is_dir(),
                },
            )
        })
        .collect()
}
impl Tree {
    fn capture(root: &Path) -> Self {
        fn walk(path: &Path, nodes: &mut BTreeMap<Identity, Node>) {
            let meta = fs::symlink_metadata(path).unwrap();
            let node = if meta.is_dir() {
                let children = names(path);
                for name in children.keys() {
                    walk(&path.join(name), nodes);
                }
                Node::Directory(children)
            } else {
                Node::File(fs::read(path).unwrap().into())
            };
            nodes.insert(identity(&meta), node);
        }
        let mut nodes = BTreeMap::new();
        walk(root, &mut nodes);
        Self {
            root: identity(&fs::metadata(root).unwrap()),
            nodes,
        }
    }

    fn restore(&self, root: &Path) {
        fn contents(tree: &Tree, directory: Identity, path: &Path) {
            let Some(Node::Directory(entries)) = tree.nodes.get(&directory) else {
                return;
            };
            for (name, entry) in entries {
                let child = path.join(name);
                if entry.directory {
                    fs::create_dir(&child).unwrap();
                    contents(tree, entry.id, &child);
                } else {
                    let bytes = match tree.nodes.get(&entry.id) {
                        Some(Node::File(bytes)) => bytes.as_ref(),
                        _ => &[],
                    };
                    fs::write(&child, bytes).unwrap();
                }
            }
        }
        // The caller owns this unique test root and has dropped every DB handle.
        fs::remove_dir_all(root).unwrap();
        fs::create_dir(root).unwrap();
        contents(self, self.root, root);
    }
}

/// Deliberately discard one guarantee without changing production operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Omission {
    None,
    SnapshotSync,
    NewWalSync,
    AppendWalSync,
    OwnerSync,
    ManifestSync,
    GenerationSync,
    PrepublicationParentSync,
    PublicationParentSync,
}

pub(crate) struct Image {
    tree: Tree,
    pub(crate) early_manifest: bool,
    pub(crate) unsynced_file: bool,
    pub(crate) partial_append: bool,
    pub(crate) acknowledged_sequence: Option<u64>,
    pub(crate) published_generation: Option<u64>,
}
impl Image {
    pub(crate) fn restore(&self, root: &Path) {
        self.tree.restore(root);
    }
}
struct Model {
    root: PathBuf,
    durable: Tree,
    images: Vec<Image>,
    pending_manifest: Option<(Identity, Entry, OsString)>,
    omission: Omission,
    root_syncs: usize,
    full: bool,
    volatile: BTreeMap<Identity, Arc<[u8]>>,
    acknowledged_sequence: Option<u64>,
    published_generation: Option<u64>,
}
thread_local! { static MODEL: RefCell<Option<Model>> = const { RefCell::new(None) }; }

pub(crate) fn start(root: &Path, omit_first_root_sync: bool) {
    begin(
        root,
        if omit_first_root_sync {
            Omission::PrepublicationParentSync
        } else {
            Omission::None
        },
        false,
        None,
    );
}
pub(crate) fn start_full(root: &Path, omission: Omission, sequence: u64) {
    begin(root, omission, true, Some(sequence));
}
fn begin(root: &Path, omission: Omission, full: bool, sequence: Option<u64>) {
    let root = fs::canonicalize(root).unwrap();
    MODEL.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Model {
            durable: Tree::capture(&root),
            root,
            images: Vec::new(),
            pending_manifest: None,
            omission,
            root_syncs: 0,
            full,
            volatile: BTreeMap::new(),
            acknowledged_sequence: sequence,
            published_generation: None,
        });
    });
    boundary();
}
pub(crate) fn observe_writes() -> bool {
    MODEL.with(|slot| slot.borrow().as_ref().is_some_and(|model| model.full))
}
/// Call only after the production write returns success; earlier images allow
/// old or new state. No parser or alternative transaction engine lives here.
pub(crate) fn acknowledged(sequence: u64) {
    MODEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let model = slot.as_mut().expect("started projection");
        assert!(
            model
                .acknowledged_sequence
                .is_none_or(|old| sequence >= old)
        );
        model.acknowledged_sequence = Some(sequence);
    });
    boundary();
}
pub(crate) fn published(generation: u64) {
    MODEL.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("started projection")
            .published_generation = Some(generation)
    });
    boundary();
}
fn contents(file: &File) -> (Identity, Arc<[u8]>) {
    let meta = file.metadata().unwrap();
    assert!(
        meta.len() <= 1024 * 1024,
        "keep persistence projection workloads small"
    );
    let mut bytes = vec![0; meta.len() as usize];
    file.read_exact_at(&mut bytes, 0).unwrap();
    (identity(&meta), bytes.into())
}
/// Observe actual bytes after a successful production write/truncate, before
/// its sync. Prefix alternatives retain all previously synchronized bytes.
pub(crate) fn file_written(file: &File) {
    if !observe_writes() {
        return;
    }
    let (id, bytes) = contents(file);
    MODEL.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .unwrap()
            .volatile
            .insert(id, bytes)
    });
    boundary();
}
pub(crate) fn file_synced(file: &File) {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            let (id, bytes) = contents(file);
            let live = Tree::capture(&model.root);
            let name = live.nodes.values().find_map(|node| match node {
                Node::Directory(entries) => entries
                    .iter()
                    .find(|(_, entry)| entry.id == id)
                    .map(|(name, _)| name.to_string_lossy()),
                _ => None,
            });
            let skip = match (model.omission, name.as_deref()) {
                (Omission::SnapshotSync, Some("snapshot"))
                | (Omission::OwnerSync, Some("OWNER")) => true,
                (Omission::NewWalSync, Some("wal")) => !model.durable.nodes.contains_key(&id),
                (Omission::AppendWalSync, Some("wal")) => model.durable.nodes.contains_key(&id),
                (Omission::ManifestSync, Some(name)) => name.starts_with("CURRENT-"),
                _ => false,
            };
            if !skip {
                model.durable.nodes.insert(id, Node::File(bytes));
                model.volatile.remove(&id);
            }
        }
    });
}
pub(crate) fn directory_synced(path: &Path) {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            if !path.starts_with(&model.root) {
                return;
            }
            if path == model.root {
                model.root_syncs += 1;
                if (model.omission == Omission::PrepublicationParentSync && model.root_syncs == 1)
                    || (model.omission == Omission::PublicationParentSync && model.root_syncs == 2)
                {
                    return;
                }
                model.pending_manifest = None;
            } else if model.omission == Omission::GenerationSync {
                return;
            }
            model.durable.nodes.insert(
                identity(&fs::metadata(path).unwrap()),
                Node::Directory(names(path)),
            );
        }
    });
}
pub(crate) fn manifest_renamed(root: &Path, temporary: &Path) {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            let meta = fs::metadata(root.join("CURRENT")).unwrap();
            model.pending_manifest = Some((
                identity(&fs::metadata(root).unwrap()),
                Entry {
                    id: identity(&meta),
                    directory: false,
                },
                temporary.file_name().unwrap().to_owned(),
            ));
        }
    });
}

// Small, bounded workloads exhaust all subsets of the currently unsynchronized
// namespace differences. File survival crosses those subsets: all lost, all
// current bytes surviving, and every observed append prefix independently for
// one file. This is not every possible sector/reordered-write combination.
impl Model {
    fn namespace_images(&self) -> Vec<Tree> {
        if !self.full {
            let mut trees = vec![self.durable.clone()];
            if let Some((parent, entry, temporary)) = &self.pending_manifest {
                let mut tree = self.durable.clone();
                let Some(Node::Directory(names)) = tree.nodes.get_mut(parent) else {
                    panic!("parent identity missing");
                };
                names.insert("CURRENT".into(), entry.clone());
                names.remove(temporary);
                trees.push(tree);
            }
            return trees;
        }
        let live = Tree::capture(&self.root);
        let mut deltas = Vec::new();
        for (&parent, node) in &live.nodes {
            let Node::Directory(current) = node else {
                continue;
            };
            let old = match self.durable.nodes.get(&parent) {
                Some(Node::Directory(old)) => old.clone(),
                _ => BTreeMap::new(),
            };
            let keys: std::collections::BTreeSet<_> = old.keys().chain(current.keys()).collect();
            for name in keys {
                if current.get(name) != old.get(name) {
                    deltas.push((parent, name.clone(), current.get(name).cloned()));
                }
            }
        }
        assert!(
            deltas.len() <= 12,
            "bound namespace subset enumeration in projection workloads"
        );
        let mut trees = Vec::new();
        for mask in 0..(1usize << deltas.len()) {
            let mut tree = self.durable.clone();
            for (bit, (parent, name, entry)) in deltas.iter().enumerate() {
                if mask & (1 << bit) == 0 {
                    continue;
                }
                let Node::Directory(names) = tree
                    .nodes
                    .entry(*parent)
                    .or_insert_with(|| Node::Directory(BTreeMap::new()))
                else {
                    panic!("directory identity changed node type");
                };
                match entry {
                    Some(entry) => {
                        names.insert(name.clone(), entry.clone());
                    }
                    None => {
                        names.remove(name);
                    }
                }
            }
            trees.push(tree);
        }
        trees
    }
    fn image(&self, tree: Tree, unsynced_file: bool, partial_append: bool) -> Image {
        let early_manifest = self.pending_manifest.as_ref().is_some_and(|(parent, entry, _)| {
            matches!(tree.nodes.get(parent), Some(Node::Directory(names)) if names.get(std::ffi::OsStr::new("CURRENT")) == Some(entry))
        });
        Image {
            tree,
            early_manifest,
            unsynced_file,
            partial_append,
            acknowledged_sequence: self.acknowledged_sequence,
            published_generation: self.published_generation,
        }
    }
    fn capture_images(&mut self) {
        for tree in self.namespace_images() {
            self.images.push(self.image(tree.clone(), false, false));
            if self.volatile.is_empty() {
                continue;
            }
            let mut all = tree.clone();
            for (&id, bytes) in &self.volatile {
                all.nodes.insert(id, Node::File(bytes.clone()));
            }
            self.images.push(self.image(all, true, false));
            for (&id, bytes) in &self.volatile {
                let old: &[u8] = match self.durable.nodes.get(&id) {
                    Some(Node::File(old)) => old,
                    _ => &[],
                };
                if matches!(self.durable.nodes.get(&id), Some(Node::File(_)))
                    && bytes.starts_with(old)
                {
                    assert!(
                        bytes.len() - old.len() <= 4096,
                        "bound exhaustive append prefixes in projection workloads"
                    );
                    for end in old.len() + 1..bytes.len() {
                        let mut prefix = tree.clone();
                        prefix
                            .nodes
                            .insert(id, Node::File(Arc::from(&bytes[..end])));
                        self.images.push(self.image(prefix, true, true));
                    }
                }
                let mut single = tree.clone();
                single.nodes.insert(id, Node::File(bytes.clone()));
                self.images.push(self.image(single, true, false));
            }
        }
    }
}
pub(crate) fn boundary() {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            model.capture_images();
        }
    });
}
pub(crate) fn finish() -> Vec<Image> {
    boundary();
    MODEL.with(|slot| slot.borrow_mut().take().expect("started projection").images)
}
