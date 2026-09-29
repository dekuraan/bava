// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where scenes come from, and how their files reach Bevy's asset server.
//!
//! Two kinds of scene exist: **built-in** ones compiled into the binary (every
//! directory under `assets/scenes/`, see `build.rs`) and **user** scenes —
//! any directory with a `scene.toml`, found under `~/.config/bava/scenes/` or
//! named by path on the command line.
//!
//! Both are served the same way: the scene's files are inserted into Bevy's
//! [`EmbeddedAssetRegistry`] under `bava/scene/<slot>/…` and loaded through the
//! `embedded://` source. That needs no asset source registered before
//! `AssetPlugin` is built (so any directory can be opened at runtime), relative
//! references inside a glTF resolve naturally, and hot reload is just
//! "re-insert the bytes, ask the asset server to reload".

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::asset::io::embedded::EmbeddedAssetRegistry;

include!(concat!(env!("OUT_DIR"), "/builtin_scenes.rs"));

/// The scene description file inside a scene directory.
pub const SCENE_FILE: &str = "scene.toml";
/// Refuse to read a user scene larger than this in total.
#[cfg(not(target_arch = "wasm32"))]
const MAX_SCENE_BYTES: u64 = 256 * 1024 * 1024;

/// Where a scene's files live.
#[derive(Clone, Debug, PartialEq, Eq)]
// The web build has no filesystem, so it only ever holds built-ins.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub enum SceneSource {
    /// Compiled in; the id is the `assets/scenes/<id>` directory name.
    Builtin(&'static str),
    /// A directory on disk.
    Dir(PathBuf),
}

/// A scene that can be selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneEntry {
    /// Stable identifier: the directory name.
    pub id: String,
    pub source: SceneSource,
}

impl SceneEntry {
    /// A short label for menus: the id, marked when it is a user scene.
    pub fn label(&self) -> String {
        match self.source {
            SceneSource::Builtin(_) => self.id.clone(),
            SceneSource::Dir(_) => format!("{} (user)", self.id),
        }
    }
}

/// Ids of the built-in scenes, sorted.
pub fn builtin_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = BUILTIN_SCENE_FILES.iter().map(|(id, _, _)| *id).collect();
    ids.dedup();
    ids
}

/// `~/.config/bava/scenes/`, where user scenes are discovered.
pub fn user_scenes_dir() -> Option<PathBuf> {
    crate::config::Config::default_path().and_then(|p| p.parent().map(|d| d.join("scenes")))
}

/// Every selectable scene: built-ins first, then user scene directories (a
/// user scene with a built-in's id shadows it, so a built-in can be copied out
/// and customized in place).
pub fn discover() -> Vec<SceneEntry> {
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut out: Vec<SceneEntry> = builtin_ids()
        .into_iter()
        .map(|id| SceneEntry {
            id: id.to_string(),
            source: SceneSource::Builtin(id),
        })
        .collect();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dir) = user_scenes_dir()
        && let Ok(read) = std::fs::read_dir(&dir)
    {
        let mut user: Vec<SceneEntry> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(SCENE_FILE).is_file())
            .filter_map(|p| {
                let id = p.file_name()?.to_string_lossy().into_owned();
                Some(SceneEntry {
                    id,
                    source: SceneSource::Dir(p),
                })
            })
            .collect();
        user.sort_by(|a, b| a.id.cmp(&b.id));
        for entry in user {
            out.retain(|e| e.id != entry.id);
            out.push(entry);
        }
    }
    out
}

/// Resolve a `--scene` / `[scene] name` argument: a known scene id, or a path
/// to a scene directory or its `scene.toml`.
pub fn resolve(arg: &str) -> Option<SceneEntry> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if let Some(entry) = discover().into_iter().find(|e| e.id == arg) {
        return Some(entry);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = scene_dir_of(Path::new(arg));
        if dir.join(SCENE_FILE).is_file() {
            let dir = dir.canonicalize().unwrap_or(dir);
            let id = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scene".into());
            return Some(SceneEntry {
                id,
                source: SceneSource::Dir(dir),
            });
        }
    }
    None
}

