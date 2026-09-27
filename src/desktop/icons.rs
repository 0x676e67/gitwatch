//! Small vector icons that keep button labels independent of installed fonts.

use eframe::egui::{self, AtomExt, Color32, Painter, Rect, Stroke, Widget};

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Settings,
    Backup,
    Add,
    Refresh,
    Tray,
    Start,
    Stop,
    Once,
    Edit,
    Remove,
    History,
    Upload,
    Download,
    Close,
    Restore,
    Preview,
    Diff,
    Save,
    Folder,
    File,
    Grip,
    Expand,
    Collapse,
}

impl Icon {
    pub(super) fn button(self, label: &str) -> impl Widget + '_ {
        move |ui: &mut egui::Ui| {
            let id = egui::Id::new("button-icon");
            let icon = egui::AtomKind::Empty
                .atom_size(egui::vec2(16.0, 16.0))
                .atom_id(id);
            let output = egui::Button::new((icon, label)).atom_ui(ui);
            if let Some(rect) = output.rect(id) {
                self.paint(
                    ui.painter(),
                    rect,
                    ui.style().interact(&output.response).fg_stroke.color,
                );
            }
            output.response
        }
    }

    pub(super) fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        let point = |x: f32, y: f32| rect.min + egui::vec2(x, y) * (rect.width() / 16.0);
        let stroke = Stroke::new(1.5 * rect.width() / 16.0, color);
        let path = |points: &[[f32; 2]]| {
            painter.add(egui::Shape::line(
                points.iter().map(|p| point(p[0], p[1])).collect(),
                stroke,
            ));
        };
        let circle = |x, y, radius| {
            painter.circle_stroke(point(x, y), radius * rect.width() / 16.0, stroke);
        };
        match self {
            Self::Expand => path(&[[5.0, 3.0], [10.0, 8.0], [5.0, 13.0]]),
            Self::Collapse => path(&[[3.0, 5.0], [8.0, 10.0], [13.0, 5.0]]),
            Self::Settings => {
                for (y, x) in [(4.0, 5.0), (8.0, 11.0), (12.0, 6.0)] {
                    path(&[[2.0, y], [x - 2.0, y]]);
                    path(&[[x + 2.0, y], [14.0, y]]);
                    circle(x, y, 1.8);
                }
            }
            Self::Backup => {
                path(&[
                    [2.0, 5.0],
                    [2.0, 3.0],
                    [14.0, 3.0],
                    [14.0, 5.0],
                    [2.0, 5.0],
                    [3.0, 5.0],
                    [3.0, 13.0],
                    [13.0, 13.0],
                    [13.0, 5.0],
                ]);
                path(&[[6.0, 8.0], [10.0, 8.0]]);
            }
            Self::Add => {
                path(&[[8.0, 2.0], [8.0, 14.0]]);
                path(&[[2.0, 8.0], [14.0, 8.0]]);
            }
            Self::Refresh => {
                path(&[[13.0, 6.0], [11.0, 3.0], [7.0, 2.0], [3.0, 4.0], [2.0, 8.0]]);
                path(&[[9.0, 6.0], [13.0, 6.0], [13.0, 2.0]]);
                path(&[
                    [3.0, 10.0],
                    [5.0, 13.0],
                    [9.0, 14.0],
                    [13.0, 12.0],
                    [14.0, 8.0],
                ]);
                path(&[[7.0, 10.0], [3.0, 10.0], [3.0, 14.0]]);
            }
            Self::Tray | Self::Download | Self::Upload => {
                path(&[[2.0, 10.0], [2.0, 14.0], [14.0, 14.0], [14.0, 10.0]]);
                path(&[[8.0, 2.0], [8.0, 10.0]]);
                if matches!(self, Self::Upload) {
                    path(&[[4.0, 6.0], [8.0, 2.0], [12.0, 6.0]]);
                } else {
                    path(&[[4.0, 6.0], [8.0, 10.0], [12.0, 6.0]]);
                }
            }
            Self::Start | Self::Once => {
                path(&[[3.0, 2.0], [12.0, 8.0], [3.0, 14.0], [3.0, 2.0]]);
                if matches!(self, Self::Once) {
                    path(&[[14.0, 3.0], [14.0, 13.0]]);
                }
            }
            Self::Stop => path(&[
                [3.0, 3.0],
                [13.0, 3.0],
                [13.0, 13.0],
                [3.0, 13.0],
                [3.0, 3.0],
            ]),
            Self::Edit => {
                path(&[
                    [2.0, 14.0],
                    [3.0, 10.0],
                    [11.0, 2.0],
                    [14.0, 5.0],
                    [6.0, 13.0],
                    [2.0, 14.0],
                ]);
                path(&[[9.0, 4.0], [12.0, 7.0]]);
            }
            Self::Remove => {
                path(&[[2.0, 4.0], [14.0, 4.0]]);
                path(&[[6.0, 4.0], [6.0, 2.0], [10.0, 2.0], [10.0, 4.0]]);
                path(&[[4.0, 4.0], [5.0, 14.0], [11.0, 14.0], [12.0, 4.0]]);
                path(&[[7.0, 7.0], [7.0, 11.0]]);
                path(&[[9.0, 7.0], [9.0, 11.0]]);
            }
            Self::History => {
                circle(8.0, 8.0, 6.0);
                path(&[[8.0, 4.0], [8.0, 8.0], [11.0, 10.0]]);
            }
            Self::Close => {
                path(&[[3.0, 3.0], [13.0, 13.0]]);
                path(&[[3.0, 13.0], [13.0, 3.0]]);
            }
            Self::Restore => {
                path(&[[2.0, 7.0], [9.0, 7.0], [13.0, 9.0], [13.0, 13.0]]);
                path(&[[6.0, 3.0], [2.0, 7.0], [6.0, 11.0]]);
            }
            Self::Preview => {
                path(&[
                    [1.0, 8.0],
                    [4.0, 4.0],
                    [8.0, 3.0],
                    [12.0, 4.0],
                    [15.0, 8.0],
                    [12.0, 12.0],
                    [8.0, 13.0],
                    [4.0, 12.0],
                    [1.0, 8.0],
                ]);
                circle(8.0, 8.0, 2.3);
            }
            Self::Diff => {
                path(&[[5.0, 2.0], [5.0, 14.0]]);
                path(&[[2.0, 5.0], [5.0, 2.0], [8.0, 5.0]]);
                path(&[[11.0, 2.0], [11.0, 14.0]]);
                path(&[[8.0, 11.0], [11.0, 14.0], [14.0, 11.0]]);
            }
            Self::Save => {
                path(&[
                    [2.0, 2.0],
                    [12.0, 2.0],
                    [14.0, 4.0],
                    [14.0, 14.0],
                    [2.0, 14.0],
                    [2.0, 2.0],
                ]);
                path(&[[5.0, 2.0], [5.0, 6.0], [11.0, 6.0], [11.0, 2.0]]);
                path(&[[5.0, 14.0], [5.0, 10.0], [11.0, 10.0], [11.0, 14.0]]);
            }
            Self::Folder => path(&[
                [2.0, 13.0],
                [2.0, 3.0],
                [7.0, 3.0],
                [9.0, 5.0],
                [14.0, 5.0],
                [14.0, 13.0],
                [2.0, 13.0],
            ]),
            Self::File => {
                path(&[
                    [3.0, 2.0],
                    [9.0, 2.0],
                    [13.0, 6.0],
                    [13.0, 14.0],
                    [3.0, 14.0],
                    [3.0, 2.0],
                ]);
                path(&[[9.0, 2.0], [9.0, 6.0], [13.0, 6.0]]);
            }
            Self::Grip => {
                for x in [5.0, 11.0] {
                    for y in [3.0, 8.0, 13.0] {
                        painter.circle_filled(point(x, y), 1.2 * rect.width() / 16.0, color);
                    }
                }
            }
        }
    }
}
