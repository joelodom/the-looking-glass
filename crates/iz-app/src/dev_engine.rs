//! In-memory stand-in for the real engine, for UI development only; it is
//! deleted at integration. It fakes the timing (a "describe" then a "paint"
//! step of about a second each, with status events), cancel, the world tree
//! with outlines, back/redo, reuse of discrete transitions, and a refusal
//! (zooming in more than 50x at once) so every UI path can be exercised.
//! Images go to a temp dir that is removed when the engine is dropped.

use image::{Rgba, RgbaImage};
use iz_types::imageops::{self, RectF};
use iz_types::transition::wrap_heading;
use iz_types::{
    Action, BusyInfo, Camera, ChildOutline, Engine, EngineError, EngineEvent, NodeId, Outcome, Service, Settings,
    StatusEvent, Transition, WorldId, WorldSummary, WorldView,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const STEP_TIME: Duration = Duration::from_millis(if cfg!(test) { 150 } else { 1000 });
const MAX_ZOOM_PER_STEP: f32 = 50.0;

struct DevNode {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    transition: Transition,
    /// Set when this node was listed under a zoom-out node: (that node, rect).
    backlink: Option<(NodeId, RectF)>,
    camera: Camera,
    image: PathBuf,
    version: u32,
    title: String,
}

struct DevWorld {
    id: WorldId,
    title: String,
    updated_at: String,
    nodes: BTreeMap<NodeId, DevNode>,
    current: NodeId,
    next_id: NodeId,
    redo: Vec<NodeId>,
    dir: PathBuf,
}

impl DevWorld {
    fn cur(&self) -> &DevNode {
        &self.nodes[&self.current]
    }

    /// None when the child no longer exists (removed by a regenerate).
    fn child_transition(&self, from: NodeId, child: NodeId) -> Option<Transition> {
        let n = self.nodes.get(&child)?;
        Some(match n.backlink {
            Some((z, rect)) if z == from => Transition::ZoomIn { rect },
            _ => n.transition.clone(),
        })
    }

    fn find_child(&self, t: &Transition) -> Option<NodeId> {
        self.cur().children.iter().copied().find(|c| {
            self.child_transition(self.current, *c)
                .map(|ct| ct.approx_eq(t))
                .unwrap_or(false)
        })
    }

    fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = self.nodes[&id].parent;
        while let Some(p) = cur {
            d += 1;
            cur = self.nodes[&p].parent;
        }
        d
    }

    fn image_path(&self, id: NodeId, version: u32) -> PathBuf {
        self.dir.join(format!("n{id:04}-v{version}.png"))
    }

    fn thumb_path(&self) -> PathBuf {
        self.dir.join("thumb.png")
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Local::now().to_rfc3339();
    }

    fn summary(&self) -> WorldSummary {
        WorldSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            thumbnail: Some(self.thumb_path()),
            updated_at: self.updated_at.clone(),
            node_count: self.nodes.len(),
        }
    }

    fn view(&self) -> WorldView {
        let n = self.cur();
        let depth = self.depth(self.current);
        WorldView {
            world_id: self.id.clone(),
            world_title: self.title.clone(),
            current: self.current,
            image_path: n.image.clone(),
            image_version: n.version,
            camera: n.camera.clone(),
            title: n.title.clone(),
            breadcrumb: format!("{} · {} · depth {depth}", n.camera.regime, n.camera.scale_label),
            children: n
                .children
                .iter()
                .filter_map(|c| {
                    Some(ChildOutline {
                        node: *c,
                        transition: self.child_transition(self.current, *c)?,
                        title: self.nodes.get(c)?.title.clone(),
                    })
                })
                .collect(),
            can_back: n.parent.is_some(),
            can_redo: !self.redo.is_empty(),
            depth,
        }
    }

    /// Add a node under the current one and move there.
    fn add_child(
        &mut self,
        transition: Transition,
        camera: Camera,
        title: String,
        img: &RgbaImage,
    ) -> Result<NodeId, String> {
        let id = self.next_id;
        let path = self.image_path(id, 0);
        imageops::save_png(img, &path)?;
        self.next_id += 1;
        let parent = self.current;
        self.nodes.insert(
            id,
            DevNode {
                parent: Some(parent),
                children: vec![],
                transition,
                backlink: None,
                camera,
                image: path,
                version: 0,
                title,
            },
        );
        self.nodes.get_mut(&parent).expect("parent").children.push(id);
        self.current = id;
        self.redo.clear();
        self.touch();
        Ok(id)
    }

    fn remove_subtree(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.remove(&id) {
            for c in n.children {
                // A back-link child belongs to another branch; keep it.
                if self.nodes.get(&c).map(|cn| cn.parent == Some(id)).unwrap_or(false) {
                    self.remove_subtree(c);
                }
            }
        }
    }
}