/// The scene directory a path argument names: the path itself, or the parent
/// of a `scene.toml`. A bare `scene.toml` has an *empty* parent, which can't be
/// read or canonicalized, so it means the current directory.
#[cfg(not(target_arch = "wasm32"))]
fn scene_dir_of(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|n| n == SCENE_FILE) {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    } else {
        path.to_path_buf()
    }
}

/// A scene's files, keyed by their `/`-separated path inside the scene.
#[derive(Clone, Debug, Default)]
pub struct SceneFiles {
    pub files: BTreeMap<String, Cow<'static, [u8]>>,
}

impl SceneFiles {
    /// The `scene.toml` text.
    pub fn scene_toml(&self) -> Result<&str, String> {
        let bytes = self
            .files
            .get(SCENE_FILE)
            .ok_or_else(|| format!("no {SCENE_FILE}"))?;
        std::str::from_utf8(bytes).map_err(|e| format!("{SCENE_FILE} is not UTF-8: {e}"))
    }

    /// A file's text (a shader), by a path as written in `scene.toml`.
    pub fn text(&self, rel: &str) -> Result<&str, String> {
        let key = normalize(rel)?;
        let bytes = self
            .files
            .get(&key)
            .ok_or_else(|| format!("missing file {rel:?}"))?;
        std::str::from_utf8(bytes).map_err(|e| format!("{rel} is not UTF-8: {e}"))
    }

    /// Whether a referenced file exists.
    pub fn contains(&self, rel: &str) -> bool {
        normalize(rel).is_ok_and(|k| self.files.contains_key(&k))
    }
}

