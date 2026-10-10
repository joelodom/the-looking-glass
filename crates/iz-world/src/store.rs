//! Loading and saving worlds under the app data dir.
//!
//! One world lives in `worlds_dir()/<world id>/`:
//!
//! - `world.json`: the [`World`] tree, written atomically.
//! - `images/nNNNN.png`: one frame per node.
//! - `thumb.png`: about 320 px, from the root image.
//! - `calls/`: service call records (the `CallContext::record_dir`).
//! - `tmp/`: scratch files such as the upscaled zoom-in crop.
//! - `log.txt`: every [`LogEvent`] of the world, appended.
//!
//! The app never deletes anything here.

use chrono::{DateTime, SecondsFormat, Utc};
use image::RgbaImage;
use iz_types::log::{CallLog, LogEvent, LogKind, MultiLog};
use iz_types::settings::{paths, write_atomic};
use iz_types::{imageops, NodeId, World, WorldId, WorldSummary};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const WORLD_FILE: &str = "world.json";
pub const IMAGES_DIR: &str = "images";
pub const CALLS_DIR: &str = "calls";
pub const TMP_DIR: &str = "tmp";
pub const THUMB_FILE: &str = "thumb.png";
pub const LOG_FILE: &str = "log.txt";
/// Longest side of `thumb.png`.
pub const THUMB_SIZE: u32 = 320;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{0}")]
    Image(String),
    #[error("invalid world id {0:?}")]
    InvalidId(String),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io { path: path.to_path_buf(), source }
}

/// Current time as RFC 3339 UTC with a `Z` suffix and fixed millisecond
/// precision, so timestamps sort correctly as strings.
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn compact_timestamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// `<first 12 hex chars of sha256(photo)>-<compact UTC timestamp>`.
pub fn photo_world_id(photo_bytes: &[u8]) -> WorldId {
    let mut hasher = Sha256::new();
    hasher.update(photo_bytes);
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("{hex}-{}", compact_timestamp())
}

/// `<12 random hex chars>-<compact UTC timestamp>`, for "feeling lucky".
pub fn lucky_world_id() -> WorldId {
    let bits = rand::random::<u64>() & 0xffff_ffff_ffff;
    format!("{bits:012x}-{}", compact_timestamp())
}

/// World ids are single path components produced by this module; reject
/// anything that could escape the worlds directory.
fn check_id(id: &str) -> Result<(), StoreError> {
    let ok =
        !id.is_empty() && !id.starts_with('.') && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidId(id.to_string()))
    }
}

pub fn world_dir(id: &str) -> PathBuf {
    paths::worlds_dir().join(id)
}

pub fn world_file(id: &str) -> PathBuf {
    world_dir(id).join(WORLD_FILE)
}

pub fn calls_dir(id: &str) -> PathBuf {
    world_dir(id).join(CALLS_DIR)
}

pub fn tmp_dir(id: &str) -> PathBuf {
    world_dir(id).join(TMP_DIR)
}

pub fn thumb_path(id: &str) -> PathBuf {
    world_dir(id).join(THUMB_FILE)
}

pub fn log_path(id: &str) -> PathBuf {
    world_dir(id).join(LOG_FILE)
}

/// Relative image path of a node, e.g. `images/n0003.png`.
pub fn image_rel_path(node: NodeId) -> PathBuf {
    Path::new(IMAGES_DIR).join(format!("n{node:04}.png"))
}

/// Absolute path of a file inside a world directory.
pub fn abs_path(world_id: &str, rel: &Path) -> PathBuf {
    world_dir(world_id).join(rel)
}

/// Absolute path of a node's image, if the node exists.
pub fn node_image_path(world: &World, node: NodeId) -> Option<PathBuf> {
    world.node(node).map(|n| abs_path(&world.id, &n.image))
}

/// Create a fresh world directory (with `images/`, `calls/`, `tmp/`) for
/// `base_id`. If that directory already exists (same photo opened twice in
/// one second), `-2`, `-3`, ... is appended. Returns the id actually used.
pub fn create_world_dir(base_id: &str) -> Result<(WorldId, PathBuf), StoreError> {
    check_id(base_id)?;
    let root = paths::worlds_dir();
    fs::create_dir_all(&root).map_err(io_err(&root))?;
    for n in 1u32.. {
        let id = if n == 1 { base_id.to_string() } else { format!("{base_id}-{n}") };
        let dir = root.join(&id);
        match fs::create_dir(&dir) {
            Ok(()) => {
                for sub in [IMAGES_DIR, CALLS_DIR, TMP_DIR] {
                    let p = dir.join(sub);
                    fs::create_dir_all(&p).map_err(io_err(&p))?;
                }
                return Ok((id, dir));
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err(&dir)(e)),
        }
    }
    unreachable!("u32 range exhausted while picking a world id")
}

