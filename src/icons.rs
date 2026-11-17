//! Vector icon set drawn with the egui painter (Lucide-style: 24-unit grid, rounded
//! strokes). Replaces emoji glyphs, which render inconsistently across fonts.

use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, Vec2,
};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Play,
    Pause,
    SkipNext,
    SkipPrev,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    VolumeLow,
    VolumeMute,
    Devices,
    Home,
    Search,
    Heart,
    HeartFilled,
    LogOut,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Back10,
    Fwd10,
    Expand,
    Close,
    Music,
}

/// Maps 24-unit icon coordinates into `rect`.
struct Grid {
    origin: Pos2,
    scale: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        Self {
            origin: rect.center() - Vec2::splat(side / 2.0),
            scale: side / 24.0,
        }
    }
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }
    fn pts(&self, v: &[(f32, f32)]) -> Vec<Pos2> {
        v.iter().map(|&(x, y)| self.p(x, y)).collect()
    }
    /// Arc points (angles in degrees, 0 = +x, clockwise on screen).
    fn arc(&self, cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Vec<Pos2> {
        let n = ((to - from).abs() / 8.0).ceil().max(2.0) as usize;
        (0..=n)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / n as f32).to_radians();
                self.p(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }
}

fn line(p: &Painter, pts: Vec<Pos2>, s: Stroke) {
    p.add(Shape::line(pts, s));
}

fn closed(p: &Painter, pts: Vec<Pos2>, s: Stroke) {
    p.add(Shape::closed_line(pts, s));
}

fn filled(p: &Painter, pts: Vec<Pos2>, c: Color32) {
    p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
}

fn speaker(p: &Painter, g: &Grid, s: Stroke) {
    closed(p, g.pts(&[(11.0, 5.0), (6.0, 9.0), (2.5, 9.0), (2.5, 15.0), (6.0, 15.0), (11.0, 19.0)]), s);
}

fn heart_points(g: &Grid) -> Vec<Pos2> {
    // Classic parametric heart, fitted into the grid.
    (0..48)
        .map(|i| {
            let t = i as f32 / 48.0 * TAU;
            let x = 16.0 * t.sin().powi(3);
            let y = 13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos();
            g.p(12.0 + x * 0.6, 11.5 - y * 0.6)
        })
        .collect()
}