/// Read every file of a scene.
pub fn read(source: &SceneSource) -> Result<SceneFiles, String> {
    match source {
        SceneSource::Builtin(id) => {
            let files: BTreeMap<String, Cow<'static, [u8]>> = BUILTIN_SCENE_FILES
                .iter()
                .filter(|(sid, _, _)| sid == id)
                .map(|(_, rel, bytes)| (rel.to_string(), Cow::Borrowed(*bytes)))
                .collect();
            if files.is_empty() {
                return Err(format!("no built-in scene {id:?}"));
            }
            Ok(SceneFiles { files })
        }
        SceneSource::Dir(dir) => read_dir(dir),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_dir(dir: &Path) -> Result<SceneFiles, String> {
    let mut paths = Vec::new();
    walk_scene(dir, &mut paths).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut total = 0u64;
    let mut files = BTreeMap::new();
    for path in paths {
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        total += meta.len();
        if total > MAX_SCENE_BYTES {
            return Err(format!(
                "{} is over the {} MB scene size limit",
                dir.display(),
                MAX_SCENE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(rel, Cow::Owned(bytes));
    }
    if !files.contains_key(SCENE_FILE) {
        return Err(format!("{} has no {SCENE_FILE}", dir.display()));
    }
    Ok(SceneFiles { files })
}

#[cfg(target_arch = "wasm32")]
fn read_dir(dir: &Path) -> Result<SceneFiles, String> {
    Err(format!(
        "{}: scene directories need a filesystem",
        dir.display()
    ))
}

/// Deepest directory nesting [`walk_scene`] descends into.
#[cfg(not(target_arch = "wasm32"))]
const MAX_SCENE_DEPTH: usize = 32;

/// Most files [`walk_scene`] lists. With [`MAX_SCENE_BYTES`] it bounds the walk
/// itself, which the hot-reload stamp repeats every second.
#[cfg(not(target_arch = "wasm32"))]
const MAX_SCENE_FILES: usize = 10_000;

/// Every file of a scene directory, recursively. Symlinks are followed, to
/// agree with [`discover`] / [`resolve`] (which accept a linked `scene.toml`)
/// and so scenes installed by a dotfile manager load. Dangling links are
/// skipped; a linked directory is expanded once however many links reach it,
/// and never when it is one of its own ancestors. Stops with an error past
/// [`MAX_SCENE_FILES`] files or [`MAX_SCENE_BYTES`] bytes, so a link into a big
/// or link-dense tree fails fast instead of walking it.
#[cfg(not(target_arch = "wasm32"))]
fn walk_scene(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let root = dir.canonicalize()?;
    let mut walk = Walk {
        out,
        ancestors: vec![root],
        expanded: std::collections::HashSet::new(),
        bytes: 0,
    };
    walk.dir(dir)
}

/// [`walk_scene`]'s state.
#[cfg(not(target_arch = "wasm32"))]
struct Walk<'a> {
    out: &'a mut Vec<PathBuf>,
    /// Canonical paths of the directory being walked and every one above it,
    /// so a link cycle is caught on its way back.
    ancestors: Vec<PathBuf>,
    /// Canonical targets of linked directories already walked.
    expanded: std::collections::HashSet<PathBuf>,
    bytes: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Walk<'_> {
    fn dir(&mut self, dir: &Path) -> std::io::Result<()> {
        if self.ancestors.len() > MAX_SCENE_DEPTH {
            return Err(std::io::Error::other(format!(
                "{} is nested more than {MAX_SCENE_DEPTH} directories deep",
                dir.display()
            )));
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name.ends_with('~') {
                continue;
            }
            let path = entry.path();
            let mut kind = entry.file_type()?;
            let linked = kind.is_symlink();
            if linked {
                match std::fs::metadata(&path) {
                    Ok(target) => kind = target.file_type(),
                    Err(_) => continue,
                }
            }
            if kind.is_dir() {
                let canonical = if linked {
                    path.canonicalize()?
                } else {
                    // A real subdirectory of a canonical path is canonical too.
                    self.ancestors[self.ancestors.len() - 1].join(entry.file_name())
                };
                if self.ancestors.contains(&canonical)
                    || (linked && !self.expanded.insert(canonical.clone()))
                {
                    continue;
                }
                self.ancestors.push(canonical);
                let walked = self.dir(&path);
                self.ancestors.pop();
                walked?;
            } else if kind.is_file() {
                self.bytes += std::fs::metadata(&path).map_or(0, |m| m.len());
                self.out.push(path);
                if self.out.len() > MAX_SCENE_FILES || self.bytes > MAX_SCENE_BYTES {
                    return Err(std::io::Error::other(format!(
                        "more than {MAX_SCENE_FILES} files or {} MB",
                        MAX_SCENE_BYTES / (1024 * 1024)
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A cheap change stamp for a user scene directory (sizes + mtimes of every
/// file), polled for hot reload. `None` for built-ins, which cannot change.
pub fn stamp(source: &SceneSource) -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    if let SceneSource::Dir(dir) = source {
        use std::hash::{Hash, Hasher};
        let mut paths = Vec::new();
        walk_scene(dir, &mut paths).ok()?;
        paths.sort();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in paths {
            p.hash(&mut h);
            if let Ok(meta) = std::fs::metadata(&p) {
                meta.len().hash(&mut h);
                if let Ok(t) = meta.modified() {
                    t.hash(&mut h);
                }
            }
        }
        return Some(h.finish());
    }
    let _ = source;
    None
}

/// Normalize a path written in `scene.toml` to a key of [`SceneFiles`]:
/// `/`-separated, no leading `./`, and never climbing out of the scene.
///
/// `#` is refused: the asset server reads everything after the last `#` of an
/// asset path as a sub-asset label, so `sounds/C#4.ogg` would load a file
/// named `sounds/C`, and a trailing `#` doesn't parse at all.
pub fn normalize(rel: &str) -> Result<String, String> {
    if rel.contains('#') {
        return Err(format!(
            "{rel:?}: '#' can't be used in scene file names (Bevy reads it as an asset label)"
        ));
    }
    let unified = rel.trim().replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(format!("{rel:?} leaves the scene directory")),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err("empty path".into());
    }
    Ok(parts.join("/").to_string())
}

/// The embedded-registry directory a loaded scene's files live under.
fn slot_dir(slot: &str) -> String {
    format!("bava/scene/{slot}")
}

/// Asset path for a scene file, loadable with `AssetServer::load`.
pub fn asset_path(slot: &str, rel: &str) -> Result<String, String> {
    Ok(format!("embedded://{}/{}", slot_dir(slot), normalize(rel)?))
}

/// Serve a scene's files through the embedded asset source. Returns the
/// registry paths, for [`EmbeddedAssetRegistry::remove_asset`] on unload.
pub fn register(registry: &EmbeddedAssetRegistry, slot: &str, files: &SceneFiles) -> Vec<PathBuf> {
    let dir = slot_dir(slot);
    let mut paths = Vec::with_capacity(files.files.len());
    for (rel, bytes) in &files.files {
        let asset_path = PathBuf::from(format!("{dir}/{rel}"));
        match bytes {
            Cow::Borrowed(b) => registry.insert_asset(asset_path.clone(), &asset_path, *b),
            Cow::Owned(b) => registry.insert_asset(asset_path.clone(), &asset_path, b.clone()),
        }
        paths.push(asset_path);
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_scenes_are_embedded_with_a_scene_file() {
        let ids = builtin_ids();
        assert!(ids.contains(&"minecraft"), "{ids:?}");
        assert!(ids.contains(&"solar_system"), "{ids:?}");
        for id in ids {
            let files = read(&SceneSource::Builtin(id)).unwrap();
            files.scene_toml().unwrap_or_else(|e| panic!("{id}: {e}"));
        }
    }

    #[test]
    fn every_builtin_scene_parses_and_references_real_files() {
        for id in builtin_ids() {
            let files = read(&SceneSource::Builtin(id)).unwrap();
            let def = crate::scene::def::SceneDef::parse(files.scene_toml().unwrap())
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            for path in crate::scene::referenced_files(&def) {
                assert!(
                    files.contains(&path),
                    "{id}: {path} is referenced but missing"
                );
            }
            for (name, path) in &def.shaders {
                files
                    .text(path)
                    .unwrap_or_else(|e| panic!("{id}: shader {name}: {e}"));
            }
        }
    }

    #[test]
    fn normalize_keeps_paths_inside_the_scene() {
        assert_eq!(normalize("./textures/a.png").unwrap(), "textures/a.png");
        assert_eq!(normalize("textures//a.png").unwrap(), "textures/a.png");
        assert_eq!(normalize("textures\\a.png").unwrap(), "textures/a.png");
        assert!(normalize("../secret").is_err());
        assert!(normalize("a/../../b").is_err());
        assert!(normalize("").is_err());
    }

    #[test]
    fn hash_in_a_file_name_is_refused_not_read_as_a_label() {
        let err = normalize("sounds/piano_C#4.ogg").unwrap_err();
        assert!(err.contains('#'), "{err}");
        // A trailing '#' would make `AssetPath::parse` fail (and panic in load).
        assert!(normalize("textures/a.png#").is_err());
        assert!(asset_path("s", "sounds/C#4.ogg").is_err());

        let mut files = SceneFiles::default();
        files
            .files
            .insert("sounds/C#4.ogg".into(), Cow::Borrowed(&[0u8][..]));
        assert!(!files.contains("sounds/C#4.ogg"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_bare_scene_file_names_the_current_directory() {
        assert_eq!(scene_dir_of(Path::new(SCENE_FILE)), Path::new("."));
        assert_eq!(scene_dir_of(Path::new("a/scene.toml")), Path::new("a"));
        assert_eq!(scene_dir_of(Path::new("./scene.toml")), Path::new("."));
        assert_eq!(scene_dir_of(Path::new("a/b")), Path::new("a/b"));
        // "." canonicalizes to a real directory, so the id is its name.
        assert!(scene_dir_of(Path::new(SCENE_FILE)).canonicalize().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_scene_files_and_dirs_are_read_and_cycles_skipped() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // A dotfile-manager layout: real files in a store, links in the scene.
        let store = dir.path().join("store");
        std::fs::create_dir_all(store.join("shared")).unwrap();
        std::fs::write(store.join(SCENE_FILE), "[scene]\nname = \"Linked\"\n").unwrap();
        std::fs::write(store.join("sand.png"), "png").unwrap();
        std::fs::write(store.join("shared/x.wgsl"), "// shared").unwrap();
        let scene = dir.path().join("mine");
        std::fs::create_dir_all(scene.join("textures")).unwrap();
        symlink(store.join(SCENE_FILE), scene.join(SCENE_FILE)).unwrap();
        symlink(store.join("sand.png"), scene.join("textures/sand.png")).unwrap();
        symlink(store.join("shared"), scene.join("shaders")).unwrap();
        symlink(&scene, scene.join("textures/loop")).unwrap();
        symlink(dir.path().join("gone"), scene.join("dangling")).unwrap();

        let entry = resolve(scene.to_str().unwrap()).expect("linked scene.toml resolves");
        let files = read(&entry.source).expect("linked scene reads");
        assert!(files.scene_toml().unwrap().contains("Linked"));
        assert!(files.contains("textures/sand.png"));
        assert_eq!(files.text("shaders/x.wgsl").unwrap(), "// shared");
        assert_eq!(files.files.len(), 3, "{:?}", files.files.keys());

        // Editing a link's target is picked up by hot reload.
        let before = stamp(&entry.source).unwrap();
        std::fs::write(store.join("sand.png"), "a bigger png").unwrap();
        assert_ne!(stamp(&entry.source).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_reached_by_many_links_is_walked_once() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // d0 → d1 → … → d20, each level linked twice: 2^20 paths to d20.
        const LEVELS: usize = 20;
        for i in 0..=LEVELS {
            std::fs::create_dir_all(dir.path().join(format!("d{i}"))).unwrap();
        }
        for i in 0..LEVELS {
            let next = dir.path().join(format!("d{}", i + 1));
            let here = dir.path().join(format!("d{i}"));
            symlink(&next, here.join("a")).unwrap();
            symlink(&next, here.join("b")).unwrap();
        }
        std::fs::write(dir.path().join(format!("d{LEVELS}/leaf.txt")), "x").unwrap();
        let mut out = Vec::new();
        walk_scene(&dir.path().join("d0"), &mut out).unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn asset_paths_live_under_the_slot() {
        assert_eq!(
            asset_path("minecraft", "textures/blocks.png").unwrap(),
            "embedded://bava/scene/minecraft/textures/blocks.png"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn user_scene_dirs_resolve_by_path_and_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let scene = dir.path().join("mine");
        std::fs::create_dir_all(scene.join("shaders")).unwrap();
        std::fs::write(scene.join(SCENE_FILE), "[scene]\nname = \"Mine\"\n").unwrap();
        std::fs::write(scene.join("shaders/x.wgsl"), "// hi").unwrap();

        let entry = resolve(scene.to_str().unwrap()).expect("dir resolves");
        assert_eq!(entry.id, "mine");
        let by_file = resolve(scene.join(SCENE_FILE).to_str().unwrap()).unwrap();
        assert_eq!(by_file.source, entry.source);

        let files = read(&entry.source).unwrap();
        assert_eq!(files.text("shaders/x.wgsl").unwrap(), "// hi");
        assert!(files.scene_toml().unwrap().contains("Mine"));

        // Touching a file changes the stamp.
        let before = stamp(&entry.source).unwrap();
        std::fs::write(scene.join("shaders/x.wgsl"), "// changed!").unwrap();
        assert_ne!(stamp(&entry.source).unwrap(), before);
        assert_eq!(stamp(&SceneSource::Builtin("minecraft")), None);
    }
}