/// Read `world.json`. The returned world's id is the directory name.
pub fn load(id: &str) -> Result<World, StoreError> {
    check_id(id)?;
    let path = world_file(id);
    let text = fs::read_to_string(&path).map_err(io_err(&path))?;
    let mut world: World =
        serde_json::from_str(&text).map_err(|source| StoreError::Json { path: path.clone(), source })?;
    world.id = id.to_string();
    Ok(world)
}

/// Write `world.json` atomically. Does not touch `updated_at`.
pub fn save(world: &World) -> Result<(), StoreError> {
    check_id(&world.id)?;
    let dir = world_dir(&world.id);
    fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    let path = dir.join(WORLD_FILE);
    let text = serde_json::to_string_pretty(world).map_err(|source| StoreError::Json { path: path.clone(), source })?;
    write_atomic(&path, text.as_bytes()).map_err(io_err(&path))
}

/// Save a frame under the world directory at `rel` (creating `images/`).
pub fn save_image(world_id: &str, rel: &Path, img: &RgbaImage) -> Result<PathBuf, StoreError> {
    let path = abs_path(world_id, rel);
    imageops::save_png(img, &path).map_err(StoreError::Image)?;
    Ok(path)
}

/// Write `thumb.png` from the root image.
pub fn write_thumbnail(world_id: &str, img: &RgbaImage) -> Result<(), StoreError> {
    let thumb = imageops::thumbnail(img, THUMB_SIZE);
    imageops::save_png(&thumb, &thumb_path(world_id)).map_err(StoreError::Image)
}

/// Saved worlds, most recently updated first. Directories without a
/// readable `world.json` are skipped.
pub fn list_worlds() -> Vec<WorldSummary> {
    let Ok(entries) = fs::read_dir(paths::worlds_dir()) else {
        return Vec::new();
    };
    let mut worlds: Vec<WorldSummary> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| load(e.file_name().to_str()?).ok())
        .map(|w| {
            let thumb = thumb_path(&w.id);
            WorldSummary {
                thumbnail: thumb.is_file().then_some(thumb),
                node_count: w.nodes.len(),
                id: w.id,
                title: w.title,
                updated_at: w.updated_at,
            }
        })
        .collect();
    worlds.sort_by(|a, b| {
        let key = |s: &WorldSummary| DateTime::parse_from_rfc3339(&s.updated_at).ok();
        key(b).cmp(&key(a)).then_with(|| b.updated_at.cmp(&a.updated_at)).then_with(|| a.id.cmp(&b.id))
    });
    worlds
}

/// Appends every event to a text file (a world's `log.txt`).
pub struct FileLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, text: &str) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        file.write_all(text.as_bytes())
    }
}

impl CallLog for FileLog {
    fn event(&self, e: &LogEvent) {
        let kind = match e.kind {
            LogKind::Request => "REQUEST",
            LogKind::Response => "RESPONSE",
            LogKind::Note => "NOTE",
            LogKind::Error => "ERROR",
        };
        let text = format!(
            "===== [{}] {} · {} · {kind} =====\n{}\n===== end {kind} =====\n\n",
            now_rfc3339(),
            e.service,
            e.purpose,
            e.text
        );
        // Logging must never fail a generation; report and carry on.
        if let Err(err) = self.append(&text) {
            eprintln!("iz-world: cannot append to {}: {err}", self.path.display());
        }
    }
}

