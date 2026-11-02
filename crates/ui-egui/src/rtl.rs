//! The right-to-left interface: the whole window mirrored in one place.
//!
//! Panels lay themselves out with absolute coordinates, so this module leaves their code alone and
//! mirrors at the two ends instead. Every pass still runs in the usual left-to-right coordinates
//! ("logical"); pointer input is reflected on the way in ([`mirror_input`]) and the finished paint
//! lists are reflected on the way out ([`Mirror`]), so the menu bar, docks, forms, dialogs and
//! scroll bars all change sides together and every hit test still matches what is drawn.
//!
//! Text is moved, never reflected: a galley keeps its glyphs and lands on the reflected box. Images
//! are moved the same way, so a thumbnail is not shown backwards.
//!
//! Some surfaces keep their direction in an Arabic interface: time runs left to right on a timeline,
//! and the composition is a picture. A panel registers such a surface with [`keep`]. The surface
//! trades places with the mirrored window like everything else, but its contents are only shifted,
//! not reflected. A zone belongs to the layer that registered it, so a dialog floating over the
//! timeline still mirrors.

use std::sync::Arc;

use egui::epaint::ClippedShape;
use egui::layers::ShapeIdx;
use egui::{Context, Event, Id, LayerId, Pos2, RawInput, Rect, Shape, Ui, Vec2, pos2, vec2};

/// The surfaces of one viewport that keep their direction.
#[derive(Clone, Default)]
struct Zones {
    /// Registered during the last finished pass: what the pointer is mapped against.
    finished: Vec<(LayerId, Rect)>,
    /// Registered during this pass: what this pass's shapes are mapped against.
    building: Vec<(LayerId, Rect)>,
    /// The mapping a held button started with (`Some(None)`: the plain reflection). A drag keeps it
    /// across zone edges, so a scrub that leaves the ruler does not jump.
    grabbed: Option<Option<Rect>>,
}

fn active_id() -> Id {
    Id::unique("rtl-active")
}

fn zones_id(ctx: &Context) -> Id {
    Id::unique("rtl-zones").with(ctx.viewport_id())
}

/// Turn mirroring on or off; called once per pass with the interface language's direction.
pub(crate) fn set_active(ctx: &Context, on: bool) {
    if on {
        ctx.add_plugin(Mirror);
    }
    ctx.data_mut(|d| d.insert_temp(active_id(), on));
}

/// Is the interface mirrored?
pub(crate) fn active(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(active_id())).unwrap_or(false)
}

/// Keep `rect` of the current layer in its own direction for this pass.
pub(crate) fn keep(ui: &Ui, rect: Rect) {
    let ctx = ui.ctx();
    if active(ctx) && rect.is_positive() {
        let (layer, id) = (ui.layer_id(), zones_id(ctx));
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Zones>(id).building.push((layer, rect)));
    }
}

/// How one point moves from the window as drawn to logical coordinates.
fn to_logical(p: Pos2, width: f32, zone: Option<Rect>) -> Pos2 {
    match zone {
        // The zone's shift, undone.
        Some(z) => pos2(p.x + z.min.x - (width - z.max.x), p.y),
        None => pos2(width - p.x, p.y),
    }
}

/// The zone under `drawn`, a point in the window as drawn. `drawn` reflected is inside the logical
/// zone exactly when it is inside the zone's mirrored place, so the reflection finds it, and the
/// topmost layer there says whether a dialog covers it.
fn zone_at(ctx: &Context, zones: &Zones, drawn: Pos2, width: f32) -> Option<Rect> {
    let reflected = to_logical(drawn, width, None);
    let top = ctx.layer_id_at(reflected);
    zones.finished.iter().find(|(layer, rect)| rect.contains(reflected) && top == Some(*layer)).map(|(_, rect)| *rect)
}

/// Reflect `raw`'s pointer events into logical coordinates. Runs before anything else touches them;
/// input the app synthesises afterwards is already logical (automation rectangles are).
pub(crate) fn mirror_input(ctx: &Context, raw: &mut RawInput) {
    if !active(ctx) {
        return;
    }
    let Some(width) = raw.screen_rect.map(|r| r.max.x).or_else(|| Some(ctx.content_rect().max.x)) else { return };
    let id = zones_id(ctx);
    let mut zones: Zones = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let hovered_zone = ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| zones.finished.iter().any(|(_, r)| r.contains(p)));
    for event in &mut raw.events {
        match event {
            Event::PointerMoved(pos) => {
                let zone = zones.grabbed.unwrap_or_else(|| zone_at(ctx, &zones, *pos, width));
                *pos = to_logical(*pos, width, zone);
            }
            Event::PointerButton { pos, pressed, .. } => {
                let zone = zones.grabbed.unwrap_or_else(|| zone_at(ctx, &zones, *pos, width));
                zones.grabbed = pressed.then_some(zone);
                *pos = to_logical(*pos, width, zone);
            }
            Event::Touch { pos, .. } => *pos = to_logical(*pos, width, zone_at(ctx, &zones, *pos, width)),
            Event::MouseMoved(delta) => delta.x = -delta.x,
            Event::MouseWheel { delta, .. } if !hovered_zone => delta.x = -delta.x,
            _ => {}
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, zones));
}

