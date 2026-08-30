//! The scene graph: which sources are on the canvas, where, and in what order.
//!
//! The model is `obs_scene_item` — an identifier of its own, the source it
//! shows, visibility, locking, a transform, a blend mode and a scale filter.
//! What changes is the container.
//!
//! # A vector, not a linked list
//!
//! libobs threads its items on an intrusive doubly-linked list, and carries the
//! comment *"would do `**prev_next`, but not really great for reordering"* — the
//! list bothers them and they keep it anyway. The compositor walks this list
//! once per frame, and chasing pointers around the heap is exactly what
//! CLAUDE.md §4.5 rules out. A `Vec` is contiguous, turns reordering into a
//! swap, and a scene holds tens of items rather than millions, so there is no
//! insertion cost worth trading for.

use crate::frame::FrameSize;
use crate::math::Transform;
use crate::source::SourceId;

/// How an item's pixels combine with what is already on the canvas.
///
/// The seven modes libobs offers (`obs_blending_type`). Implementing them is
/// the compositor's job; the scene only records the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum BlendMode {
    /// Source over destination, honouring alpha. The default.
    #[default]
    Normal,
    /// Adds to what is underneath. Light, fire, glows.
    Additive,
    /// Subtracts from what is underneath.
    Subtract,
    /// Inverse multiply: never darkens.
    Screen,
    /// Multiplies: never lightens.
    Multiply,
    /// Keeps the lighter of the two.
    Lighten,
    /// Keeps the darker of the two.
    Darken,
}

impl BlendMode {
    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Additive => "additive",
            BlendMode::Subtract => "subtract",
            BlendMode::Screen => "screen",
            BlendMode::Multiply => "multiply",
            BlendMode::Lighten => "lighten",
            BlendMode::Darken => "darken",
        }
    }
}

/// How a source is resampled when its size does not match its box.
///
/// The six filters libobs offers (`obs_scale_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ScaleFilter {
    /// No resampling; the source is drawn at its own size.
    Disable,
    /// Nearest neighbour. Sharp, aliased. Right for pixel art.
    Point,
    /// Bilinear. The default: cheap and good enough for most content.
    #[default]
    Bilinear,
    /// Bicubic. Smoother on upscales, more expensive.
    Bicubic,
    /// Lanczos. Sharpest, most expensive.
    Lanczos,
    /// Area average. The best choice for large downscales.
    Area,
}

impl ScaleFilter {
    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            ScaleFilter::Disable => "disable",
            ScaleFilter::Point => "point",
            ScaleFilter::Bilinear => "bilinear",
            ScaleFilter::Bicubic => "bicubic",
            ScaleFilter::Lanczos => "lanczos",
            ScaleFilter::Area => "area",
        }
    }
}

/// Identifies an item within its scene.
///
/// Separate from the item's position in the drawing order, so the interface can
/// hold a selection while the user drags layers around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneItemId(u64);

impl SceneItemId {
    /// The underlying value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SceneItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "item#{}", self.0)
    }
}

/// One source placed on a scene.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneItem {
    id: SceneItemId,
    source: SourceId,
    /// Where and how the source is drawn.
    pub transform: Transform,
    /// Whether the item is drawn at all.
    pub visible: bool,
    /// Whether the interface should refuse to move it.
    ///
    /// A flag, not an enforced rule: honouring it is the interface's job, and
    /// making the type enforce it would make every other caller awkward.
    pub locked: bool,
    /// How the item combines with what is underneath.
    pub blend: BlendMode,
    /// How the item is resampled.
    pub scale_filter: ScaleFilter,
}

impl SceneItem {
    /// The item's identifier, stable across reordering.
    pub const fn id(&self) -> SceneItemId {
        self.id
    }

    /// The source this item shows.
    pub const fn source(&self) -> SourceId {
        self.source
    }
}

/// An ordered collection of sources on a canvas.
///
/// # Examples
///
/// ```
/// use voltra_core::{FrameSize, Scene, SourceId};
///
/// let mut scene = Scene::new("Live", FrameSize::new(1920, 1080)?);
/// let background = scene.add(SourceId::from_raw(1));
/// let overlay = scene.add(SourceId::from_raw(2));
///
/// // Items come back in drawing order, so the last added is on top.
/// let order: Vec<_> = scene.items().map(|item| item.id()).collect();
/// assert_eq!(order, vec![background, overlay]);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Scene {
    name: String,
    size: FrameSize,
    items: Vec<SceneItem>,
    next_item_id: u64,
}

impl Scene {
    /// An empty scene of the given canvas size.
    pub fn new(name: impl Into<String>, size: FrameSize) -> Self {
        Self {
            name: name.into(),
            size,
            items: Vec::new(),
            next_item_id: 1,
        }
    }

