//! The Engine implementation: background thread, two calls per step.
//!
//! Navigation that needs no service call (Back, Redo, EnterChild, OpenWorld,
//! reuse of an existing discrete child) is handled synchronously inside
//! `submit`. Everything else runs on one worker thread per action, which
//! works on a snapshot of the world and commits it back when done. Only one
//! action runs at a time (`submit` answers `Busy` otherwise), so nothing else
//! mutates the world meanwhile.
//!
//! `busy()` is `Some` for the whole action: the service call in flight, or
//! `Service::Local` between calls. It is cleared before the action's final
//! events are sent, so a UI that reacts to `ViewChanged` can submit again
//! right away.

use crate::store::{self, StoreError};
use crossbeam_channel::{unbounded, Receiver, Sender};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};
use iz_types::imageops::{self, RectF};
use iz_types::log::{CallLog, LogEvent, LogKind};
use iz_types::transition::wrap_heading;
use iz_types::{
    Action, AncestorBrief, Backlink, BusyInfo, CallContext, Camera, CanvasMode, ChildOutline, DescribeError,
    DescribeKind, DescribeRequest, Describer, Engine, EngineError, EngineEvent, Node, NodeId, Outcome, PaintError,
    PaintRequest, Painter, SceneSpec, Service, Settings, StatusEvent, Transition, World, WorldId, WorldSummary,
    WorldView,
};
use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Instant;

/// Longest side of a stored source photo.
pub const PHOTO_MAX_SIDE: u32 = 4096;
/// Shown when the image model refused the original and the rewritten prompt.
pub const CANT_GO_FURTHER: &str = "Can't go further here.";
/// Id of the root node of every world (its image is `images/n0001.png`).
const ROOT_ID: NodeId = 1;