enum Job {
    Photo(PathBuf),
    Lucky,
    Step(Transition),
    Regenerate,
}

impl Job {
    fn label(&self) -> String {
        match self {
            Job::Photo(_) => "photo".into(),
            Job::Lucky => "lucky".into(),
            Job::Step(t) => t.label(),
            Job::Regenerate => "regenerate".into(),
        }
    }

    fn steps(&self) -> Vec<(Service, &'static str)> {
        match self {
            Job::Photo(_) => vec![(Service::Claude, "analyse")],
            _ => vec![(Service::Claude, "describe"), (Service::OpenAI, "paint")],
        }
    }
}

struct State {
    root_dir: PathBuf,
    worlds: Vec<DevWorld>,
    open: Option<usize>,
    busy: Option<BusyInfo>,
    world_counter: u32,
}

impl State {
    fn world(&mut self) -> Option<&mut DevWorld> {
        self.open.and_then(|i| self.worlds.get_mut(i))
    }

    fn new_world(&mut self, title: String) -> DevWorld {
        self.world_counter += 1;
        let id = format!("dev-{:03}", self.world_counter);
        DevWorld {
            dir: self.root_dir.join(&id),
            id,
            title,
            updated_at: chrono::Local::now().to_rfc3339(),
            nodes: BTreeMap::new(),
            current: 0,
            next_id: 0,
            redo: vec![],
        }
    }
}