/// Reflect `rect` in the window's vertical centre line.
fn reflect_rect(rect: Rect, width: f32) -> Rect {
    Rect::from_min_max(pos2(width - rect.max.x, rect.min.y), pos2(width - rect.min.x, rect.max.y))
}

fn reflect_point(p: &mut Pos2, width: f32) {
    p.x = width - p.x;
}

/// The move that puts `rect` on its reflected place without reflecting what is in it.
fn slide(rect: Rect, width: f32) -> Vec2 {
    vec2(width - rect.max.x - rect.min.x, 0.0)
}

/// Reflect one shape; text and images slide instead (see the module docs).
fn reflect_shape(shape: &mut Shape, width: f32) {
    match shape {
        Shape::Noop => {}
        Shape::Vec(shapes) => shapes.iter_mut().for_each(|s| reflect_shape(s, width)),
        Shape::Circle(c) => reflect_point(&mut c.center, width),
        Shape::Ellipse(e) => reflect_point(&mut e.center, width),
        Shape::LineSegment { points, .. } => points.iter_mut().for_each(|p| reflect_point(p, width)),
        Shape::Path(path) => {
            path.points.iter_mut().for_each(|p| reflect_point(p, width));
            // Reflection flips the winding the tessellator's fill expects.
            path.points.reverse();
        }
        Shape::Rect(r) => {
            r.rect = reflect_rect(r.rect, width);
            std::mem::swap(&mut r.corner_radius.nw, &mut r.corner_radius.ne);
            std::mem::swap(&mut r.corner_radius.sw, &mut r.corner_radius.se);
        }
        Shape::QuadraticBezier(b) => b.points.iter_mut().for_each(|p| reflect_point(p, width)),
        Shape::CubicBezier(b) => b.points.iter_mut().for_each(|p| reflect_point(p, width)),
        Shape::Mesh(mesh) if mesh.texture_id == egui::TextureId::default() => {
            std::sync::Arc::make_mut(mesh).vertices.iter_mut().for_each(|v| reflect_point(&mut v.pos, width));
        }
        // Text, images, GPU callbacks, and the rare shape with no points to reflect.
        other => {
            let bounds = other.visual_bounding_rect();
            if bounds.is_finite() {
                other.translate(slide(bounds, width));
            }
        }
    }
}

/// Map one painted shape: a shift inside a zone, the reflection elsewhere.
fn mirror_shape(clipped: &mut ClippedShape, width: f32, zones: &[Rect]) {
    let visible = clipped.shape.visual_bounding_rect().intersect(clipped.clip_rect);
    if let Some(zone) = zones.iter().find(|z| visible.is_finite() && z.contains(visible.center())) {
        let shift = slide(*zone, width);
        clipped.shape.translate(shift);
        clipped.clip_rect = clipped.clip_rect.translate(shift);
    } else {
        reflect_shape(&mut clipped.shape, width);
        clipped.clip_rect = reflect_rect(clipped.clip_rect, width);
    }
}

/// The caret of the focused text field: selection and caret are drawn over the text by offset, so
/// that field is shifted whole.
fn focused_field(ctx: &Context) -> Option<(LayerId, Rect)> {
    if !ctx.egui_wants_keyboard_input() {
        return None;
    }
    let response = ctx.read_response(ctx.memory(|m| m.focused())?)?;
    Some((response.layer_id, response.rect))
}

/// Translation audit: with `EFFECTCRAFT_TEXT_AUDIT=<file>` set, append every drawn string that has
/// Latin words to that file, so what a real session leaves in English can be listed.
fn audit_text(list: &egui::layers::PaintList, path: &std::ffi::OsStr) {
    use std::io::Write as _;
    let drawn = list.all_entries().filter_map(|c| if let Shape::Text(text) = &c.shape { Some(text.galley.text()) } else { None });
    let lines: String =
        drawn.filter(|s| s.split(|c: char| !c.is_ascii_alphabetic()).any(|w| w.len() >= 2)).map(|s| format!("{}\n", s.replace('\n', "\\n"))).collect();
    if !lines.is_empty()
        && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = file.write_all(lines.as_bytes());
    }
}

