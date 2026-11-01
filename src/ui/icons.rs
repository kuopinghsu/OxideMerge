//! Monoline vector icons, drawn with the painter so they look the same on every platform and
//! follow the text color of the widget they are in.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};
use std::f32::consts::PI;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    First,
    Prev,
    Next,
    /// Disclosure of a collapsed tree row.
    ChevronRight,
    Last,
    PrevConflict,
    NextConflict,
    CopyRight,
    CopyLeft,
    CopyAllRight,
    CopyAllLeft,
    Undo,
    Redo,
    Search,
    Close,
    Refresh,
    Stop,
    ExpandAll,
    CollapseAll,
    ZoomIn,
    ZoomOut,
    Fit,
    Help,
    Settings,
    Sun,
    Moon,
    Folder,
    File,
    Plus,
    Lock,
    Unlock,
    Swap,
    Bolt,
    Table,
    Globe,
    Text,
    Binary,
    Image,
    Warning,
    Drop,
    Clock,
    Compare,
    Filter,
    Method,
    Check,
    Fold,
    Wrap,
    Palette,
    Export,
    Sidebar,
    Inline,
}

#[cfg(test)]
impl Icon {
    pub const ALL: [Icon; 51] = [
        Icon::First,
        Icon::Prev,
        Icon::Next,
        Icon::ChevronRight,
        Icon::Last,
        Icon::PrevConflict,
        Icon::NextConflict,
        Icon::CopyRight,
        Icon::CopyLeft,
        Icon::CopyAllRight,
        Icon::CopyAllLeft,
        Icon::Undo,
        Icon::Redo,
        Icon::Search,
        Icon::Close,
        Icon::Refresh,
        Icon::Stop,
        Icon::ExpandAll,
        Icon::CollapseAll,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::Fit,
        Icon::Help,
        Icon::Settings,
        Icon::Sun,
        Icon::Moon,
        Icon::Folder,
        Icon::File,
        Icon::Plus,
        Icon::Lock,
        Icon::Unlock,
        Icon::Swap,
        Icon::Bolt,
        Icon::Table,
        Icon::Globe,
        Icon::Text,
        Icon::Binary,
        Icon::Image,
        Icon::Warning,
        Icon::Drop,
        Icon::Clock,
        Icon::Compare,
        Icon::Filter,
        Icon::Method,
        Icon::Check,
        Icon::Fold,
        Icon::Wrap,
        Icon::Palette,
        Icon::Export,
        Icon::Sidebar,
        Icon::Inline,
    ];
}

/// Maps design-grid coordinates into `rect`: 16 units across, or 24 for a pen from `grid24`.
#[derive(Clone, Copy)]
struct Pen<'a> {
    painter: &'a Painter,
    origin: Pos2,
    scale: f32,
    stroke: Stroke,
    color: Color32,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + egui::vec2(x, y) * self.scale
    }

    fn line(&self, pts: &[(f32, f32)]) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::line(pts, self.stroke));
    }

    fn closed(&self, pts: &[(f32, f32)]) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter.add(Shape::closed_line(pts, self.stroke));
    }

    fn circle(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_stroke(self.p(x, y), r * self.scale, self.stroke);
    }

    fn dot(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_filled(self.p(x, y), r * self.scale, self.color);
    }

    /// Arc from `a0` to `a1` degrees; angles grow clockwise on screen, 0° points right.
    fn arc(&self, x: f32, y: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
        let steps = ((a1 - a0).abs() / 12.0).ceil().max(2.0) as usize;
        (0..=steps)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / steps as f32) * PI / 180.0;
                (x + r * a.cos(), y + r * a.sin())
            })
            .collect()
    }

    /// The same pen on a 24×24 grid, with the same line width.
    fn grid24(&self) -> Self {
        Pen {
            scale: self.scale * 16.0 / 24.0,
            ..*self
        }
    }

    /// Outline of a rectangle with corners rounded by `r`.
    fn rounded(&self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let mut pts = self.arc(x + r, y + r, r, 180.0, 270.0);
        pts.extend(self.arc(x + w - r, y + r, r, 270.0, 360.0));
        pts.extend(self.arc(x + w - r, y + h - r, r, 0.0, 90.0));
        pts.extend(self.arc(x + r, y + h - r, r, 90.0, 180.0));
        self.closed(&pts);
    }

    /// Cubic Bézier curve from `a` to `d` with control points `b` and `c`.
    fn cubic(&self, a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> Vec<(f32, f32)> {
        (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0;
                let u = 1.0 - t;
                let mix = |p: f32, q: f32, r: f32, s: f32| {
                    u * u * u * p + 3.0 * u * u * t * q + 3.0 * u * t * t * r + t * t * t * s
                };
                (mix(a.0, b.0, c.0, d.0), mix(a.1, b.1, c.1, d.1))
            })
            .collect()
    }
}