pub struct DevEngine {
    shared: Arc<Mutex<State>>,
    tx: Sender<EngineEvent>,
    rx: Receiver<EngineEvent>,
    cancel: Arc<AtomicBool>,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DevEngine {
    pub fn new() -> Self {
        // Unique per instance so parallel tests never share (or delete) a dir.
        static INSTANCES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = INSTANCES.fetch_add(1, Ordering::SeqCst);
        let root_dir = std::env::temp_dir().join(format!("iz-dev-engine-{}-{n}", std::process::id()));
        let mut state = State {
            root_dir,
            worlds: vec![],
            open: None,
            busy: None,
            world_counter: 0,
        };
        for (title, seed, minutes_ago) in [
            ("Dev world: dusk gradient", 7u64, 5i64),
            ("Dev world: sea glass", 42, 60 * 26),
        ] {
            match fake_world(&mut state, title, seed) {
                Ok(mut w) => {
                    w.updated_at = (chrono::Local::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339();
                    state.worlds.push(w);
                }
                Err(e) => eprintln!("[dev engine] could not create a fake world: {e}"),
            }
        }
        let (tx, rx) = channel();
        println!("[dev engine] in-memory stub; images under {}", state.root_dir.display());
        Self {
            shared: Arc::new(Mutex::new(state)),
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn send(&self, e: EngineEvent) {
        let _ = self.tx.send(e);
    }

    fn start(&self, st: &mut State, job: Job, settings: Settings) {
        let (service, verb) = job.steps()[0];
        st.busy = Some(BusyInfo {
            service,
            purpose: format!("{verb} {}", job.label()),
            since: Instant::now(),
            step: format!("1/{} {verb}", job.steps().len()),
        });
        self.cancel.store(false, Ordering::SeqCst);
        let (shared, tx, cancel) = (self.shared.clone(), self.tx.clone(), self.cancel.clone());
        std::thread::spawn(move || run_job(shared, tx, cancel, job, settings));
    }

    /// Discrete transition: reuse an existing child or generate one.
    fn step(&self, st: &mut State, t: Transition, settings: Settings) -> Result<(), EngineError> {
        let w = st.world().ok_or(EngineError::NoWorld)?;
        if let Some(existing) = w.find_child(&t) {
            w.current = existing;
            w.redo.clear();
            self.send(EngineEvent::ViewChanged);
            self.send(EngineEvent::Info(format!(
                "{}: reused the frame made earlier",
                t.label()
            )));
            return Ok(());
        }
        self.start(st, Job::Step(t), settings);
        Ok(())
    }

    fn look(&self, st: &mut State, tilt: f32, heading: f32, settings: Settings) -> Result<(), EngineError> {
        let cam = st.world().ok_or(EngineError::NoWorld)?.cur().camera.clone();
        let (tilt, heading) = (tilt.clamp(0.0, 90.0), wrap_heading(heading));
        if (tilt - cam.tilt_deg).abs() < 0.01 && (heading - wrap_heading(cam.heading_deg)).abs() < 0.01 {
            self.send(EngineEvent::Info("Already looking that way".into()));
            return Ok(());
        }
        self.step(
            st,
            Transition::Look {
                tilt_deg: tilt,
                heading_deg: heading,
            },
            settings,
        )
    }
}

impl Engine for DevEngine {
    fn submit(&mut self, action: Action, settings: Settings) -> Result<(), EngineError> {
        let shared = self.shared.clone();
        let mut st = lock(&shared);
        if st.busy.is_some() {
            return Err(EngineError::Busy);
        }
        match action {
            Action::OpenPhoto(path) => self.start(&mut st, Job::Photo(path), settings),
            Action::Lucky => self.start(&mut st, Job::Lucky, settings),
            Action::OpenWorld(id) => {
                let idx = st
                    .worlds
                    .iter()
                    .position(|w| w.id == id)
                    .ok_or_else(|| EngineError::Other(format!("no world {id}")))?;
                st.open = Some(idx);
                self.send(EngineEvent::WorldOpened);
                self.send(EngineEvent::ViewChanged);
            }
            Action::ZoomIn(rect) => {
                st.world().ok_or(EngineError::NoWorld)?;
                self.start(&mut st, Job::Step(Transition::ZoomIn { rect }), settings);
            }
            Action::ZoomOut => self.step(
                &mut st,
                Transition::ZoomOut {
                    factor: settings.zoom_out_factor,
                },
                settings,
            )?,
            Action::Tilt { delta_deg } => {
                let cam = st.world().ok_or(EngineError::NoWorld)?.cur().camera.clone();
                self.look(&mut st, cam.tilt_deg + delta_deg, cam.heading_deg, settings)?;
            }
            Action::Rotate { delta_deg } => {
                let cam = st.world().ok_or(EngineError::NoWorld)?.cur().camera.clone();
                self.look(&mut st, cam.tilt_deg, cam.heading_deg + delta_deg, settings)?;
            }
            Action::Home => self.look(&mut st, 0.0, 0.0, settings)?,
            Action::Forward => self.step(
                &mut st,
                Transition::Forward {
                    meters: settings.forward_step_m,
                },
                settings,
            )?,
            Action::Backward => self.step(
                &mut st,
                Transition::Forward {
                    meters: -settings.forward_step_m,
                },
                settings,
            )?,
            Action::Back => {
                let w = st.world().ok_or(EngineError::NoWorld)?;
                match w.cur().parent {
                    Some(p) => {
                        w.redo.push(w.current);
                        w.current = p;
                        self.send(EngineEvent::ViewChanged);
                    }
                    None => self.send(EngineEvent::Info("This is the first frame".into())),
                }
            }
            Action::Redo => {
                let w = st.world().ok_or(EngineError::NoWorld)?;
                match w.redo.pop() {
                    Some(n) if w.nodes.get(&n).and_then(|n| n.parent) == Some(w.current) => {
                        w.current = n;
                        self.send(EngineEvent::ViewChanged);
                    }
                    _ => {
                        w.redo.clear();
                        self.send(EngineEvent::Info("Nothing to redo".into()));
                    }
                }
            }
            Action::EnterChild(id) => {
                let w = st.world().ok_or(EngineError::NoWorld)?;
                if !w.cur().children.contains(&id) || !w.nodes.contains_key(&id) {
                    return Err(EngineError::Other(format!("frame {id} is not reachable from here")));
                }
                w.current = id;
                w.redo.clear();
                self.send(EngineEvent::ViewChanged);
            }
            Action::Regenerate => {
                st.world().ok_or(EngineError::NoWorld)?;
                self.start(&mut st, Job::Regenerate, settings);
            }
        }
        Ok(())
    }

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn poll(&mut self) -> Vec<EngineEvent> {
        self.rx.try_iter().collect()
    }

    fn view(&self) -> Option<WorldView> {
        let st = lock(&self.shared);
        st.open.and_then(|i| st.worlds.get(i)).map(|w| w.view())
    }

    fn list_worlds(&self) -> Vec<WorldSummary> {
        let st = lock(&self.shared);
        let mut list: Vec<WorldSummary> = st.worlds.iter().map(|w| w.summary()).collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list
    }

    fn busy(&self) -> Option<BusyInfo> {
        lock(&self.shared).busy.clone()
    }
}

impl Drop for DevEngine {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&lock(&self.shared).root_dir);
    }
}

/// Sleep in small slices; false if cancelled.
fn sleep_unless_cancelled(d: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    !cancel.load(Ordering::SeqCst)
}

fn run_job(shared: Arc<Mutex<State>>, tx: Sender<EngineEvent>, cancel: Arc<AtomicBool>, job: Job, settings: Settings) {
    let send = |e: EngineEvent| {
        let _ = tx.send(e);
    };
    let finish = |events: Vec<EngineEvent>| {
        lock(&shared).busy = None;
        for e in events {
            let _ = tx.send(e);
        }
    };

    // A photo opens at once; the analysis runs while it is on screen.
    if let Job::Photo(path) = &job {
        let opened = {
            let mut st = lock(&shared);
            photo_world(&mut st, path, &settings).map(|w| {
                st.worlds.push(w);
                st.open = Some(st.worlds.len() - 1);
            })
        };
        match opened {
            Ok(()) => {
                send(EngineEvent::WorldOpened);
                send(EngineEvent::ViewChanged);
                send(EngineEvent::WorldsChanged);
            }
            Err(e) => return finish(vec![EngineEvent::Error(format!("Could not open the photo: {e}"))]),
        }
    }

    let steps = job.steps();
    for (i, (service, verb)) in steps.iter().enumerate() {
        let purpose = format!("{verb} {}", job.label());
        let at = Instant::now();
        lock(&shared).busy = Some(BusyInfo {
            service: *service,
            purpose: purpose.clone(),
            since: at,
            step: format!("{}/{} {verb}", i + 1, steps.len()),
        });
        send(EngineEvent::Status(StatusEvent::Started {
            service: *service,
            purpose: purpose.clone(),
            at,
        }));
        let ok = sleep_unless_cancelled(STEP_TIME, &cancel);
        let refused = ok && *verb == "paint" && is_too_deep(&job);
        let outcome = match (ok, refused) {
            (false, _) => Outcome::Cancelled,
            (true, true) => Outcome::Refused,
            (true, false) => Outcome::Ok,
        };
        send(EngineEvent::Status(StatusEvent::Finished {
            service: *service,
            purpose,
            elapsed: at.elapsed(),
            outcome,
        }));
        if !ok {
            return finish(vec![EngineEvent::Info("Cancelled".into())]);
        }
        if refused {
            return finish(vec![EngineEvent::Error(format!(
                "Can't go further here: more than {MAX_ZOOM_PER_STEP}x in one step (dev engine refusal)"
            ))]);
        }
    }

    let result = {
        let mut st = lock(&shared);
        produce(&mut st, &job, &settings)
    };
    match result {
        Ok(mut events) => {
            events.push(EngineEvent::WorldsChanged);
            finish(events)
        }
        Err(e) => finish(vec![EngineEvent::Error(e)]),
    }
}

fn is_too_deep(job: &Job) -> bool {
    matches!(job, Job::Step(Transition::ZoomIn { rect }) if 1.0 / rect.w.max(1e-6) > MAX_ZOOM_PER_STEP)
}

/// Make the new frame for a finished job and update the world.
fn produce(st: &mut State, job: &Job, s: &Settings) -> Result<Vec<EngineEvent>, String> {
    let (fw, fh) = (s.frame_width.max(16), s.frame_height.max(16));
    match job {
        Job::Photo(_) => {
            let w = st.world().ok_or("no world")?;
            let root = w.current;
            let node = w.nodes.get_mut(&root).ok_or("no node")?;
            node.title = format!("{} (analysed)", node.title);
            node.camera.scale_label = label_for_width(node.camera.frame_width_m);
            Ok(vec![
                EngineEvent::ViewChanged,
                EngineEvent::Info("Photo analysed (dev engine)".into()),
            ])
        }
        Job::Lucky => {
            let seed = chrono::Local::now().timestamp_nanos_opt().unwrap_or(0) as u64 ^ std::process::id() as u64;
            let mut w = st.new_world(format!("Lucky #{}", seed % 1000));
            seed_root(&mut w, &gradient(fw, fh, seed), "A random place (dev)", 2_000.0)?;
            st.worlds.push(w);
            st.open = Some(st.worlds.len() - 1);
            Ok(vec![
                EngineEvent::WorldOpened,
                EngineEvent::ViewChanged,
                EngineEvent::Info("Lucky world created".into()),
            ])
        }
        Job::Regenerate => {
            let w = st.world().ok_or("no world")?;
            let id = w.current;
            let mut img = imageops::load_rgba(&w.cur().image)?;
            let n = w.cur().version as u64 + 1;
            imageops::tint(
                &mut img,
                [(n * 70 % 255) as u8, (n * 130 % 255) as u8, (n * 200 % 255) as u8],
                0.25,
            );
            let version = w.cur().version + 1;
            let path = w.image_path(id, version);
            imageops::save_png(&img, &path)?;
            let old_children = std::mem::take(&mut w.nodes.get_mut(&id).ok_or("no node")?.children);
            for c in old_children {
                if w.nodes.get(&c).map(|cn| cn.parent == Some(id)).unwrap_or(false) {
                    w.remove_subtree(c);
                }
            }
            let node = w.nodes.get_mut(&id).ok_or("no node")?;
            node.version = version;
            node.image = path;
            w.redo.clear();
            w.touch();
            Ok(vec![
                EngineEvent::ViewChanged,
                EngineEvent::Info("Regenerated; frames made from it were discarded".into()),
            ])
        }
        Job::Step(t) => {
            let w = st.world().ok_or("no world")?;
            let cur_img = imageops::load_rgba(&w.cur().image)?;
            let mut cam = w.cur().camera.clone();
            let (img, title, backlink) = match t {
                Transition::ZoomIn { rect } => {
                    let crop = imageops::crop_norm(&cur_img, *rect);
                    let mut img = imageops::resize_to(&crop, fw, fh);
                    imageops::tint(&mut img, [255, 255, 255], 0.08);
                    cam.frame_width_m *= rect.w.max(1e-6) as f64;
                    (img, format!("Closer: {}", label_for_width(cam.frame_width_m)), None)
                }
                Transition::ZoomOut { factor } => {
                    let (canvas, _mask, rect) =
                        imageops::center_on_transparent(&cur_img, fw, fh, 1.0 / factor.max(1.0));
                    let mut img = gradient(fw, fh, w.next_id * 31 + 5);
                    for (x, y, p) in canvas.enumerate_pixels() {
                        if p[3] > 0 {
                            img.put_pixel(x, y, *p);
                        }
                    }
                    cam.frame_width_m *= *factor as f64;
                    (
                        img,
                        format!("Wider: {}", label_for_width(cam.frame_width_m)),
                        Some(rect),
                    )
                }
                Transition::Look { tilt_deg, heading_deg } => {
                    let mut img = cur_img.clone();
                    imageops::tint(
                        &mut img,
                        [(heading_deg / 360.0 * 255.0) as u8, 90, (tilt_deg / 90.0 * 255.0) as u8],
                        0.3,
                    );
                    cam.tilt_deg = *tilt_deg;
                    cam.heading_deg = *heading_deg;
                    (
                        img,
                        format!("Looking tilt {tilt_deg:.0}°, heading {heading_deg:.0}°"),
                        None,
                    )
                }
                Transition::Forward { meters } => {
                    let mut img = cur_img.clone();
                    let c = if *meters >= 0.0 { [40, 200, 80] } else { [200, 80, 40] };
                    imageops::tint(&mut img, c, 0.2);
                    (
                        img,
                        format!(
                            "{} {:.0} m",
                            if *meters >= 0.0 { "Forward" } else { "Back" },
                            meters.abs()
                        ),
                        None,
                    )
                }
                Transition::Root => return Err("cannot step to a root".into()),
            };
            cam.regime = regime_for_width(cam.frame_width_m).into();
            cam.scale_label = label_for_width(cam.frame_width_m);
            let from = w.current;
            let id = w.add_child(t.clone(), cam, title, &img)?;
            if let Some(rect) = backlink {
                // The frame we zoomed out from is also a child of the new one.
                w.nodes.get_mut(&from).ok_or("no node")?.backlink = Some((id, rect));
                w.nodes.get_mut(&id).ok_or("no node")?.children.push(from);
            }
            Ok(vec![EngineEvent::ViewChanged])
        }
    }
}

/// Give a fresh world its root node and thumbnail.
fn seed_root(w: &mut DevWorld, img: &RgbaImage, title: &str, width_m: f64) -> Result<(), String> {
    let path = w.image_path(0, 0);
    imageops::save_png(img, &path)?;
    imageops::save_png(&imageops::thumbnail(img, 256), &w.thumb_path())?;
    let camera = Camera {
        frame_width_m: width_m,
        regime: regime_for_width(width_m).into(),
        scale_label: label_for_width(width_m),
        ..Camera::default()
    };
    w.nodes.insert(
        0,
        DevNode {
            parent: None,
            children: vec![],
            transition: Transition::Root,
            backlink: None,
            camera,
            image: path,
            version: 0,
            title: title.into(),
        },
    );
    w.current = 0;
    w.next_id = 1;
    Ok(())
}

fn photo_world(st: &mut State, path: &Path, s: &Settings) -> Result<DevWorld, String> {
    let img = imageops::load_rgba(path)?;
    let img = imageops::fit_within(&img, s.frame_width.max(256) * 2, s.frame_height.max(256) * 2);
    let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("photo").to_string();
    let mut w = st.new_world(name.clone());
    seed_root(&mut w, &img, &name, 500.0)?;
    Ok(w)
}

/// A small saved world with two zoom-ins and a look so outlines render.
fn fake_world(st: &mut State, title: &str, seed: u64) -> Result<DevWorld, String> {
    let (fw, fh) = (768, 512);
    let mut w = st.new_world(title.to_string());
    let root = gradient(fw, fh, seed);
    seed_root(&mut w, &root, title, 5_000.0)?;
    let rects = [RectF::new(0.1, 0.15, 0.3, 0.3), RectF::new(0.55, 0.5, 0.25, 0.25)];
    for (i, rect) in rects.into_iter().enumerate() {
        w.current = 0;
        let img = imageops::resize_to(&imageops::crop_norm(&root, rect), fw, fh);
        let cam = Camera {
            frame_width_m: 5_000.0 * rect.w as f64,
            ..w.nodes[&0].camera.clone()
        };
        w.add_child(Transition::ZoomIn { rect }, cam, format!("Detail {}", i + 1), &img)?;
    }
    w.current = 0;
    let mut look = root.clone();
    imageops::tint(&mut look, [30, 60, 200], 0.3);
    let cam = Camera {
        tilt_deg: 30.0,
        ..w.nodes[&0].camera.clone()
    };
    w.add_child(
        Transition::Look {
            tilt_deg: 30.0,
            heading_deg: 0.0,
        },
        cam,
        "Tilted a little".into(),
        &look,
    )?;
    w.current = 0;
    Ok(w)
}

/// A smooth two-color gradient with a few soft discs, from a seed.
fn gradient(w: u32, h: u32, seed: u64) -> RgbaImage {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((s >> 33) % 256) as f32
    };
    let a = [next(), next(), next()];
    let b = [next(), next(), next()];
    let discs: Vec<(f32, f32, f32)> = (0..5)
        .map(|_| (next() / 255.0, next() / 255.0, 0.05 + next() / 255.0 * 0.15))
        .collect();
    RgbaImage::from_fn(w, h, |x, y| {
        let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
        let t = (u + v) / 2.0;
        let mut c = [0.0f32; 3];
        for i in 0..3 {
            c[i] = a[i] * (1.0 - t) + b[i] * t;
        }
        for (dx, dy, r) in &discs {
            let d = ((u - dx).powi(2) + (v - dy).powi(2) * 0.44).sqrt();
            if d < *r {
                let k = 1.0 - d / r;
                for ch in &mut c {
                    *ch = *ch * (1.0 - 0.4 * k) + 255.0 * 0.4 * k;
                }
            }
        }
        Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255])
    })
}