    /// The scene's name, as shown to the user.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Rename the scene.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// The canvas size this scene composes onto.
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// Resize the canvas. Item transforms are left alone.
    pub const fn set_size(&mut self, size: FrameSize) {
        self.size = size;
    }

    /// How many items the scene holds.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the scene holds no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Put `source` on top of the scene with a default transform.
    ///
    /// Identifiers are never reused, so an id held by the interface can only
    /// ever refer to the item it was taken from.
    pub fn add(&mut self, source: SourceId) -> SceneItemId {
        let id = SceneItemId(self.next_item_id);
        self.next_item_id += 1;
        self.items.push(SceneItem {
            id,
            source,
            transform: Transform::default(),
            visible: true,
            locked: false,
            blend: BlendMode::default(),
            scale_filter: ScaleFilter::default(),
        });
        id
    }

    /// Remove an item, returning it.
    ///
    /// `None` when no item carries that identifier.
    pub fn remove(&mut self, id: SceneItemId) -> Option<SceneItem> {
        let index = self.index_of(id)?;
        Some(self.items.remove(index))
    }

    /// Look an item up.
    pub fn item(&self, id: SceneItemId) -> Option<&SceneItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Look an item up for modification.
    pub fn item_mut(&mut self, id: SceneItemId) -> Option<&mut SceneItem> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    /// Every item, **back to front** — the order they are drawn in.
    ///
    /// The convention is declared rather than implied: the first item returned
    /// is the furthest back, the last one is on top. An interface listing layers
    /// top-first wants this reversed.
    pub fn items(&self) -> impl DoubleEndedIterator<Item = &SceneItem> {
        self.items.iter()
    }

    /// Every item, for modification, in drawing order.
    pub fn items_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut SceneItem> {
        self.items.iter_mut()
    }

    /// The visible items, in drawing order.
    ///
    /// The compositor's entry point: filtering once while walking is cheaper
    /// than asking per item inside the drawing loop.
    pub fn visible_items(&self) -> impl DoubleEndedIterator<Item = &SceneItem> {
        self.items.iter().filter(|item| item.visible)
    }

    /// Whether any item shows `source`.
    ///
    /// The building block for the cycle check a scene collection will need: a
    /// scene that ends up containing itself would make the compositor recurse
    /// forever.
    pub fn references(&self, source: SourceId) -> bool {
        self.items.iter().any(|item| item.source == source)
    }

    /// Move an item one place towards the front.
    ///
    /// A no-op when it is already on top, rather than wrapping or failing:
    /// pressing "raise" on the top layer should do nothing, not surprise anyone.
    pub fn raise(&mut self, id: SceneItemId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        if index + 1 >= self.items.len() {
            return false;
        }
        self.items.swap(index, index + 1);
        true
    }

    /// Move an item one place towards the back.
    pub fn lower(&mut self, id: SceneItemId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        if index == 0 {
            return false;
        }
        self.items.swap(index, index - 1);
        true
    }

    /// Move an item to the front, keeping everything else in order.
    pub fn raise_to_top(&mut self, id: SceneItemId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let item = self.items.remove(index);
        self.items.push(item);
        true
    }

    /// Move an item to the back, keeping everything else in order.
    pub fn lower_to_bottom(&mut self, id: SceneItemId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let item = self.items.remove(index);
        self.items.insert(0, item);
        true
    }

    /// Position of an item in the drawing order.
    fn index_of(&self, id: SceneItemId) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::{BlendMode, ScaleFilter, Scene, SceneItemId};
    use crate::frame::FrameSize;
    use crate::math::Vec2;
    use crate::source::SourceId;

    fn scene() -> Scene {
        Scene::new("Live", FrameSize::new(1920, 1080).expect("valid size"))
    }

    fn order(scene: &Scene) -> Vec<u64> {
        scene.items().map(|item| item.id().get()).collect()
    }

    #[test]
    fn items_come_back_in_drawing_order() {
        let mut scene = scene();
        let back = scene.add(SourceId::from_raw(1));
        let middle = scene.add(SourceId::from_raw(2));
        let front = scene.add(SourceId::from_raw(3));

        assert_eq!(
            order(&scene),
            vec![back.get(), middle.get(), front.get()],
            "the last item added should be drawn last, on top"
        );
        assert_eq!(scene.len(), 3);
        assert!(!scene.is_empty());
    }

    #[test]
    fn identifiers_are_unique_and_never_reused() {
        let mut scene = scene();
        let first = scene.add(SourceId::from_raw(1));
        let second = scene.add(SourceId::from_raw(1));
        assert_ne!(first, second);

        scene.remove(second).expect("item exists");
        let third = scene.add(SourceId::from_raw(1));
        assert_ne!(
            third, second,
            "a removed identifier must never come back around"
        );
    }

