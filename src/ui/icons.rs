//! Monoline vector icons, drawn with the painter so they look the same on every platform and
//! follow the text color of the widget they are in.

use egui::{Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};
use std::f32::consts::PI;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    First,
    Prev,
    Next,
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
}

/// Maps 16×16 design-grid coordinates into `rect`.
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

    fn rays(&self, r0: f32, r1: f32, n: usize, offset: f32) {
        for i in 0..n {
            let a = (offset + i as f32 * 360.0 / n as f32) * PI / 180.0;
            let (c, s) = (a.cos(), a.sin());
            self.line(&[(8.0 + r0 * c, 8.0 + r0 * s), (8.0 + r1 * c, 8.0 + r1 * s)]);
        }
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
        stroke: Stroke::new((1.5 * scale).max(1.2), color),
        color,
    };
    match icon {
        Icon::First => {
            pen.line(&[(4.0, 4.0), (12.0, 4.0)]);
            pen.line(&[(4.0, 11.5), (8.0, 7.5), (12.0, 11.5)]);
        }
        Icon::Prev => pen.line(&[(4.0, 10.0), (8.0, 6.0), (12.0, 10.0)]),
        Icon::Next => pen.line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)]),
        Icon::Last => {
            pen.line(&[(4.0, 4.5), (8.0, 8.5), (12.0, 4.5)]);
            pen.line(&[(4.0, 12.0), (12.0, 12.0)]);
        }
        Icon::PrevConflict => {
            pen.line(&[(2.5, 11.0), (6.5, 7.0), (10.5, 11.0)]);
            pen.line(&[(13.0, 2.5), (13.0, 6.5)]);
            pen.dot(13.0, 9.0, 1.0);
        }
        Icon::NextConflict => {
            pen.line(&[(2.5, 5.0), (6.5, 9.0), (10.5, 5.0)]);
            pen.line(&[(13.0, 6.5), (13.0, 10.5)]);
            pen.dot(13.0, 13.0, 1.0);
        }
        Icon::CopyRight => {
            pen.line(&[(2.5, 8.0), (13.0, 8.0)]);
            pen.line(&[(9.0, 4.0), (13.0, 8.0), (9.0, 12.0)]);
        }
        Icon::CopyLeft => {
            pen.line(&[(13.5, 8.0), (3.0, 8.0)]);
            pen.line(&[(7.0, 4.0), (3.0, 8.0), (7.0, 12.0)]);
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
            pen.circle(7.0, 7.0, 4.5);
            pen.line(&[(10.4, 10.4), (14.0, 14.0)]);
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
            painter.text(
                pen.p(8.0, 8.3),
                egui::Align2::CENTER_CENTER,
                "?",
                FontId::proportional(10.5 * scale),
                color,
            );
        }
        Icon::Settings => {
            pen.circle(8.0, 8.0, 2.2);
            pen.circle(8.0, 8.0, 4.6);
            pen.rays(4.6, 6.6, 8, 22.5);
        }
        Icon::Sun => {
            pen.circle(8.0, 8.0, 3.0);
            pen.rays(5.0, 6.8, 8, 0.0);
        }
        Icon::Moon => {
            // A circle of radius 6 at (8, 8) minus one of radius 5 at (11.5, 4.5); the angles
            // are where the two circles intersect. Two open strokes, as a closed outline would
            // grow miter spikes at the sharp tips.
            pen.line(&pen.arc(8.0, 8.0, 6.0, 8.3, 261.7));
            pen.line(&pen.arc(11.5, 4.5, 5.0, 209.2, 60.8));
        }
        Icon::Folder => pen.closed(&[
            (2.0, 3.5),
            (6.0, 3.5),
            (7.5, 5.0),
            (14.0, 5.0),
            (14.0, 13.0),
            (2.0, 13.0),
        ]),
        Icon::File => {
            pen.closed(&[
                (3.5, 1.5),
                (9.5, 1.5),
                (12.5, 4.5),
                (12.5, 14.5),
                (3.5, 14.5),
            ]);
            pen.line(&[(9.5, 1.5), (9.5, 4.5), (12.5, 4.5)]);
        }
        Icon::Plus => {
            pen.line(&[(3.0, 8.0), (13.0, 8.0)]);
            pen.line(&[(8.0, 3.0), (8.0, 13.0)]);
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
            pen.circle(8.0, 8.0, 6.0);
            let meridian: Vec<(f32, f32)> = (0..=24)
                .map(|i| {
                    let a = i as f32 / 24.0 * 2.0 * PI;
                    (8.0 + 2.6 * a.cos(), 8.0 + 6.0 * a.sin())
                })
                .collect();
            pen.line(&meridian);
            pen.line(&[(2.0, 8.0), (14.0, 8.0)]);
        }
        Icon::Text => {
            pen.line(&[(3.0, 4.0), (13.0, 4.0)]);
            pen.line(&[(3.0, 7.0), (13.0, 7.0)]);
            pen.line(&[(3.0, 10.0), (13.0, 10.0)]);
            pen.line(&[(3.0, 13.0), (9.0, 13.0)]);
        }
        Icon::Binary => {
            let zero: Vec<(f32, f32)> = (0..=24)
                .map(|i| {
                    let a = i as f32 / 24.0 * 2.0 * PI;
                    (5.5 + 2.2 * a.cos(), 8.0 + 4.5 * a.sin())
                })
                .collect();
            pen.line(&zero);
            pen.line(&[(9.5, 5.0), (11.5, 3.5), (11.5, 12.5)]);
        }
        Icon::Image => {
            pen.closed(&[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0)]);
            pen.line(&[
                (2.5, 12.0),
                (6.5, 8.0),
                (9.0, 10.5),
                (11.0, 8.5),
                (13.5, 11.0),
            ]);
            pen.dot(10.5, 6.0, 1.2);
        }
        Icon::Warning => {
            pen.closed(&[(8.0, 2.0), (14.5, 13.5), (1.5, 13.5)]);
            pen.line(&[(8.0, 6.5), (8.0, 9.5)]);
            pen.dot(8.0, 11.6, 0.9);
        }
        Icon::Drop => {
            pen.line(&[(8.0, 2.0), (8.0, 10.0)]);
            pen.line(&[(4.5, 6.5), (8.0, 10.0), (11.5, 6.5)]);
            pen.line(&[(2.5, 10.5), (2.5, 13.5), (13.5, 13.5), (13.5, 10.5)]);
        }
        Icon::Clock => {
            pen.circle(8.0, 8.0, 6.0);
            pen.line(&[(8.0, 4.5), (8.0, 8.0), (10.5, 9.5)]);
        }
        Icon::Compare => {
            pen.closed(&[(1.5, 3.0), (6.5, 3.0), (6.5, 13.0), (1.5, 13.0)]);
            pen.closed(&[(9.5, 3.0), (14.5, 3.0), (14.5, 13.0), (9.5, 13.0)]);
            pen.line(&[(3.5, 6.5), (4.5, 6.5)]);
            pen.line(&[(11.5, 9.5), (12.5, 9.5)]);
        }
        Icon::Filter => pen.closed(&[
            (2.0, 3.0),
            (14.0, 3.0),
            (9.5, 8.5),
            (9.5, 13.5),
            (6.5, 12.0),
            (6.5, 8.5),
        ]),
        Icon::Method => {
            pen.line(&[(2.5, 4.5), (13.5, 4.5)]);
            pen.line(&[(2.5, 11.5), (13.5, 11.5)]);
            pen.circle(6.0, 4.5, 1.8);
            pen.circle(10.5, 11.5, 1.8);
        }
        Icon::Check => {
            pen.circle(8.0, 8.0, 6.5);
            pen.line(&[(5.0, 8.2), (7.2, 10.4), (11.2, 6.0)]);
        }
    }
}