/// Remember the names the user chose (layers, project items) for this pass: drawn text equal to one
/// is theirs, even if it spells a catalog entry.
pub(crate) fn set_user_names(ctx: &Context, names: std::collections::HashSet<String>) {
    ctx.data_mut(|d| d.insert_temp(Id::unique("rtl-user-names"), Arc::new(names)));
}

/// Translate finished text that reached the painter in English, for literals that no panel routes
/// through the catalog. Only a whole single-style string is replaced, so code and rich text stay as
/// they are, and the focused field is left alone.
fn translate_drawn(ctx: &Context, layers: &[LayerId], focused: Option<(LayerId, Rect)>) {
    let names: Arc<std::collections::HashSet<String>> = ctx.data(|d| d.get_temp(Id::unique("rtl-user-names"))).unwrap_or_default();
    let mut found = Vec::new();
    ctx.graphics(|graphics| {
        for layer in layers {
            let Some(list) = graphics.get(*layer) else { continue };
            for (index, clipped) in list.all_entries().enumerate() {
                let Shape::Text(text) = &clipped.shape else { continue };
                let job = &text.galley.job;
                let typed = focused.is_some_and(|(owner, rect)| owner == *layer && rect.contains(text.pos));
                if job.sections.len() != 1 || typed || names.contains(job.text.as_str()) {
                    continue;
                }
                if let Some(translated) = crate::i18n::drawn_text(&job.text) {
                    found.push((*layer, index, (**job).clone(), translated));
                }
            }
        }
    });
    for (layer, index, mut job, translated) in found {
        if let Some(section) = job.sections.first_mut() {
            section.byte_range = egui::text::ByteIndex(0)..egui::text::ByteIndex(translated.len());
        }
        job.text = translated;
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        ctx.graphics_mut(|graphics| {
            if let Some(list) = graphics.get_mut(layer) {
                list.mutate_shape(ShapeIdx(index), |clipped| {
                    if let Shape::Text(text) = &mut clipped.shape {
                        text.galley = galley;
                    }
                });
            }
        });
    }
}

/// Reflects the finished paint lists at the end of every pass, in every viewport.
struct Mirror;