pub fn paint(p: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let g = Grid::new(rect);
    let s = Stroke::new((g.scale * 1.9).max(1.3), color);
    match icon {
        Icon::Play => filled(p, g.pts(&[(7.0, 4.5), (19.5, 12.0), (7.0, 19.5)]), color),
        Icon::Pause => {
            for x in [6.0, 14.0] {
                let r = Rect::from_min_max(g.p(x, 4.5), g.p(x + 4.0, 19.5));
                p.rect_filled(r, g.scale * 1.2, color);
            }
        }
        Icon::SkipNext => {
            filled(p, g.pts(&[(5.0, 5.0), (16.0, 12.0), (5.0, 19.0)]), color);
            p.rect_filled(Rect::from_min_max(g.p(17.0, 5.0), g.p(19.5, 19.0)), g.scale, color);
        }
        Icon::SkipPrev => {
            filled(p, g.pts(&[(19.0, 5.0), (8.0, 12.0), (19.0, 19.0)]), color);
            p.rect_filled(Rect::from_min_max(g.p(4.5, 5.0), g.p(7.0, 19.0)), g.scale, color);
        }
        Icon::Shuffle => {
            line(p, g.pts(&[(2.5, 17.0), (7.0, 17.0), (15.0, 7.0), (20.5, 7.0)]), s);
            line(p, g.pts(&[(17.5, 4.0), (20.5, 7.0), (17.5, 10.0)]), s);
            line(p, g.pts(&[(2.5, 7.0), (7.0, 7.0), (9.5, 10.0)]), s);
            line(p, g.pts(&[(14.5, 14.0), (15.0, 17.0), (20.5, 17.0)]), s);
            line(p, g.pts(&[(17.5, 14.0), (20.5, 17.0), (17.5, 20.0)]), s);
        }
        Icon::Repeat | Icon::RepeatOne => {
            let mut top = g.arc(7.0, 10.0, 4.0, 180.0, 270.0);
            top.push(g.p(20.5, 6.0));
            line(p, top, s);
            line(p, g.pts(&[(17.5, 3.0), (20.5, 6.0), (17.5, 9.0)]), s);
            let mut bottom = g.arc(17.0, 14.0, 4.0, 0.0, 90.0);
            bottom.push(g.p(3.5, 18.0));
            line(p, bottom, s);
            line(p, g.pts(&[(6.5, 15.0), (3.5, 18.0), (6.5, 21.0)]), s);
            if icon == Icon::RepeatOne {
                p.text(g.p(12.0, 12.2), Align2::CENTER_CENTER, "1", FontId::proportional(g.scale * 8.5), color);
            }
        }
        Icon::Volume => {
            speaker(p, &g, s);
            line(p, g.arc(12.0, 12.0, 4.0, -50.0, 50.0), s);
            line(p, g.arc(12.0, 12.0, 8.0, -50.0, 50.0), s);
        }
        Icon::VolumeLow => {
            speaker(p, &g, s);
            line(p, g.arc(12.0, 12.0, 4.0, -50.0, 50.0), s);
        }
        Icon::VolumeMute => {
            speaker(p, &g, s);
            line(p, g.pts(&[(15.5, 9.5), (20.5, 14.5)]), s);
            line(p, g.pts(&[(20.5, 9.5), (15.5, 14.5)]), s);
        }
        Icon::Devices => {
            p.rect_stroke(Rect::from_min_max(g.p(2.5, 4.5), g.p(14.5, 14.5)), g.scale * 1.5, s, egui::StrokeKind::Middle);
            line(p, g.pts(&[(5.5, 18.5), (11.5, 18.5)]), s);
            p.rect_stroke(Rect::from_min_max(g.p(16.5, 4.5), g.p(21.5, 19.5)), g.scale * 1.5, s, egui::StrokeKind::Middle);
            p.circle_stroke(g.p(19.0, 14.5), g.scale * 1.6, s);
            p.circle_filled(g.p(19.0, 8.0), g.scale * 0.9, color);
        }
        Icon::Home => closed(
            p,
            g.pts(&[(3.0, 10.5), (12.0, 3.5), (21.0, 10.5), (21.0, 20.5), (15.0, 20.5), (15.0, 14.5), (9.0, 14.5), (9.0, 20.5), (3.0, 20.5)]),
            s,
        ),
        Icon::Search => {
            p.circle_stroke(g.p(10.5, 10.5), g.scale * 6.5, s);
            line(p, g.pts(&[(15.5, 15.5), (20.5, 20.5)]), s);
        }
        Icon::Heart => closed(p, heart_points(&g), s),
        Icon::HeartFilled => {
            let pts = heart_points(&g);
            // Not convex: fill as a fan of triangles from the centre.
            let c = g.p(12.0, 11.0);
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(c, color);
            for q in &pts {
                mesh.colored_vertex(*q, color);
            }
            let n = pts.len() as u32;
            for i in 0..n {
                mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
            }
            p.add(Shape::mesh(mesh));
        }
        Icon::LogOut => {
            line(p, g.pts(&[(9.0, 3.5), (5.0, 3.5), (3.5, 5.0), (3.5, 19.0), (5.0, 20.5), (9.0, 20.5)]), s);
            line(p, g.pts(&[(10.0, 12.0), (20.5, 12.0)]), s);
            line(p, g.pts(&[(16.0, 7.5), (20.5, 12.0), (16.0, 16.5)]), s);
        }
        Icon::ChevronLeft => line(p, g.pts(&[(15.0, 5.5), (8.5, 12.0), (15.0, 18.5)]), s),
        Icon::ChevronRight => line(p, g.pts(&[(9.0, 5.5), (15.5, 12.0), (9.0, 18.5)]), s),
        Icon::ChevronDown => line(p, g.pts(&[(5.5, 9.0), (12.0, 15.5), (18.5, 9.0)]), s),
        Icon::Back10 | Icon::Fwd10 => {
            // Circular arrow ending at the top: forward travels clockwise (arrow points
            // right), back counter-clockwise (arrow points left), like media players.
            let (from, to) = if icon == Icon::Fwd10 { (-30.0_f32, 270.0_f32) } else { (210.0, -90.0) };
            let arc = g.arc(12.0, 12.5, 8.0, from, to);
            let tip = *arc.last().unwrap();
            let a = to.to_radians();
            let dir = Vec2::new(-a.sin(), a.cos()) * (to - from).signum();
            let normal = Vec2::new(-dir.y, dir.x);
            let l = g.scale * 3.0;
            line(p, arc, s);
            line(p, vec![tip - dir * l + normal * l, tip, tip - dir * l - normal * l], s);
            p.text(g.p(12.0, 13.0), Align2::CENTER_CENTER, "10", FontId::proportional(g.scale * 7.0), color);
        }
        Icon::Expand => {
            line(p, g.pts(&[(14.5, 3.5), (20.5, 3.5), (20.5, 9.5)]), s);
            line(p, g.pts(&[(20.5, 3.5), (13.5, 10.5)]), s);
            line(p, g.pts(&[(9.5, 20.5), (3.5, 20.5), (3.5, 14.5)]), s);
            line(p, g.pts(&[(3.5, 20.5), (10.5, 13.5)]), s);
        }
        Icon::Close => {
            line(p, g.pts(&[(6.0, 6.0), (18.0, 18.0)]), s);
            line(p, g.pts(&[(18.0, 6.0), (6.0, 18.0)]), s);
        }
        Icon::Music => {
            p.circle_stroke(g.p(7.0, 18.0), g.scale * 3.0, s);
            p.circle_stroke(g.p(18.0, 16.0), g.scale * 3.0, s);
            line(p, g.pts(&[(10.0, 18.0), (10.0, 5.5), (21.0, 3.5), (21.0, 16.0)]), s);
        }
    }
}

/// Side of the square hit area of a `button` with an icon of `size`.
pub fn button_side(size: f32) -> f32 {
    size + (size * 0.45).round() * 2.0
}

/// Borderless icon button with a soft circular hover background.
pub fn button(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32, hover: Color32) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(button_side(size)), Sense::hover());
    button_at(ui, rect, icon, size, color, hover)
}

/// Like `button`, but at an explicit position (for rows laid out by hand).
pub fn button_at(ui: &mut egui::Ui, rect: Rect, icon: Icon, size: f32, color: Color32, hover: Color32) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(("icon_btn", rect.min.x as i32, rect.min.y as i32)), Sense::click());
    if resp.hovered() {
        ui.painter().circle_filled(rect.center(), rect.width() / 2.0, Color32::from_rgba_premultiplied(26, 22, 22, 28));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = if resp.hovered() { hover } else { color };
    paint(ui.painter(), icon, Rect::from_center_size(rect.center(), Vec2::splat(size)), c);
    resp
}

/// Spinner arc for "loading" states inside round buttons.
pub fn spinner(p: &Painter, center: Pos2, radius: f32, color: Color32, t: f64) {
    let start = (t * 4.0) as f32 % TAU;
    let n = 20;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = start + i as f32 / n as f32 * PI * 1.4;
            center + Vec2::new(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(2.5, color)));
}