/// Paints `icon` centered in `rect` with the given color.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height());
    let scale = side / 16.0;
    let pen = Pen {
        painter,
        origin: rect.center() - egui::vec2(side, side) / 2.0,
        scale,
        stroke: Stroke::new((1.2 * scale).max(1.2), color),
        color,
    };
    let g = pen.grid24();
    match icon {
        Icon::First => {
            pen.line(&[(4.0, 4.0), (12.0, 4.0)]);
            pen.line(&[(4.0, 11.5), (8.0, 7.5), (12.0, 11.5)]);
        }
        Icon::Prev => g.line(&[(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)]),
        Icon::Next => g.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Icon::ChevronRight => pen.line(&[(6.0, 4.0), (10.0, 8.0), (6.0, 12.0)]),
        Icon::Last => {
            pen.line(&[(4.0, 4.5), (8.0, 8.5), (12.0, 4.5)]);
            pen.line(&[(4.0, 12.0), (12.0, 12.0)]);
        }
        Icon::PrevConflict => {
            pen.line(&[(2.5, 12.0), (6.5, 8.0), (10.5, 12.0)]);
            pen.line(&[(13.0, 3.5), (13.0, 7.5)]);
            pen.dot(13.0, 10.0, 1.0);
        }
        Icon::NextConflict => {
            pen.line(&[(2.5, 4.0), (6.5, 8.0), (10.5, 4.0)]);
            pen.line(&[(13.0, 5.5), (13.0, 9.5)]);
            pen.dot(13.0, 12.0, 1.0);
        }
        Icon::CopyRight => {
            g.line(&[(12.0, 5.0), (19.0, 12.0), (12.0, 19.0)]);
            g.line(&[(19.0, 12.0), (5.0, 12.0)]);
        }
        Icon::CopyLeft => {
            g.line(&[(12.0, 5.0), (5.0, 12.0), (12.0, 19.0)]);
            g.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::CopyAllRight => {
            pen.line(&[(3.0, 4.0), (7.0, 8.0), (3.0, 12.0)]);
            pen.line(&[(9.0, 4.0), (13.0, 8.0), (9.0, 12.0)]);
        }
        Icon::CopyAllLeft => {
            pen.line(&[(13.0, 4.0), (9.0, 8.0), (13.0, 12.0)]);
            pen.line(&[(7.0, 4.0), (3.0, 8.0), (7.0, 12.0)]);
        }
        Icon::Undo | Icon::Redo => {
            let mirror = icon == Icon::Redo;
            let mut path = vec![(3.0, 6.0), (10.0, 6.0)];
            path.extend(pen.arc(10.0, 9.5, 3.5, -90.0, 90.0));
            path.push((6.0, 13.0));
            let head = [(6.0, 3.0), (3.0, 6.0), (6.0, 9.0)];
            let flip = |pts: &[(f32, f32)]| -> Vec<(f32, f32)> {
                pts.iter()
                    .map(|&(x, y)| if mirror { (16.0 - x, y) } else { (x, y) })
                    .collect()
            };
            pen.line(&flip(&path));
            pen.line(&flip(&head));
        }
        Icon::Search => {
            g.circle(10.7, 10.7, 6.7);
            g.line(&[(16.0, 16.0), (21.0, 21.0)]);
        }
        Icon::Close => {
            pen.line(&[(4.0, 4.0), (12.0, 12.0)]);
            pen.line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
        Icon::Refresh => {
            pen.line(&pen.arc(8.0, 8.0, 5.5, -30.0, 270.0));
            pen.line(&[(5.6, 0.6), (8.0, 2.5), (5.6, 4.6)]);
        }
        Icon::Stop => {
            let r = Rect::from_min_max(pen.p(4.0, 4.0), pen.p(12.0, 12.0));
            painter.rect_filled(r, 1.5 * scale, color);
        }
        Icon::ExpandAll | Icon::CollapseAll => {
            pen.closed(&[(2.5, 2.5), (13.5, 2.5), (13.5, 13.5), (2.5, 13.5)]);
            pen.line(&[(5.0, 8.0), (11.0, 8.0)]);
            if icon == Icon::ExpandAll {
                pen.line(&[(8.0, 5.0), (8.0, 11.0)]);
            }
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            pen.circle(7.0, 7.0, 4.8);
            pen.line(&[(10.6, 10.6), (14.0, 14.0)]);
            pen.line(&[(4.8, 7.0), (9.2, 7.0)]);
            if icon == Icon::ZoomIn {
                pen.line(&[(7.0, 4.8), (7.0, 9.2)]);
            }
        }
        Icon::Fit => {
            pen.line(&[(2.0, 6.0), (2.0, 2.0), (6.0, 2.0)]);
            pen.line(&[(10.0, 2.0), (14.0, 2.0), (14.0, 6.0)]);
            pen.line(&[(14.0, 10.0), (14.0, 14.0), (10.0, 14.0)]);
            pen.line(&[(6.0, 14.0), (2.0, 14.0), (2.0, 10.0)]);
        }
        Icon::Help => {
            pen.circle(8.0, 8.0, 6.5);
            let mut hook = pen.arc(8.0, 6.4, 2.1, 190.0, 405.0);
            hook.extend([(8.0, 8.6), (8.0, 9.4)]);
            pen.line(&hook);
            pen.dot(8.0, 11.5, 0.95);
        }
        Icon::Settings => {
            g.line(&[(4.0, 7.0), (20.0, 7.0)]);
            g.line(&[(4.0, 17.0), (20.0, 17.0)]);
            g.circle(9.0, 7.0, 2.3);
            g.circle(16.0, 17.0, 2.3);
        }
        Icon::Sun => {
            g.circle(12.0, 12.0, 4.0);
            for ray in [
                [(12.0, 2.0), (12.0, 4.0)],
                [(12.0, 20.0), (12.0, 22.0)],
                [(2.0, 12.0), (4.0, 12.0)],
                [(20.0, 12.0), (22.0, 12.0)],
                [(4.9, 4.9), (6.4, 6.4)],
                [(17.6, 17.6), (19.1, 19.1)],
                [(19.1, 4.9), (17.6, 6.4)],
                [(6.4, 17.6), (4.9, 19.1)],
            ] {
                g.line(&ray);
            }
        }
        Icon::Moon => {
            // Two arcs of radius 9 meeting at (20.5, 15.8) and (8.2, 3.5), drawn as two open
            // strokes, as a closed outline would grow miter spikes at the sharp tips.
            g.line(&g.arc(15.986, 8.014, 9.0, 59.9, 210.1));
            g.line(&g.arc(12.714, 11.286, 9.0, -120.1, -329.9));
        }
        Icon::Folder => {
            let mut pts = g.arc(5.0, 7.0, 2.0, 180.0, 270.0);
            pts.extend([(10.0, 5.0), (12.0, 7.0)]);
            pts.extend(g.arc(19.0, 9.0, 2.0, 270.0, 360.0));
            pts.extend([(21.0, 19.0), (3.0, 19.0)]);
            g.closed(&pts);
        }
        Icon::File => {
            g.closed(&[
                (6.0, 3.0),
                (14.0, 3.0),
                (19.0, 8.0),
                (19.0, 21.0),
                (6.0, 21.0),
            ]);
            g.line(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)]);
            g.line(&[(9.0, 13.0), (16.0, 13.0)]);
            g.line(&[(9.0, 17.0), (16.0, 17.0)]);
        }
        Icon::Plus => {
            g.line(&[(12.0, 4.0), (12.0, 20.0)]);
            g.line(&[(4.0, 12.0), (20.0, 12.0)]);
        }
        Icon::Lock | Icon::Unlock => {
            pen.closed(&[(3.5, 7.5), (12.5, 7.5), (12.5, 14.0), (3.5, 14.0)]);
            let mut shackle = vec![(10.8, 7.5)];
            if icon == Icon::Lock {
                shackle.extend(pen.arc(8.0, 5.0, 2.8, 0.0, -180.0));
                shackle.push((5.2, 7.5));
            } else {
                shackle.extend(pen.arc(8.0, 5.0, 2.8, 0.0, -150.0));
            }
            pen.line(&shackle);
        }
        Icon::Swap => {
            pen.line(&[(2.5, 5.0), (13.0, 5.0)]);
            pen.line(&[(10.5, 2.5), (13.0, 5.0), (10.5, 7.5)]);
            pen.line(&[(13.5, 11.0), (3.0, 11.0)]);
            pen.line(&[(5.5, 8.5), (3.0, 11.0), (5.5, 13.5)]);
        }
        Icon::Bolt => pen.closed(&[
            (9.0, 1.5),
            (3.5, 9.0),
            (7.5, 9.0),
            (6.5, 14.5),
            (12.5, 6.5),
            (8.5, 6.5),
        ]),
        Icon::Table => {
            pen.closed(&[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0)]);
            pen.line(&[(2.0, 6.5), (14.0, 6.5)]);
            pen.line(&[(2.0, 9.8), (14.0, 9.8)]);
            pen.line(&[(7.0, 3.0), (7.0, 13.0)]);
        }
        Icon::Globe => {
            g.circle(12.0, 12.0, 9.0);
            g.line(&[(3.0, 12.0), (21.0, 12.0)]);
            let mut meridian = g.cubic((12.0, 3.0), (17.0, 9.0), (17.0, 15.0), (12.0, 21.0));
            meridian.extend(g.cubic((12.0, 21.0), (7.0, 15.0), (7.0, 9.0), (12.0, 3.0)));
            g.closed(&meridian);
        }
        Icon::Text => {
            pen.line(&[(3.0, 4.0), (13.0, 4.0)]);
            pen.line(&[(3.0, 7.0), (13.0, 7.0)]);
            pen.line(&[(3.0, 10.0), (13.0, 10.0)]);
            pen.line(&[(3.0, 13.0), (9.0, 13.0)]);
        }
        Icon::Binary => {
            g.rounded(3.0, 3.0, 18.0, 18.0, 3.0);
            g.line(&[(6.0, 10.0), (8.0, 8.0), (8.0, 16.0)]);
            g.line(&[(6.0, 16.0), (10.0, 16.0)]);
            let mut zero = g.cubic((15.0, 8.0), (18.0, 7.0), (19.0, 15.0), (15.0, 16.0));
            zero.extend(g.cubic((15.0, 16.0), (11.0, 17.0), (11.0, 9.0), (15.0, 8.0)));
            g.closed(&zero);
        }
        Icon::Image => {
            g.rounded(3.0, 3.0, 18.0, 18.0, 3.0);
            g.circle(8.5, 8.5, 1.5);
            g.line(&[
                (4.0, 18.0),
                (9.0, 13.0),
                (12.0, 16.0),
                (15.0, 12.0),
                (20.0, 18.0),
            ]);
        }
        Icon::Warning => {
            pen.closed(&[(8.0, 2.0), (14.5, 13.5), (1.5, 13.5)]);
            pen.line(&[(8.0, 6.5), (8.0, 9.5)]);
            pen.dot(8.0, 11.6, 0.9);
        }
        Icon::Export | Icon::Drop => {
            g.line(&[(12.0, 3.0), (12.0, 15.0)]);
            g.line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            g.line(&[(4.0, 17.0), (4.0, 21.0), (20.0, 21.0), (20.0, 17.0)]);
        }
        Icon::Clock => {
            g.line(&g.arc(12.0, 11.973, 9.0, 179.83, 494.43));
            g.line(&[(3.0, 17.0), (3.0, 12.0), (8.0, 12.0)]);
            g.line(&[(12.0, 7.0), (12.0, 12.0), (15.0, 14.0)]);
        }
        Icon::Compare => {
            g.rounded(3.0, 4.0, 7.0, 16.0, 2.0);
            g.rounded(14.0, 4.0, 7.0, 16.0, 2.0);
            for y in [9.0, 13.0] {
                g.line(&[(6.0, y), (8.0, y)]);
                g.line(&[(17.0, y), (19.0, y)]);
            }
        }
        Icon::Filter => g.closed(&[
            (3.0, 5.0),
            (21.0, 5.0),
            (14.0, 13.0),
            (14.0, 19.0),
            (10.0, 21.0),
            (10.0, 13.0),
        ]),
        Icon::Method => {
            pen.line(&[(2.5, 4.5), (13.5, 4.5)]);
            pen.line(&[(2.5, 11.5), (13.5, 11.5)]);
            pen.circle(6.0, 4.5, 1.8);
            pen.circle(10.5, 11.5, 1.8);
        }
        Icon::Check => g.line(&[(4.0, 12.0), (9.0, 17.0), (20.0, 6.0)]),
        Icon::Fold => {
            pen.line(&[(4.5, 2.5), (8.0, 6.0), (11.5, 2.5)]);
            pen.line(&[(4.5, 13.5), (8.0, 10.0), (11.5, 13.5)]);
            pen.line(&[(2.5, 8.0), (5.5, 8.0)]);
            pen.line(&[(10.5, 8.0), (13.5, 8.0)]);
        }
        Icon::Wrap => {
            pen.line(&[(2.5, 3.5), (13.5, 3.5)]);
            let mut path = vec![(2.5, 8.0), (11.0, 8.0)];
            path.extend(pen.arc(11.0, 10.25, 2.25, -90.0, 90.0));
            path.push((8.0, 12.5));
            pen.line(&path);
            pen.line(&[(9.5, 11.0), (8.0, 12.5), (9.5, 14.0)]);
            pen.line(&[(2.5, 12.5), (5.0, 12.5)]);
        }
        Icon::Palette => {
            pen.closed(&[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0)]);
            pen.line(&[(4.5, 6.0), (7.0, 8.0), (4.5, 10.0)]);
            pen.line(&[(8.5, 10.5), (11.5, 10.5)]);
        }
        Icon::Sidebar => {
            // The drawer opens on the right.
            g.rounded(3.0, 4.0, 18.0, 16.0, 2.0);
            g.line(&[(15.0, 4.0), (15.0, 20.0)]);
        }
        Icon::Inline => {
            // One column: a removed line over an added one.
            pen.line(&[(2.5, 4.0), (13.5, 4.0)]);
            pen.line(&[(2.5, 7.0), (4.5, 7.0)]);
            pen.line(&[(6.5, 7.0), (13.5, 7.0)]);
            pen.line(&[(2.5, 10.0), (4.5, 10.0)]);
            pen.line(&[(3.5, 9.0), (3.5, 11.0)]);
            pen.line(&[(6.5, 10.0), (13.5, 10.0)]);
            pen.line(&[(2.5, 13.0), (9.0, 13.0)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Icons stay inside their square and are centered in it, so a row of them lines up.
    #[test]
    fn icons_fit_their_square_and_are_centered() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(40.0, 40.0), egui::vec2(32.0, 32.0));
        let mut off_center = Vec::new();
        for icon in Icon::ALL {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint(ui.painter(), rect, icon, Color32::WHITE)
            });
            out.textures_delta.clear();
            let bounds = out
                .shapes
                .iter()
                .map(|s| s.shape.visual_bounding_rect())
                .filter(|r| r.width() < 100.0)
                .fold(Rect::NOTHING, |a, b| a.union(b));
            assert!(bounds.is_positive(), "{icon:?} draws nothing");
            assert!(
                rect.expand(1.0).contains_rect(bounds),
                "{icon:?} spills out of its square: {bounds:?}"
            );
            let off = bounds.center() - rect.center();
            if off.x.abs() > 2.0 || off.y.abs() > 2.0 {
                off_center.push(format!("{icon:?} {off:?}"));
            }
        }
        assert!(off_center.is_empty(), "off center: {off_center:?}");
    }
}
