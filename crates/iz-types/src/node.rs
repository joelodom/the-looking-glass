use crate::camera::Camera;
use crate::imageops::RectF;
use crate::scene::SceneSpec;
use crate::transition::Transition;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub type NodeId = u64;
pub type WorldId = String;

/// Set on a node N when a ZoomOut node Z was generated from it: N is then
/// also listed among Z's children and is drawn on Z as a ZoomIn outline at
/// `rect` (the centered 1/factor region). N's `parent` is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backlink {
    pub zoom_out_node: NodeId,
    pub rect: RectF,
}

/// One frame in a world. Image paths are relative to the world directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    /// Ordered. May include a back-link child created by ZoomOut (see DESIGN.md).
    #[serde(default)]
    pub children: Vec<NodeId>,
    pub transition: Transition,
    pub camera: Camera,
    /// None only for a Root node loaded from a photo before analysis
    /// completes, or when analysis failed.
    pub spec: Option<SceneSpec>,
    /// Relative path, e.g. "images/n0003.png".
    pub image: PathBuf,
    /// Bumped on regenerate so the UI reloads its texture.
    #[serde(default)]
    pub image_version: u32,
    pub title: String,
    /// RFC 3339.
    pub created_at: String,
    /// See [`Backlink`].
    #[serde(default)]
    pub backlink: Option<Backlink>,
}

/// The persistent form of a world (world.json).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub id: WorldId,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub root: NodeId,
    /// Where the user left off; restored on reopen.
    pub current: NodeId,
    pub next_id: NodeId,
    pub nodes: BTreeMap<NodeId, Node>,
    /// Original photo path, if the world started from a file.
    #[serde(default)]
    pub source_photo: Option<PathBuf>,
}

impl World {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Root-to-node chain of ids, root first.
    pub fn ancestry(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            chain.push(c);
            cur = self.nodes.get(&c).and_then(|n| n.parent);
        }
        chain.reverse();
        chain
    }

    /// Existing child of `from` reached by an approximately equal discrete
    /// transition, if any. Back-link children are matched on their
    /// synthesized ZoomIn rect, not their own transition.
    pub fn find_child(&self, from: NodeId, t: &Transition) -> Option<NodeId> {
        let parent = self.nodes.get(&from)?;
        parent
            .children
            .iter()
            .copied()
            .find(|cid| self.child_transition(from, *cid).map(|ct| ct.approx_eq(t)).unwrap_or(false))
    }

    /// The transition to show for `child` when listed under `from`: its own,
    /// or a synthesized ZoomIn when it is a back-link child.
    pub fn child_transition(&self, from: NodeId, child: NodeId) -> Option<Transition> {
        let n = self.nodes.get(&child)?;
        match &n.backlink {
            Some(b) if b.zoom_out_node == from => Some(Transition::ZoomIn { rect: b.rect }),
            _ => Some(n.transition.clone()),
        }
    }
}

/// Compact description of an ancestor for the describe prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AncestorBrief {
    pub title: String,
    pub transition: Transition,
    pub camera: Camera,
}

/// What the UI needs to draw an existing child on top of its parent frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildOutline {
    pub node: NodeId,
    pub transition: Transition,
    pub title: String,
}

/// Snapshot of the current view for the UI. Cheap to clone.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldView {
    pub world_id: WorldId,
    pub world_title: String,
    pub current: NodeId,
    /// Absolute path to the current frame's PNG.
    pub image_path: PathBuf,
    pub image_version: u32,
    pub camera: Camera,
    pub title: String,
    /// e.g. "geographic · about 40 km across · depth 3"
    pub breadcrumb: String,
    pub children: Vec<ChildOutline>,
    pub can_back: bool,
    pub can_redo: bool,
    pub depth: usize,
}

/// One row on the open screen's list of saved worlds.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldSummary {
    pub id: WorldId,
    pub title: String,
    /// Absolute path to thumb.png, if present.
    pub thumbnail: Option<PathBuf>,
    pub updated_at: String,
    pub node_count: usize,
}