impl egui::Plugin for Mirror {
    fn debug_name(&self) -> &'static str {
        "effectcraft-rtl"
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !active(&ctx) {
            return;
        }
        let width = ctx.content_rect().max.x;
        let id = zones_id(&ctx);
        let mut state: Zones = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        let focused = focused_field(&ctx);
        state.building.extend(focused);
        let layers: Vec<LayerId> = ctx.memory(|m| m.layer_ids().collect());
        translate_drawn(&ctx, &layers, focused);
        let audit = std::env::var_os("EFFECTCRAFT_TEXT_AUDIT");
        ctx.graphics_mut(|graphics| {
            for layer in layers {
                let Some(list) = graphics.get_mut(layer) else { continue };
                if let Some(path) = &audit {
                    audit_text(list, path);
                }
                let zones: Vec<Rect> = state.building.iter().filter(|(owner, _)| *owner == layer).map(|(_, rect)| *rect).collect();
                for index in 0..list.next_idx().0 {
                    list.mutate_shape(ShapeIdx(index), |clipped| mirror_shape(clipped, width, &zones));
                }
            }
        });
        state.finished = std::mem::take(&mut state.building);
        ctx.data_mut(|d| d.insert_temp(id, state));
    }

    /// Other viewports never reach the app's own input hook.
    fn input_hook(&mut self, ctx: &Context, input: &mut RawInput) {
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            mirror_input(ctx, input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Stroke, StrokeKind};

    const WIDTH: f32 = 1000.0;

    #[test]
    fn a_rectangle_lands_on_its_reflected_place_with_its_corners_swapped() {
        let mut shape = Shape::Rect(egui::epaint::RectShape::new(
            Rect::from_min_max(pos2(10.0, 5.0), pos2(110.0, 25.0)),
            egui::CornerRadius { nw: 8, ne: 0, sw: 0, se: 2 },
            Color32::RED,
            Stroke::NONE,
            StrokeKind::Inside,
        ));
        reflect_shape(&mut shape, WIDTH);
        let Shape::Rect(r) = shape else { panic!("still a rectangle") };
        assert_eq!(r.rect, Rect::from_min_max(pos2(890.0, 5.0), pos2(990.0, 25.0)));
        assert_eq!((r.corner_radius.ne, r.corner_radius.nw, r.corner_radius.sw, r.corner_radius.se), (8, 0, 2, 0));
    }

    fn press(x: f32, pressed: bool) -> Event {
        Event::PointerButton { pos: pos2(x, 50.0), button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    fn mapped(ctx: &Context, events: Vec<Event>) -> Vec<f32> {
        let mut raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, 600.0))), events, ..Default::default() };
        mirror_input(ctx, &mut raw);
        raw.events
            .iter()
            .filter_map(|e| match e {
                Event::PointerMoved(p) | Event::PointerButton { pos: p, .. } => Some(p.x),
                _ => None,
            })
            .collect()
    }

    /// A real click lands where it was drawn, and a drag that starts in a zone keeps the zone's map
    /// when it leaves, so a playhead scrub never jumps. (`layer_id_at` needs a finished pass, so the
    /// held mapping is set directly.)
    #[test]
    fn the_pointer_is_reflected_and_a_held_drag_keeps_its_map() {
        let ctx = Context::default();
        ctx.data_mut(|d| d.insert_temp(active_id(), true));
        assert_eq!(mapped(&ctx, vec![Event::PointerMoved(pos2(100.0, 50.0))]), [900.0]);
        let zone = Rect::from_min_max(pos2(600.0, 0.0), pos2(1000.0, 300.0));
        let id = zones_id(&ctx);
        ctx.data_mut(|d| d.insert_temp(id, Zones { grabbed: Some(Some(zone)), ..Default::default() }));
        assert_eq!(
            mapped(&ctx, vec![Event::PointerMoved(pos2(10.0, 50.0)), Event::PointerMoved(pos2(700.0, 50.0)), press(700.0, false)]),
            [610.0, 1300.0, 1300.0]
        );
        assert_eq!(mapped(&ctx, vec![Event::PointerMoved(pos2(700.0, 50.0))]), [300.0], "the release ends the held map");
    }

    #[test]
    fn nothing_moves_in_a_left_to_right_interface() {
        let ctx = Context::default();
        assert_eq!(mapped(&ctx, vec![press(100.0, true)]), [100.0]);
    }

    #[test]
    fn reflecting_twice_is_the_identity() {
        let rect = Rect::from_min_max(pos2(120.0, 0.0), pos2(300.0, 40.0));
        assert_eq!(reflect_rect(reflect_rect(rect, WIDTH), WIDTH), rect);
        assert_eq!(to_logical(pos2(WIDTH - 42.0, 7.0), WIDTH, None), pos2(42.0, 7.0));
    }

    /// A zone's contents move with it and keep their order: a playhead 10 px after the start of the
    /// timeline's time area is still 10 px after its start, not 10 px before its end.
    #[test]
    fn a_zone_shifts_its_contents_and_its_pointer_map_undoes_the_shift() {
        let zone = Rect::from_min_max(pos2(600.0, 0.0), pos2(1000.0, 300.0));
        let shape = Shape::line_segment([pos2(610.0, 0.0), pos2(610.0, 300.0)], Stroke::new(1.0, Color32::RED));
        let mut clipped = ClippedShape { clip_rect: zone, shape, transform_after_tessellation: Default::default() };
        mirror_shape(&mut clipped, WIDTH, &[zone]);
        let Shape::LineSegment { points, .. } = clipped.shape else { panic!("still a line") };
        assert_eq!(points[0].x, 10.0);
        assert_eq!(clipped.clip_rect, reflect_rect(zone, WIDTH));
        assert_eq!(to_logical(pos2(10.0, 50.0), WIDTH, Some(zone)), pos2(610.0, 50.0));
    }

    #[test]
    fn a_shape_outside_every_zone_is_reflected() {
        let zone = Rect::from_min_max(pos2(600.0, 0.0), pos2(1000.0, 300.0));
        let shape = Shape::line_segment([pos2(10.0, 0.0), pos2(20.0, 0.0)], Stroke::new(1.0, Color32::RED));
        let clip_rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(500.0, 300.0));
        let mut clipped = ClippedShape { clip_rect, shape, transform_after_tessellation: Default::default() };
        mirror_shape(&mut clipped, WIDTH, &[zone]);
        let Shape::LineSegment { points, .. } = clipped.shape else { panic!("still a line") };
        assert_eq!((points[0].x, points[1].x), (990.0, 980.0));
    }
}