    /// What lets the interface keep a selection while layers are dragged.
    #[test]
    fn identifiers_survive_reordering() {
        let mut scene = scene();
        let bottom = scene.add(SourceId::from_raw(1));
        let top = scene.add(SourceId::from_raw(2));

        scene.raise(bottom);
        assert_eq!(order(&scene), vec![top.get(), bottom.get()]);
        assert_eq!(scene.item(bottom).expect("item").id(), bottom);
        assert_eq!(scene.item(bottom).expect("item").source().get(), 1);
    }

    #[test]
    fn raising_and_lowering_stop_at_the_edges() {
        let mut scene = scene();
        let bottom = scene.add(SourceId::from_raw(1));
        let top = scene.add(SourceId::from_raw(2));

        assert!(!scene.raise(top), "already on top");
        assert!(!scene.lower(bottom), "already at the bottom");
        assert_eq!(order(&scene), vec![bottom.get(), top.get()]);

        assert!(scene.raise(bottom));
        assert_eq!(order(&scene), vec![top.get(), bottom.get()]);
        assert!(scene.lower(bottom));
        assert_eq!(order(&scene), vec![bottom.get(), top.get()]);
    }

    #[test]
    fn moving_to_an_end_keeps_the_others_in_order() {
        let mut scene = scene();
        let first = scene.add(SourceId::from_raw(1));
        let second = scene.add(SourceId::from_raw(2));
        let third = scene.add(SourceId::from_raw(3));
        let fourth = scene.add(SourceId::from_raw(4));

        assert!(scene.raise_to_top(second));
        assert_eq!(
            order(&scene),
            vec![first.get(), third.get(), fourth.get(), second.get()]
        );

        assert!(scene.lower_to_bottom(fourth));
        assert_eq!(
            order(&scene),
            vec![fourth.get(), first.get(), third.get(), second.get()]
        );
    }

    #[test]
    fn hidden_items_are_skipped_but_the_order_holds() {
        let mut scene = scene();
        let back = scene.add(SourceId::from_raw(1));
        let middle = scene.add(SourceId::from_raw(2));
        let front = scene.add(SourceId::from_raw(3));

        scene.item_mut(middle).expect("item").visible = false;

        let visible: Vec<u64> = scene.visible_items().map(|item| item.id().get()).collect();
        assert_eq!(visible, vec![back.get(), front.get()]);
        // The item is still there, just not drawn.
        assert_eq!(scene.len(), 3);
    }

    #[test]
    fn operations_on_unknown_items_report_failure_quietly() {
        let mut scene = scene();
        let ghost = SceneItemId(999);
        assert!(scene.remove(ghost).is_none());
        assert!(scene.item(ghost).is_none());
        assert!(!scene.raise(ghost));
        assert!(!scene.lower(ghost));
        assert!(!scene.raise_to_top(ghost));
        assert!(!scene.lower_to_bottom(ghost));
    }

    #[test]
    fn a_source_used_twice_is_found_once() {
        let mut scene = scene();
        scene.add(SourceId::from_raw(7));
        scene.add(SourceId::from_raw(7));
        scene.add(SourceId::from_raw(8));

        assert!(scene.references(SourceId::from_raw(7)));
        assert!(scene.references(SourceId::from_raw(8)));
        assert!(!scene.references(SourceId::from_raw(9)));
    }

    #[test]
    fn new_items_carry_sensible_defaults() {
        let mut scene = scene();
        let id = scene.add(SourceId::from_raw(1));
        let item = scene.item(id).expect("item");

        assert!(item.visible);
        assert!(!item.locked);
        assert_eq!(item.blend, BlendMode::Normal);
        assert_eq!(item.scale_filter, ScaleFilter::Bilinear);
        assert_eq!(item.transform.pos, Vec2::ZERO);
        assert_eq!(item.transform.scale, Vec2::ONE);
    }

    #[test]
    fn transforms_are_edited_through_the_item() {
        let mut scene = scene();
        let id = scene.add(SourceId::from_raw(1));
        scene.item_mut(id).expect("item").transform.pos = Vec2::new(100.0, 50.0);
        scene.item_mut(id).expect("item").blend = BlendMode::Screen;

        let item = scene.item(id).expect("item");
        assert_eq!(item.transform.pos, Vec2::new(100.0, 50.0));
        assert_eq!(item.blend.name(), "screen");
    }

    #[test]
    fn a_scene_carries_its_name_and_canvas() {
        let mut scene = scene();
        assert_eq!(scene.name(), "Live");
        assert_eq!(scene.size().width(), 1920);

        scene.set_name("Starting soon");
        scene.set_size(FrameSize::new(1280, 720).expect("valid size"));
        assert_eq!(scene.name(), "Starting soon");
        assert_eq!(scene.size().height(), 720);
        assert_eq!(ScaleFilter::Area.name(), "area");
    }
}
