//! Test-only filesystem persistence projection beneath the production protocol.
//!
//! Successful file sync persists contents; successful directory sync persists
//! names/inode identities. At every production boundary we can discard unsynced
//! namespace changes. We additionally let an unsynced CURRENT rename survive
//! independently, exposing a missing pre-publication parent-directory sync.
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
#[derive(Clone)]
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

pub(crate) struct Image {
    tree: Tree,
    pub(crate) early_manifest: bool,
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
    omit_first_root_sync: bool,
    root_syncs: usize,
}
thread_local! { static MODEL: RefCell<Option<Model>> = const { RefCell::new(None) }; }

pub(crate) fn start(root: &Path, omit_first_root_sync: bool) {
    let root = fs::canonicalize(root).unwrap();
    MODEL.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Model {
            durable: Tree::capture(&root),
            root,
            images: Vec::new(),
            pending_manifest: None,
            omit_first_root_sync,
            root_syncs: 0,
        });
    });
    boundary();
}

pub(crate) fn file_synced(file: &File) {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            let meta = file.metadata().unwrap();
            assert!(
                meta.len() <= 1024 * 1024,
                "keep persistence projection workloads small"
            );
            let mut bytes = vec![0; meta.len() as usize];
            file.read_exact_at(&mut bytes, 0).unwrap();
            model
                .durable
                .nodes
                .insert(identity(&meta), Node::File(bytes.into()));
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
                // Negative control: the real sync still happens, but projection
                // deliberately ignores its guarantee to prove the test is useful.
                if model.omit_first_root_sync && model.root_syncs == 1 {
                    return;
                }
                model.pending_manifest = None;
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

pub(crate) fn boundary() {
    MODEL.with(|slot| {
        if let Some(model) = slot.borrow_mut().as_mut() {
            model.images.push(Image {
                tree: model.durable.clone(),
                early_manifest: false,
            });
            if let Some((parent, entry, temporary)) = &model.pending_manifest {
                let mut tree = model.durable.clone();
                let Some(Node::Directory(names)) = tree.nodes.get_mut(parent) else {
                    panic!("parent identity missing");
                };
                names.insert("CURRENT".into(), entry.clone());
                names.remove(temporary);
                model.images.push(Image {
                    tree,
                    early_manifest: true,
                });
            }
        }
    });
}

pub(crate) fn finish() -> Vec<Image> {
    boundary();
    MODEL.with(|slot| slot.borrow_mut().take().expect("started projection").images)
}