/// The real pipeline: Claude describes, the painter paints, worlds persist
/// under the app data dir. Also drives the mocks (see [`crate::mock_engine`]).
pub struct RealEngine {
    services: Arc<Services>,
    shared: Arc<Mutex<State>>,
    tx: Sender<EngineEvent>,
    rx: Receiver<EngineEvent>,
    /// Cancel flag of the running (or last) action; fresh per action.
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

struct Services {
    describer: Arc<dyn Describer>,
    painter: Arc<dyn Painter>,
    log: Arc<dyn CallLog>,
    describe_service: Service,
    paint_service: Service,
}

#[derive(Default)]
struct State {
    world: Option<World>,
    redo: Vec<NodeId>,
    busy: Option<BusyInfo>,
    running: bool,
}

fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panicking worker must not wedge the UI; the state stays usable.
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl RealEngine {
    /// Engine over the real services; status events name Claude and OpenAI.
    pub fn new(describer: Arc<dyn Describer>, painter: Arc<dyn Painter>, log: Arc<dyn CallLog>) -> RealEngine {
        Self::with_services(describer, painter, log, Service::Claude, Service::OpenAI)
    }

    /// Like [`RealEngine::new`] with the service names shown in status events
    /// (e.g. `Service::Mock` for the mocks).
    pub fn with_services(
        describer: Arc<dyn Describer>,
        painter: Arc<dyn Painter>,
        log: Arc<dyn CallLog>,
        describe_service: Service,
        paint_service: Service,
    ) -> RealEngine {
        let (tx, rx) = unbounded();
        RealEngine {
            services: Arc::new(Services { describer, painter, log, describe_service, paint_service }),
            shared: Arc::new(Mutex::new(State::default())),
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    /// True while an action runs on the worker thread.
    pub fn is_running(&self) -> bool {
        lock(&self.shared).running
    }

    /// Block until the running action (if any) has finished and sent all of
    /// its events. For tests and shutdown; the UI should poll instead.
    pub fn wait_idle(&mut self) {
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }

    /// A copy of the open world, if any.
    pub fn world_snapshot(&self) -> Option<World> {
        lock(&self.shared).world.clone()
    }

    /// Send an event, logging Info/Error to the world's log.
    fn notify(&self, world: Option<&World>, event: EngineEvent) {
        if let Some(entry) = log_entry(&event, "engine") {
            match world {
                Some(w) => store::world_log(&w.id, self.services.log.clone()).event(&entry),
                None => self.services.log.event(&entry),
            }
        }
        let _ = self.tx.send(event);
    }

    /// Decide what to do with an action. Instant ones are done here (with
    /// their events sent); generations come back as a job to run.
    fn route(&self, st: &mut State, action: Action, settings: &Settings) -> Result<Option<JobKind>, EngineError> {
        match action {
            Action::OpenPhoto(path) => Ok(Some(JobKind::OpenPhoto(path))),
            Action::Lucky => Ok(Some(JobKind::Lucky)),
            Action::OpenWorld(id) => {
                self.open_world(st, &id);
                Ok(None)
            }
            Action::Back => self.back(st).map(|()| None),
            Action::Redo => self.redo(st).map(|()| None),
            Action::EnterChild(id) => self.enter_child(st, id).map(|()| None),
            Action::Regenerate => {
                current_node(st)?;
                Ok(Some(JobKind::Regenerate))
            }
            Action::ZoomIn(rect) => {
                current_node(st)?;
                let rect = rect.clamped();
                if !(rect.w > 0.0 && rect.h > 0.0) {
                    return Err(EngineError::Other("The zoom rectangle is empty.".into()));
                }
                Ok(Some(JobKind::Step(Transition::ZoomIn { rect })))
            }
            Action::ZoomOut => {
                let factor = settings.zoom_out_factor;
                if !(factor > 1.0 && factor.is_finite()) {
                    return Err(EngineError::Other("The zoom-out factor must be greater than 1.".into()));
                }
                self.discrete(st, Transition::ZoomOut { factor })
            }
            Action::Tilt { .. } | Action::Rotate { .. } | Action::Home => {
                let camera = current_node(st)?.camera.clone();
                let (tilt_deg, heading_deg) = look_target(&camera, &action);
                let here =
                    Transition::Look { tilt_deg: camera.tilt_deg, heading_deg: wrap_heading(camera.heading_deg) };
                let target = Transition::Look { tilt_deg, heading_deg };
                if here.approx_eq(&target) {
                    self.notify(st.world.as_ref(), EngineEvent::Info("already looking that way".into()));
                    return Ok(None);
                }
                self.discrete(st, target)
            }
            Action::Forward => self.discrete(st, Transition::Forward { meters: settings.forward_step_m }),
            Action::Backward => self.discrete(st, Transition::Forward { meters: -settings.forward_step_m }),
        }
    }

    /// Reuse an existing child reached by the same discrete transition, or
    /// ask for a generation.
    fn discrete(&self, st: &mut State, t: Transition) -> Result<Option<JobKind>, EngineError> {
        let world = st.world.as_mut().ok_or(EngineError::NoWorld)?;
        match world.find_child(world.current, &t) {
            Some(child) => {
                st.redo.clear();
                self.go_to(world, child, Some(format!("cached: {}", t.label())));
                Ok(None)
            }
            None => Ok(Some(JobKind::Step(t))),
        }
    }

    fn open_world(&self, st: &mut State, id: &str) {
        match store::load(id) {
            Ok(mut world) if world.nodes.contains_key(&world.root) => {
                if !world.nodes.contains_key(&world.current) {
                    world.current = world.root;
                }
                st.world = Some(world);
                st.redo.clear();
                self.notify(None, EngineEvent::WorldOpened);
                self.notify(None, EngineEvent::ViewChanged);
            }
            Ok(_) => self.notify(None, EngineEvent::Error(format!("World {id} has no starting frame."))),
            Err(e) => self.notify(None, EngineEvent::Error(format!("Could not open world {id}: {e}"))),
        }
    }

    fn back(&self, st: &mut State) -> Result<(), EngineError> {
        let world = st.world.as_mut().ok_or(EngineError::NoWorld)?;
        let from = world.current;
        match world.node(from).and_then(|n| n.parent) {
            Some(parent) => {
                st.redo.push(from);
                self.go_to(world, parent, None);
            }
            None => self.notify(Some(world), EngineEvent::Info("Already at the start.".into())),
        }
        Ok(())
    }

    fn redo(&self, st: &mut State) -> Result<(), EngineError> {
        let world = st.world.as_mut().ok_or(EngineError::NoWorld)?;
        while let Some(next) = st.redo.pop() {
            if world.nodes.contains_key(&next) {
                self.go_to(world, next, None);
                return Ok(());
            }
        }
        self.notify(Some(world), EngineEvent::Info("Nothing to redo.".into()));
        Ok(())
    }

    fn enter_child(&self, st: &mut State, id: NodeId) -> Result<(), EngineError> {
        let world = st.world.as_mut().ok_or(EngineError::NoWorld)?;
        let listed = world.node(world.current).is_some_and(|n| n.children.contains(&id));
        if !listed || !world.nodes.contains_key(&id) {
            return Err(EngineError::Other(format!("Frame {id} is not reachable from here.")));
        }
        st.redo.clear();
        self.go_to(world, id, None);
        Ok(())
    }

    /// Move to an existing node, persist where the user is, tell the UI.
    fn go_to(&self, world: &mut World, to: NodeId, info: Option<String>) {
        world.current = to;
        world.updated_at = store::now_rfc3339();
        let saved = store::save(world);
        self.notify(Some(world), EngineEvent::ViewChanged);
        if let Some(info) = info {
            self.notify(Some(world), EngineEvent::Info(info));
        }
        if let Err(e) = saved {
            self.notify(Some(world), EngineEvent::Error(format!("Could not save the world: {e}")));
        }
    }
}

impl Drop for RealEngine {
    fn drop(&mut self) {
        // Let a running worker stop at its next check; don't block on it.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Engine for RealEngine {
    fn submit(&mut self, action: Action, settings: Settings) -> Result<(), EngineError> {
        if self.is_running() {
            return Err(EngineError::Busy);
        }
        // The previous worker, if any, is at most flushing its last events;
        // joining keeps event order intact.
        self.wait_idle();

        let mut guard = lock(&self.shared);
        let st = &mut *guard;
        let Some(kind) = self.route(st, action, &settings)? else {
            return Ok(());
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let attach = match kind {
            JobKind::Step(_) | JobKind::Regenerate => st.world.as_ref().map(|w| w.id.clone()),
            JobKind::OpenPhoto(_) | JobKind::Lucky => None,
        };
        let mut job = Job {
            label: kind.label(),
            steps: kind.steps(),
            kind: Some(kind),
            settings,
            services: self.services.clone(),
            shared: self.shared.clone(),
            tx: self.tx.clone(),
            cancel: cancel.clone(),
            log: self.services.log.clone(),
            record_dir: None,
            deferred: Vec::new(),
            step: 0,
        };
        if let Some(id) = attach {
            job.attach_world(&id);
        }
        st.running = true;
        st.busy = Some(BusyInfo {
            service: Service::Local,
            purpose: job.label.clone(),
            since: Instant::now(),
            step: "preparing".into(),
        });
        drop(guard);

        match std::thread::Builder::new().name("iz-engine".into()).spawn(move || job.run()) {
            Ok(handle) => {
                self.worker = Some(handle);
                self.cancel = cancel;
                Ok(())
            }
            Err(e) => {
                let mut st = lock(&self.shared);
                st.running = false;
                st.busy = None;
                Err(EngineError::Other(format!("Could not start the engine thread: {e}")))
            }
        }
    }

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn poll(&mut self) -> Vec<EngineEvent> {
        self.rx.try_iter().collect()
    }

    fn view(&self) -> Option<WorldView> {
        let st = lock(&self.shared);
        build_view(st.world.as_ref()?, !st.redo.is_empty())
    }

    fn list_worlds(&self) -> Vec<WorldSummary> {
        store::list_worlds()
    }

    fn busy(&self) -> Option<BusyInfo> {
        lock(&self.shared).busy.clone()
    }
}

fn current_node(st: &State) -> Result<&Node, EngineError> {
    let world = st.world.as_ref().ok_or(EngineError::NoWorld)?;
    world.node(world.current).ok_or(EngineError::NoWorld)
}

/// Absolute (tilt, heading) after a Tilt/Rotate/Home action.
fn look_target(camera: &Camera, action: &Action) -> (f32, f32) {
    match action {
        Action::Tilt { delta_deg } => {
            ((camera.tilt_deg + delta_deg).clamp(0.0, 90.0), wrap_heading(camera.heading_deg))
        }
        Action::Rotate { delta_deg } => {
            (camera.tilt_deg.clamp(0.0, 90.0), wrap_heading(camera.heading_deg + delta_deg))
        }
        _ => (0.0, 0.0),
    }
}

/// Short noun for purposes: "zoom in", "zoom out", "look", "forward"...
pub(crate) fn transition_noun(t: &Transition) -> &'static str {
    match t {
        Transition::Root => "root",
        Transition::ZoomIn { .. } => "zoom in",
        Transition::ZoomOut { .. } => "zoom out",
        Transition::Look { .. } => "look",
        Transition::Forward { meters } if *meters < 0.0 => "backward",
        Transition::Forward { .. } => "forward",
    }
}

/// Info and Error events are also written to the log.
fn log_entry(event: &EngineEvent, purpose: &str) -> Option<LogEvent> {
    let (kind, text) = match event {
        EngineEvent::Info(text) => (LogKind::Note, text),
        EngineEvent::Error(text) => (LogKind::Error, text),
        _ => return None,
    };
    Some(LogEvent::new(Service::Local.name(), purpose, kind, text.clone()))
}

pub(crate) fn build_view(world: &World, can_redo: bool) -> Option<WorldView> {
    let node = world.node(world.current)?;
    let depth = world.ancestry(node.id).len().saturating_sub(1);
    let children = node
        .children
        .iter()
        .filter_map(|&c| {
            Some(ChildOutline {
                node: c,
                transition: world.child_transition(node.id, c)?,
                title: world.node(c)?.title.clone(),
            })
        })
        .collect();
    Some(WorldView {
        world_id: world.id.clone(),
        world_title: world.title.clone(),
        current: node.id,
        image_path: store::abs_path(&world.id, &node.image),
        image_version: node.image_version,
        camera: node.camera.clone(),
        title: node.title.clone(),
        breadcrumb: breadcrumb(node, depth),
        children,
        can_back: node.parent.is_some(),
        can_redo,
        depth,
    })
}

/// "<regime> · <scale_label> · depth <n>"; before analysis, "<transition> · depth <n>".
fn breadcrumb(node: &Node, depth: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if node.spec.is_some() {
        for part in [&node.camera.regime, &node.camera.scale_label] {
            let part = part.trim();
            if !part.is_empty() {
                parts.push(part.to_string());
            }
        }
    } else {
        parts.push(node.transition.label());
    }
    parts.push(format!("depth {depth}"));
    parts.join(" · ")
}

/// Root-first briefs of the ancestors of `of`, excluding `of` itself.
fn ancestry_briefs(world: &World, of: NodeId) -> Vec<AncestorBrief> {
    let chain = world.ancestry(of);
    chain[..chain.len().saturating_sub(1)]
        .iter()
        .filter_map(|id| world.node(*id))
        .map(|n| AncestorBrief { title: n.title.clone(), transition: n.transition.clone(), camera: n.camera.clone() })
        .collect()
}

/// Record that `from` sits inside the zoom-out node `zoom_out` at `rect`.
/// A node has a single back-link: re-pointing it (a second zoom out with a
/// different factor) drops `from` from the previous zoom-out node's children,
/// which could no longer place it.
fn set_backlink(world: &mut World, from: NodeId, zoom_out: NodeId, rect: RectF) {
    let previous = world.node(from).and_then(|n| n.backlink.as_ref()).map(|b| b.zoom_out_node);
    if let Some(old) = previous.filter(|old| *old != zoom_out) {
        if let Some(z) = world.nodes.get_mut(&old) {
            z.children.retain(|c| *c != from);
        }
    }
    if let Some(n) = world.nodes.get_mut(&from) {
        n.backlink = Some(Backlink { zoom_out_node: zoom_out, rect });
    }
    if let Some(z) = world.nodes.get_mut(&zoom_out) {
        if !z.children.contains(&from) {
            z.children.push(from);
        }
    }
}

/// Remove every descendant of `id` from the node map (files stay on disk).
/// Only true children (whose `parent` is the node) are descended into, so a
/// zoom-out node's back-link child (its own parent) is kept. Returns the
/// removed ids.
pub(crate) fn remove_descendants(world: &mut World, id: NodeId) -> HashSet<NodeId> {
    let true_children = |world: &World, of: NodeId| -> Vec<NodeId> {
        world
            .node(of)
            .map(|n| {
                n.children.iter().copied().filter(|c| world.node(*c).and_then(|cn| cn.parent) == Some(of)).collect()
            })
            .unwrap_or_default()
    };
    let mut removed = HashSet::new();
    let mut stack = true_children(world, id);
    while let Some(c) = stack.pop() {
        if removed.insert(c) {
            stack.extend(true_children(world, c));
        }
    }
    world.nodes.retain(|nid, _| !removed.contains(nid));
    for n in world.nodes.values_mut() {
        n.children.retain(|c| !removed.contains(c));
        if n.backlink.as_ref().is_some_and(|b| removed.contains(&b.zoom_out_node)) {
            n.backlink = None;
        }
    }
    removed
}

/// Smallest unused node id whose image file does not exist yet (the app
/// never overwrites an orphaned frame).
fn alloc_node_id(world: &mut World) -> NodeId {
    let mut id = world.next_id.max(ROOT_ID + 1);
    while world.nodes.contains_key(&id) || store::abs_path(&world.id, &store::image_rel_path(id)).exists() {
        id += 1;
    }
    world.next_id = id + 1;
    id
}

fn new_world(id: WorldId, title: String, root: Node, source_photo: Option<PathBuf>) -> World {
    let now = store::now_rfc3339();
    World {
        id,
        title,
        created_at: now.clone(),
        updated_at: now,
        root: root.id,
        current: root.id,
        next_id: root.id + 1,
        nodes: BTreeMap::from([(root.id, root)]),
        source_photo,
    }
}

fn root_node(camera: Camera, spec: Option<SceneSpec>, title: String) -> Node {
    Node {
        id: ROOT_ID,
        parent: None,
        children: Vec::new(),
        transition: Transition::Root,
        camera,
        spec,
        image: store::image_rel_path(ROOT_ID),
        image_version: 0,
        title,
        created_at: store::now_rfc3339(),
        backlink: None,
    }
}

/// A root analysis names the frame and the world.
fn apply_root_spec(world: &mut World, id: NodeId, spec: SceneSpec) {
    world.title = spec.title.clone();
    if let Some(n) = world.nodes.get_mut(&id) {
        n.camera = spec.camera.clone();
        n.title = spec.title.clone();
        n.spec = Some(spec);
    }
}

/// Decode a photo, honouring its EXIF orientation.
fn decode_photo(bytes: &[u8]) -> Result<RgbaImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img.to_rgba8())
}

/// Downscale (never upscale) to fit inside `w` x `h`, keeping the aspect.
fn shrink_to_fit(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if img.width() <= w && img.height() <= h {
        img.clone()
    } else {
        imageops::fit_within(img, w, h)
    }
}

fn frame_size(settings: &Settings) -> (u32, u32) {
    (settings.frame_width.max(1), settings.frame_height.max(1))
}

fn paint_request(
    settings: &Settings,
    spec: &SceneSpec,
    purpose: String,
    canvas: Option<RgbaImage>,
    mask: Option<RgbaImage>,
    references: Vec<RgbaImage>,
) -> PaintRequest {
    let (width, height) = frame_size(settings);
    PaintRequest {
        model: settings.image_model.clone(),
        prompt: spec.render_prompt.clone(),
        width,
        height,
        quality: settings.image_quality,
        canvas,
        mask,
        references,
        purpose,
    }
}

// ---------------------------------------------------------------------------
// The worker side.

#[derive(Debug, Clone)]
enum JobKind {
    OpenPhoto(PathBuf),
    Lucky,
    /// A new node from the current one.
    Step(Transition),
    Regenerate,
}

impl JobKind {
    fn label(&self) -> String {
        match self {
            JobKind::OpenPhoto(_) => "open photo".into(),
            JobKind::Lucky => "feeling lucky".into(),
            JobKind::Step(t) => transition_noun(t).into(),
            JobKind::Regenerate => "regenerate".into(),
        }
    }

    /// Planned service calls, for the "1/2 describe" step label.
    fn steps(&self) -> usize {
        match self {
            JobKind::OpenPhoto(_) => 1,
            _ => 2,
        }
    }
}

/// Why an action stopped. The world is left unchanged (except a photo world
/// whose analysis failed, which is kept).
enum Fail {
    Cancelled,
    /// The image model refused the rewritten prompt too.
    Refused(String),
    Error(String),
}

impl From<StoreError> for Fail {
    fn from(e: StoreError) -> Self {
        Fail::Error(e.to_string())
    }
}

/// What a step produced, before it is committed.
struct Generated {
    spec: SceneSpec,
    image: RgbaImage,
    /// ZoomOut: where the frame being left sits in the new one.
    backlink_rect: Option<RectF>,
}

/// Paint inputs prepared before the describe call.
enum PaintInputs {
    /// The upscaled crop; how it is used depends on the spec's canvas mode.
    ZoomIn {
        crop: RgbaImage,
    },
    Outpaint {
        canvas: RgbaImage,
        mask: RgbaImage,
    },
    /// Look / Forward: the current frame plus the parent frame if any.
    References {
        parent: Option<RgbaImage>,
    },
}

enum RedoUpdate {
    Clear,
    Prune(HashSet<NodeId>),
}

struct Job {
    kind: Option<JobKind>,
    label: String,
    settings: Settings,
    services: Arc<Services>,
    shared: Arc<Mutex<State>>,
    tx: Sender<EngineEvent>,
    cancel: Arc<AtomicBool>,
    /// Stdout plus the world's log.txt once the world is known.
    log: Arc<dyn CallLog>,
    record_dir: Option<PathBuf>,
    /// Events sent only after the action is over (and `busy` is cleared).
    deferred: Vec<EngineEvent>,
    step: usize,
    steps: usize,
}

impl Job {
    fn run(mut self) {
        let result = catch_unwind(AssertUnwindSafe(|| self.execute()));
        let tail = match result {
            Ok(Ok(())) => None,
            Ok(Err(Fail::Cancelled)) => Some(EngineEvent::Info("cancelled".into())),
            Ok(Err(Fail::Refused(_))) => Some(EngineEvent::Error(CANT_GO_FURTHER.into())),
            Ok(Err(Fail::Error(msg))) => Some(EngineEvent::Error(msg)),
            Err(_) => Some(EngineEvent::Error("Internal error: the generation thread panicked.".into())),
        };
        if let Some(event) = tail {
            self.defer(event);
        }
        {
            let mut st = lock(&self.shared);
            st.running = false;
            st.busy = None;
        }
        for event in self.deferred.drain(..) {
            let _ = self.tx.send(event);
        }
    }

    fn execute(&mut self) -> Result<(), Fail> {
        match self.kind.take() {
            Some(JobKind::OpenPhoto(path)) => self.open_photo(&path),
            Some(JobKind::Lucky) => self.lucky(),
            Some(JobKind::Step(t)) => self.step_from_current(t),
            Some(JobKind::Regenerate) => self.regenerate(),
            None => Ok(()),
        }
    }

    // -- plumbing ----------------------------------------------------------

    fn attach_world(&mut self, world_id: &str) {
        self.log = store::world_log(world_id, self.services.log.clone());
        self.record_dir = Some(store::calls_dir(world_id));
    }

    fn ctx(&self) -> CallContext {
        CallContext { cancel: self.cancel.clone(), log: self.log.clone(), record_dir: self.record_dir.clone() }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn check_cancel(&self) -> Result<(), Fail> {
        if self.cancelled() {
            Err(Fail::Cancelled)
        } else {
            Ok(())
        }
    }

    fn log_event(&self, event: &EngineEvent) {
        if let Some(entry) = log_entry(event, &self.label) {
            self.log.event(&entry);
        }
    }

    fn send_now(&self, event: EngineEvent) {
        self.log_event(&event);
        let _ = self.tx.send(event);
    }

    fn defer(&mut self, event: EngineEvent) {
        self.log_event(&event);
        self.deferred.push(event);
    }

    fn set_busy(&self, busy: BusyInfo) {
        lock(&self.shared).busy = Some(busy);
    }

    fn snapshot(&self) -> Result<World, Fail> {
        lock(&self.shared).world.clone().ok_or_else(|| Fail::Error("No world is open.".into()))
    }

    /// Persist and publish a new state of the world.
    fn commit(&mut self, mut world: World, redo: RedoUpdate) {
        world.updated_at = store::now_rfc3339();
        let saved = store::save(&world);
        {
            let mut st = lock(&self.shared);
            st.world = Some(world);
            match redo {
                RedoUpdate::Clear => st.redo.clear(),
                RedoUpdate::Prune(removed) => st.redo.retain(|id| !removed.contains(id)),
            }
        }
        if let Err(e) = saved {
            self.defer(EngineEvent::Error(format!("Could not save the world: {e}")));
        }
    }

    fn write_thumb(&mut self, world_id: &str, img: &RgbaImage) {
        if let Err(e) = store::write_thumbnail(world_id, img) {
            self.defer(EngineEvent::Error(format!("Could not write the thumbnail: {e}")));
        }
    }

    fn begin_call(&mut self, service: Service, purpose: &str, verb: &str) -> Instant {
        self.step += 1;
        self.steps = self.steps.max(self.step);
        let at = Instant::now();
        self.set_busy(BusyInfo {
            service,
            purpose: purpose.to_string(),
            since: at,
            step: format!("{}/{} {verb}", self.step, self.steps),
        });
        self.send_now(EngineEvent::Status(StatusEvent::Started { service, purpose: purpose.to_string(), at }));
        at
    }

    fn end_call(&mut self, service: Service, purpose: &str, at: Instant, outcome: Outcome) {
        self.send_now(EngineEvent::Status(StatusEvent::Finished {
            service,
            purpose: purpose.to_string(),
            elapsed: at.elapsed(),
            outcome,
        }));
        self.set_busy(BusyInfo {
            service: Service::Local,
            purpose: self.label.clone(),
            since: Instant::now(),
            step: "working".into(),
        });
    }

    fn describe(&mut self, req: &DescribeRequest, purpose: &str) -> Result<SceneSpec, Fail> {
        self.check_cancel()?;
        let service = self.services.describe_service;
        let at = self.begin_call(service, purpose, "describe");
        let result = self.services.describer.describe(req, &self.ctx());
        let (outcome, result) = match result {
            _ if self.cancelled() => (Outcome::Cancelled, Err(Fail::Cancelled)),
            Ok(spec) => (Outcome::Ok, Ok(spec)),
            Err(DescribeError::Cancelled) => (Outcome::Cancelled, Err(Fail::Cancelled)),
            Err(e) => {
                (Outcome::Failed(e.to_string()), Err(Fail::Error(format!("{} ({purpose}): {e}", service.name()))))
            }
        };
        self.end_call(service, purpose, at, outcome);
        result
    }

    fn paint(&mut self, req: &PaintRequest) -> Result<RgbaImage, Fail> {
        self.check_cancel()?;
        let service = self.services.paint_service;
        let at = self.begin_call(service, &req.purpose, "paint");
        let result = self.services.painter.paint(req, &self.ctx());
        let (outcome, result) = match result {
            _ if self.cancelled() => (Outcome::Cancelled, Err(Fail::Cancelled)),
            Ok(painted) => (Outcome::Ok, Ok(imageops::opaque(&painted.image))),
            Err(PaintError::Cancelled) => (Outcome::Cancelled, Err(Fail::Cancelled)),
            Err(PaintError::Refused(msg)) => (Outcome::Refused, Err(Fail::Refused(msg))),
            Err(e) => {
                (Outcome::Failed(e.to_string()), Err(Fail::Error(format!("{} ({}): {e}", service.name(), req.purpose))))
            }
        };
        self.end_call(service, &req.purpose, at, outcome);
        result
    }

    /// Describe, paint; on a refusal, have the describer rewrite the prompt
    /// once and paint again. A second refusal is final.
    fn describe_and_paint(
        &mut self,
        req: DescribeRequest,
        noun: &str,
        build: &dyn Fn(&SceneSpec, String) -> PaintRequest,
    ) -> Result<(SceneSpec, RgbaImage), Fail> {
        let spec = self.describe(&req, &format!("describe {noun}"))?;
        let refusal = match self.paint(&build(&spec, format!("paint {noun}"))) {
            Ok(image) => return Ok((spec, image)),
            Err(Fail::Refused(msg)) => msg,
            Err(e) => return Err(e),
        };
        self.steps += 2;
        self.send_now(EngineEvent::Info(format!(
            "The image model refused ({refusal}); asking {} for a safer prompt.",
            self.services.describe_service.name()
        )));
        let rewrite =
            DescribeRequest { kind: DescribeKind::Rewrite { refused_message: refusal, previous: spec }, ..req };
        let spec = self.describe(&rewrite, &format!("rewrite {noun}"))?;
        let image = self.paint(&build(&spec, format!("paint {noun}")))?;
        Ok((spec, image))
    }

    // -- actions -----------------------------------------------------------

    fn open_photo(&mut self, path: &Path) -> Result<(), Fail> {
        let bytes = std::fs::read(path).map_err(|e| Fail::Error(format!("Could not read {}: {e}", path.display())))?;
        let mut photo =
            decode_photo(&bytes).map_err(|e| Fail::Error(format!("Could not decode {}: {e}", path.display())))?;
        if photo.width().max(photo.height()) > PHOTO_MAX_SIDE {
            photo = imageops::fit_within(&photo, PHOTO_MAX_SIDE, PHOTO_MAX_SIDE);
        }
        self.check_cancel()?;

        let (world_id, _) = store::create_world_dir(&store::photo_world_id(&bytes))?;
        self.attach_world(&world_id);
        store::save_image(&world_id, &store::image_rel_path(ROOT_ID), &photo)?;
        self.write_thumb(&world_id, &photo);
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
        let source = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let world = new_world(world_id, title.clone(), root_node(Camera::default(), None, title), Some(source));
        self.commit(world, RedoUpdate::Clear);
        // Show the photo while it is being analysed.
        self.send_now(EngineEvent::WorldOpened);
        self.send_now(EngineEvent::ViewChanged);
        self.send_now(EngineEvent::WorldsChanged);

        // From here on a failure keeps the (unanalysed) world.
        let world = self.snapshot()?;
        let spec = self.describe_root(&world)?;
        let mut world = self.snapshot()?;
        apply_root_spec(&mut world, ROOT_ID, spec);
        self.commit(world, RedoUpdate::Clear);
        self.defer(EngineEvent::ViewChanged);
        self.defer(EngineEvent::WorldsChanged);
        Ok(())
    }

    fn describe_root(&mut self, world: &World) -> Result<SceneSpec, Fail> {
        let req = DescribeRequest {
            kind: DescribeKind::Root,
            transition: Transition::Root,
            current_image: store::node_image_path(world, world.root),
            crop_image: None,
            parent_image: None,
            prior_spec: None,
            proposed_camera: None,
            ancestry: Vec::new(),
            world_dir: store::world_dir(&world.id),
            settings: self.settings.clone(),
        };
        self.describe(&req, "describe root")
    }

    fn lucky(&mut self) -> Result<(), Fail> {
        let (world_id, world_dir) = store::create_world_dir(&store::lucky_world_id())?;
        self.attach_world(&world_id);
        let (spec, image) = self.lucky_generate(world_dir)?;
        self.check_cancel()?;
        store::save_image(&world_id, &store::image_rel_path(ROOT_ID), &image)?;
        self.write_thumb(&world_id, &image);
        let title = spec.title.clone();
        let world = new_world(world_id, title.clone(), root_node(spec.camera.clone(), Some(spec), title), None);
        self.commit(world, RedoUpdate::Clear);
        self.defer(EngineEvent::WorldOpened);
        self.defer(EngineEvent::ViewChanged);
        self.defer(EngineEvent::WorldsChanged);
        Ok(())
    }

    fn lucky_generate(&mut self, world_dir: PathBuf) -> Result<(SceneSpec, RgbaImage), Fail> {
        let req = DescribeRequest {
            kind: DescribeKind::Lucky,
            transition: Transition::Root,
            current_image: None,
            crop_image: None,
            parent_image: None,
            prior_spec: None,
            proposed_camera: None,
            ancestry: Vec::new(),
            world_dir,
            settings: self.settings.clone(),
        };
        let settings = self.settings.clone();
        let build = |spec: &SceneSpec, purpose: String| paint_request(&settings, spec, purpose, None, None, Vec::new());
        self.describe_and_paint(req, "lucky", &build)
    }

    /// Describe and paint the frame reached from `from` by `t`.
    fn generate(&mut self, world: &World, from: NodeId, t: &Transition) -> Result<Generated, Fail> {
        let node = world.node(from).ok_or_else(|| Fail::Error(format!("Frame {from} is missing.")))?;
        let current_path = store::abs_path(&world.id, &node.image);
        let current = imageops::load_rgba(&current_path).map(|i| imageops::opaque(&i)).map_err(Fail::Error)?;
        let parent_path = node.parent.and_then(|p| store::node_image_path(world, p));
        let (w, h) = frame_size(&self.settings);
        let mut camera = node.camera.clone();
        let mut crop_image = None;
        let mut backlink_rect = None;
        // Crops and the zoom-out canvas use the full-resolution frame; the
        // frames passed as references never need more pixels than the output.
        let reference = shrink_to_fit(&current, w, h);
        let load_parent_reference = || {
            let parent = imageops::load_rgba(parent_path.as_deref()?).ok()?;
            Some(shrink_to_fit(&imageops::opaque(&parent), w, h))
        };

        let inputs = match t {
            Transition::ZoomIn { rect } => {
                let crop = imageops::resize_to(&imageops::crop_norm(&current, *rect), w, h);
                let crop_path = store::tmp_dir(&world.id).join("crop.png");
                imageops::save_png(&crop, &crop_path).map_err(Fail::Error)?;
                crop_image = Some(crop_path);
                camera.frame_width_m *= f64::from(rect.w);
                camera.distance_m *= f64::from(rect.w);
                PaintInputs::ZoomIn { crop }
            }
            Transition::ZoomOut { factor } => {
                let (canvas, mask, placed) = imageops::center_on_transparent(&current, w, h, 1.0 / factor);
                backlink_rect = Some(placed);
                camera.frame_width_m *= f64::from(*factor);
                camera.distance_m *= f64::from(*factor);
                PaintInputs::Outpaint { canvas, mask }
            }
            Transition::Look { tilt_deg, heading_deg } => {
                camera.tilt_deg = *tilt_deg;
                camera.heading_deg = *heading_deg;
                PaintInputs::References { parent: load_parent_reference() }
            }
            Transition::Forward { .. } => PaintInputs::References { parent: load_parent_reference() },
            Transition::Root => return Err(Fail::Error("A start frame cannot be stepped to.".into())),
        };

        let req = DescribeRequest {
            kind: DescribeKind::Step,
            transition: t.clone(),
            current_image: Some(current_path),
            crop_image,
            parent_image: parent_path,
            prior_spec: node.spec.clone(),
            proposed_camera: Some(camera),
            ancestry: ancestry_briefs(world, from),
            world_dir: store::world_dir(&world.id),
            settings: self.settings.clone(),
        };
        let settings = self.settings.clone();
        let build = |spec: &SceneSpec, purpose: String| {
            let (canvas, mask, references) = match &inputs {
                PaintInputs::ZoomIn { crop } => match spec.canvas_mode {
                    CanvasMode::Crop => (Some(crop.clone()), None, vec![reference.clone()]),
                    CanvasMode::Reference => (None, None, vec![crop.clone(), reference.clone()]),
                },
                PaintInputs::Outpaint { canvas, mask } => {
                    (Some(canvas.clone()), Some(mask.clone()), vec![reference.clone()])
                }
                PaintInputs::References { parent } => {
                    (None, None, std::iter::once(reference.clone()).chain(parent.iter().cloned()).collect())
                }
            };
            paint_request(&settings, spec, purpose, canvas, mask, references)
        };
        let (spec, image) = self.describe_and_paint(req, transition_noun(t), &build)?;
        Ok(Generated { spec, image, backlink_rect })
    }

    fn step_from_current(&mut self, t: Transition) -> Result<(), Fail> {
        let mut world = self.snapshot()?;
        let from = world.current;
        let generated = self.generate(&world, from, &t)?;
        self.check_cancel()?;

        let id = alloc_node_id(&mut world);
        let image = store::image_rel_path(id);
        store::save_image(&world.id, &image, &generated.image)?;
        let spec = generated.spec;
        let node = Node {
            id,
            parent: Some(from),
            children: Vec::new(),
            transition: t,
            camera: spec.camera.clone(),
            title: spec.title.clone(),
            spec: Some(spec),
            image,
            image_version: 0,
            created_at: store::now_rfc3339(),
            backlink: None,
        };
        world.nodes.insert(id, node);
        if let Some(parent) = world.nodes.get_mut(&from) {
            parent.children.push(id);
        }
        if let Some(rect) = generated.backlink_rect {
            set_backlink(&mut world, from, id, rect);
        }
        world.current = id;
        self.commit(world, RedoUpdate::Clear);
        self.defer(EngineEvent::ViewChanged);
        Ok(())
    }

    /// Re-run the current node's own transition from its parent, replace its
    /// image and discard its descendants.
    fn regenerate(&mut self) -> Result<(), Fail> {
        let mut world = self.snapshot()?;
        let id = world.current;
        let node = world.node(id).cloned().ok_or_else(|| Fail::Error("The current frame is missing.".into()))?;
        let mut worlds_changed = false;
        match (&node.transition, node.parent) {
            (Transition::Root, _) if world.source_photo.is_some() => {
                // The photo stays; only its analysis is redone.
                self.steps = 1;
                let spec = self.describe_root(&world)?;
                self.check_cancel()?;
                apply_root_spec(&mut world, id, spec);
                worlds_changed = true;
            }
            (Transition::Root, _) => {
                let (spec, image) = self.lucky_generate(store::world_dir(&world.id))?;
                self.check_cancel()?;
                store::save_image(&world.id, &node.image, &image)?;
                self.write_thumb(&world.id, &image);
                apply_root_spec(&mut world, id, spec);
                if let Some(n) = world.nodes.get_mut(&id) {
                    n.image_version += 1;
                }
                worlds_changed = true;
            }
            (t, Some(parent)) => {
                let generated = self.generate(&world, parent, t)?;
                self.check_cancel()?;
                store::save_image(&world.id, &node.image, &generated.image)?;
                if let Some(n) = world.nodes.get_mut(&id) {
                    n.camera = generated.spec.camera.clone();
                    n.title = generated.spec.title.clone();
                    n.spec = Some(generated.spec);
                    n.image_version += 1;
                }
                if let Some(rect) = generated.backlink_rect {
                    set_backlink(&mut world, parent, id, rect);
                }
            }
            (_, None) => return Err(Fail::Error("This frame has no parent to regenerate from.".into())),
        }
        let removed = remove_descendants(&mut world, id);
        let discarded = removed.len();
        self.commit(world, RedoUpdate::Prune(removed));
        if discarded > 0 {
            self.defer(EngineEvent::Info(format!("Regenerated; {discarded} later frame(s) discarded.")));
        }
        self.defer(EngineEvent::ViewChanged);
        if worlds_changed {
            self.defer(EngineEvent::WorldsChanged);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: NodeId, parent: Option<NodeId>, transition: Transition) -> Node {
        Node {
            id,
            parent,
            children: Vec::new(),
            transition,
            camera: Camera::default(),
            spec: None,
            image: store::image_rel_path(id),
            image_version: 0,
            title: format!("node {id}"),
            created_at: String::new(),
            backlink: None,
        }
    }

    fn world(nodes: Vec<Node>) -> World {
        World {
            id: "unit".into(),
            title: "unit".into(),
            created_at: String::new(),
            updated_at: String::new(),
            root: 1,
            current: 1,
            next_id: nodes.len() as NodeId + 1,
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            source_photo: None,
        }
    }

    /// 1 -> 2 (zoom in) -> 3 (zoom out, back-link to 2) -> 5 (look);
    /// 2 -> 4 (look); 2 also zoomed out once to 3.
    fn tree() -> World {
        let mut w = world(vec![
            node(1, None, Transition::Root),
            node(2, Some(1), Transition::ZoomIn { rect: RectF::new(0.1, 0.1, 0.5, 0.5) }),
            node(3, Some(2), Transition::ZoomOut { factor: 4.0 }),
            node(4, Some(2), Transition::Look { tilt_deg: 30.0, heading_deg: 0.0 }),
            node(5, Some(3), Transition::Look { tilt_deg: 60.0, heading_deg: 0.0 }),
        ]);
        w.nodes.get_mut(&1).unwrap().children = vec![2];
        w.nodes.get_mut(&2).unwrap().children = vec![3, 4];
        w.nodes.get_mut(&3).unwrap().children = vec![5];
        set_backlink(&mut w, 2, 3, RectF::centered(0.25, 0.25));
        w
    }

    #[test]
    fn regenerating_a_zoom_out_node_keeps_its_back_link_child() {
        let mut w = tree();
        assert_eq!(w.nodes[&3].children, vec![5, 2]);
        let removed = remove_descendants(&mut w, 3);
        assert_eq!(removed, HashSet::from([5]));
        assert_eq!(w.nodes.len(), 4);
        assert_eq!(w.nodes[&3].children, vec![2]);
        assert!(w.nodes[&2].backlink.is_some());
    }

    #[test]
    fn removing_descendants_clears_dangling_back_links() {
        let mut w = tree();
        let removed = remove_descendants(&mut w, 2);
        assert_eq!(removed, HashSet::from([3, 4, 5]));
        assert_eq!(w.nodes.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert!(w.nodes[&2].children.is_empty());
        assert_eq!(w.nodes[&2].backlink, None);
        assert_eq!(remove_descendants(&mut w, 1), HashSet::from([2]));
    }

    #[test]
    fn re_pointing_a_back_link_drops_the_stale_outline() {
        let mut w = tree();
        w.nodes.insert(6, node(6, Some(2), Transition::ZoomOut { factor: 2.0 }));
        w.nodes.get_mut(&2).unwrap().children.push(6);
        set_backlink(&mut w, 2, 6, RectF::centered(0.5, 0.5));
        assert_eq!(w.nodes[&3].children, vec![5]);
        assert_eq!(w.nodes[&6].children, vec![2]);
        assert_eq!(w.find_child(6, &Transition::ZoomIn { rect: RectF::centered(0.5, 0.5) }), Some(2));
        assert_eq!(w.find_child(2, &Transition::ZoomOut { factor: 4.0 }), Some(3));
    }

    #[test]
    fn view_shows_back_links_as_zoom_in_outlines() {
        let mut w = tree();
        w.current = 3;
        let view = build_view(&w, false).unwrap();
        assert_eq!(view.depth, 2);
        assert!(view.can_back && !view.can_redo);
        assert_eq!(view.breadcrumb, "zoom out ×4 · depth 2");
        let outline = view.children.iter().find(|c| c.node == 2).unwrap();
        assert_eq!(outline.transition, Transition::ZoomIn { rect: RectF::centered(0.25, 0.25) });
    }

    #[test]
    fn breadcrumb_uses_regime_and_scale_once_described() {
        let mut n = node(1, None, Transition::Root);
        assert_eq!(breadcrumb(&n, 0), "start · depth 0");
        n.camera.regime = "street".into();
        n.camera.scale_label = "about 20 m across".into();
        n.spec = Some(SceneSpec {
            title: "t".into(),
            render_prompt: String::new(),
            camera: n.camera.clone(),
            canvas_mode: CanvasMode::Reference,
            dossier: serde_json::Value::Null,
            notes: String::new(),
        });
        assert_eq!(breadcrumb(&n, 3), "street · about 20 m across · depth 3");
    }

    #[test]
    fn look_targets_clamp_and_wrap() {
        let cam = Camera { tilt_deg: 75.0, heading_deg: 330.0, ..Camera::default() };
        assert_eq!(look_target(&cam, &Action::Tilt { delta_deg: 30.0 }), (90.0, 330.0));
        assert_eq!(look_target(&cam, &Action::Tilt { delta_deg: -90.0 }), (0.0, 330.0));
        assert_eq!(look_target(&cam, &Action::Rotate { delta_deg: 45.0 }), (75.0, 15.0));
        assert_eq!(look_target(&cam, &Action::Home), (0.0, 0.0));
    }

    #[test]
    fn ancestry_briefs_exclude_the_node_itself() {
        let w = tree();
        let briefs = ancestry_briefs(&w, 5);
        assert_eq!(briefs.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), vec!["node 1", "node 2", "node 3"]);
        assert!(ancestry_briefs(&w, 1).is_empty());
    }
}