/// The log for one world: `base` (usually stdout) plus the world's `log.txt`.
pub fn world_log(world_id: &str, base: Arc<dyn CallLog>) -> Arc<dyn CallLog> {
    Arc::new(MultiLog(vec![base, Arc::new(FileLog::new(log_path(world_id)))]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::data_dir;
    use iz_types::{Backlink, Camera, Node, RectF, Transition};
    use std::collections::BTreeMap;

    fn node(id: NodeId, parent: Option<NodeId>, transition: Transition) -> Node {
        Node {
            id,
            parent,
            children: Vec::new(),
            transition,
            camera: Camera::default(),
            spec: None,
            image: image_rel_path(id),
            image_version: 0,
            title: format!("node {id}"),
            created_at: now_rfc3339(),
            backlink: None,
        }
    }

    /// root(1) -zoom in-> 2 -zoom out-> 3 (with back-link 3 => 2), 2 -look-> 4
    fn sample_world(id: &str) -> World {
        let mut nodes = BTreeMap::new();
        let mut n1 = node(1, None, Transition::Root);
        let mut n2 = node(2, Some(1), Transition::ZoomIn { rect: RectF::new(0.25, 0.25, 0.5, 0.5) });
        let mut n3 = node(3, Some(2), Transition::ZoomOut { factor: 4.0 });
        let n4 = node(4, Some(2), Transition::Look { tilt_deg: 30.0, heading_deg: 90.0 });
        n1.children = vec![2];
        n2.children = vec![3, 4];
        n2.backlink = Some(Backlink { zoom_out_node: 3, rect: RectF::centered(0.25, 0.25) });
        n3.children = vec![2];
        for n in [n1, n2, n3, n4] {
            nodes.insert(n.id, n);
        }
        World {
            id: id.to_string(),
            title: "sample".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            root: 1,
            current: 4,
            next_id: 5,
            nodes,
            source_photo: Some(PathBuf::from("/tmp/photo.png")),
        }
    }

    #[test]
    fn ids_have_the_documented_shape() {
        data_dir();
        let a = photo_world_id(b"same bytes");
        let b = photo_world_id(b"same bytes");
        assert_eq!(&a[..12], &b[..12]);
        assert!(a[..12].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.as_bytes()[12], b'-');
        assert_ne!(&photo_world_id(b"other bytes")[..12], &a[..12]);
        let l = lucky_world_id();
        assert!(l[..12].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(check_id(&a).is_ok() && check_id(&l).is_ok());
        assert!(check_id("../etc").is_err() && check_id("a/b").is_err() && check_id("").is_err());
    }

    #[test]
    fn create_world_dir_never_reuses_a_directory() {
        data_dir();
        let base = photo_world_id(b"collision test");
        let (id1, dir1) = create_world_dir(&base).unwrap();
        let (id2, dir2) = create_world_dir(&base).unwrap();
        assert_eq!(id1, base);
        assert_eq!(id2, format!("{base}-2"));
        assert_ne!(dir1, dir2);
        for sub in [IMAGES_DIR, CALLS_DIR, TMP_DIR] {
            assert!(dir2.join(sub).is_dir());
        }
    }

    #[test]
    fn save_load_round_trip_and_listing() {
        data_dir();
        let (id, _) = create_world_dir(&lucky_world_id()).unwrap();
        let mut world = sample_world(&id);
        save(&world).unwrap();
        assert_eq!(load(&id).unwrap(), world);

        let img = RgbaImage::from_pixel(640, 480, image::Rgba([10, 20, 30, 255]));
        save_image(&id, &image_rel_path(1), &img).unwrap();
        write_thumbnail(&id, &img).unwrap();
        assert!(abs_path(&id, Path::new("images/n0001.png")).is_file());
        let thumb = imageops::load_rgba(&thumb_path(&id)).unwrap();
        assert_eq!(thumb.dimensions(), (320, 240));

        // A second, older world sorts after this one.
        let (old_id, _) = create_world_dir(&lucky_world_id()).unwrap();
        let mut old = sample_world(&old_id);
        old.updated_at = "2001-01-01T00:00:00.000Z".into();
        save(&old).unwrap();
        world.updated_at = "2099-01-01T00:00:00.000Z".into();
        save(&world).unwrap();
        // An unreadable directory is skipped.
        fs::create_dir_all(world_dir("zz-broken")).unwrap();
        fs::write(world_file("zz-broken"), "{ not json").unwrap();

        let list = list_worlds();
        let pos = |id: &str| list.iter().position(|w| w.id == id).unwrap();
        assert!(pos(&id) < pos(&old_id));
        assert!(list.iter().all(|w| w.id != "zz-broken"));
        let summary = &list[pos(&id)];
        assert_eq!(summary.node_count, 4);
        assert_eq!(summary.thumbnail.as_deref(), Some(thumb_path(&id).as_path()));
        assert_eq!(list[pos(&old_id)].thumbnail, None);
    }

    #[test]
    fn world_tree_ops() {
        let world = sample_world("tree");
        assert_eq!(world.ancestry(4), vec![1, 2, 4]);
        assert_eq!(world.ancestry(3), vec![1, 2, 3]);
        assert_eq!(world.find_child(2, &Transition::ZoomOut { factor: 4.0 }), Some(3));
        assert_eq!(world.find_child(2, &Transition::ZoomOut { factor: 2.0 }), None);
        assert_eq!(world.find_child(2, &Transition::Look { tilt_deg: 30.0, heading_deg: 450.0 }), Some(4));
        // The back-link child is matched on its synthesized centered rect.
        let centered = Transition::ZoomIn { rect: RectF::centered(0.25, 0.25) };
        assert_eq!(world.find_child(3, &centered), Some(2));
        assert_eq!(world.child_transition(3, 2), Some(centered));
        // Under its real parent the node keeps its own transition.
        assert_eq!(world.child_transition(1, 2), Some(world.nodes[&2].transition.clone()));
    }

    #[test]
    fn file_log_appends() {
        data_dir();
        let (id, _) = create_world_dir(&lucky_world_id()).unwrap();
        let log = world_log(&id, Arc::new(crate::test_support::NullLog));
        log.event(&LogEvent::new("mock", "describe root", LogKind::Request, "first"));
        log.event(&LogEvent::new("mock", "describe root", LogKind::Response, "second"));
        let text = fs::read_to_string(log_path(&id)).unwrap();
        assert!(text.contains("first") && text.contains("second"));
        assert!(text.find("first") < text.find("second"));
    }
}