fn regime_for_width(m: f64) -> &'static str {
    match m {
        m if m > 1e7 => "planetary",
        m if m > 1e4 => "geographic",
        m if m > 10.0 => "street",
        m if m > 0.1 => "human",
        m if m > 1e-3 => "eye",
        m if m > 1e-5 => "cellular",
        m if m > 1e-9 => "molecular",
        _ => "imaginary",
    }
}

fn label_for_width(m: f64) -> String {
    let (v, unit) = match m {
        m if m >= 1000.0 => (m / 1000.0, "km"),
        m if m >= 1.0 => (m, "m"),
        m if m >= 1e-3 => (m * 1e3, "mm"),
        m if m >= 1e-6 => (m * 1e6, "µm"),
        m => (m * 1e9, "nm"),
    };
    let digits = if v >= 100.0 {
        0
    } else if v >= 10.0 {
        1
    } else {
        2
    };
    format!("about {v:.digits$} {unit} across")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_idle(e: &mut DevEngine) -> Vec<EngineEvent> {
        let mut events = vec![];
        let end = Instant::now() + Duration::from_secs(10);
        while e.busy().is_some() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        events.extend(e.poll());
        events
    }

    #[test]
    fn stub_flows_behave_like_an_engine() {
        let mut e = DevEngine::new();
        let s = Settings {
            frame_width: 384,
            frame_height: 256,
            ..Settings::default()
        };
        let worlds = e.list_worlds();
        assert_eq!(worlds.len(), 2);
        assert!(worlds
            .iter()
            .all(|w| w.thumbnail.as_ref().map(|p| p.is_file()).unwrap_or(false)));
        assert!(matches!(
            e.submit(Action::ZoomOut, s.clone()),
            Err(EngineError::NoWorld)
        ));

        e.submit(Action::OpenWorld(worlds[0].id.clone()), s.clone()).unwrap();
        assert!(e.poll().contains(&EngineEvent::WorldOpened));
        let v = e.view().unwrap();
        assert_eq!(v.children.len(), 3);
        assert!(v.image_path.is_file());

        // A generation: busy at once, a second submit is refused.
        e.submit(Action::ZoomIn(RectF::new(0.25, 0.25, 0.5, 0.5)), s.clone())
            .unwrap();
        let busy = e.busy().expect("busy right after submit");
        assert_eq!(busy.service, Service::Claude);
        assert!(matches!(e.submit(Action::Back, s.clone()), Err(EngineError::Busy)));
        let events = wait_idle(&mut e);
        assert!(events.contains(&EngineEvent::ViewChanged), "{events:?}");
        assert!(events.iter().any(|ev| matches!(
            ev,
            EngineEvent::Status(StatusEvent::Finished {
                service: Service::OpenAI,
                outcome: Outcome::Ok,
                ..
            })
        )));
        let v = e.view().unwrap();
        assert_eq!(v.depth, 1);

        // Back and redo are instant.
        e.submit(Action::Back, s.clone()).unwrap();
        assert_eq!(e.view().unwrap().depth, 0);
        e.submit(Action::Redo, s.clone()).unwrap();
        assert_eq!(e.view().unwrap().current, v.current);

        // Cancel discards the step.
        e.submit(Action::Forward, s.clone()).unwrap();
        e.cancel();
        let events = wait_idle(&mut e);
        assert!(events.contains(&EngineEvent::Info("Cancelled".into())), "{events:?}");
        assert_eq!(e.view().unwrap().current, v.current);

        // Zoom out makes a node whose outline points back at the frame we left.
        e.submit(Action::ZoomOut, s.clone()).unwrap();
        wait_idle(&mut e);
        let z = e.view().unwrap();
        assert!(z
            .children
            .iter()
            .any(|c| c.node == v.current && matches!(c.transition, Transition::ZoomIn { .. })));
        // Repeating a discrete transition from the same node reuses it.
        e.submit(Action::Back, s.clone()).unwrap();
        e.submit(Action::ZoomOut, s.clone()).unwrap();
        assert!(e.busy().is_none());
        assert_eq!(e.view().unwrap().current, z.current);
    }
}
