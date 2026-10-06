use std::{
    f32::consts::TAU,
    mem,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, epaint::CircleShape, pos2, vec2,
};

use super::{
    glass::GlassPainter,
    jobs::{self, Event},
};
use crate::{
    background::Background,
    record::{Source, StopHandle},
};

const ITEM: f32 = 38.0;
const BOTTOM_GAP: f32 = 48.0;
const WINDOW_BOTTOM_GAP: f32 = 12.0;
const PAD: f32 = 8.0;
const GAP: f32 = 6.0;
const PANEL_RADIUS: f32 = 26.0;
const BUTTON_RADIUS: u8 = 16;
const MENU_GAP: f32 = 12.0;
const ICON_STROKE: f32 = 1.7;
const TEXT: Color32 = Color32::WHITE;
const MUTED: Color32 = Color32::from_rgba_premultiplied(179, 179, 179, 179);
const RECORD_RED: Color32 = Color32::from_rgb(240, 69, 59);
const LEAD_OMEGA: f32 = 26.0;
const TRAIL_OMEGA: f32 = 13.0;
const LIQUID_DAMPING: f32 = 0.68;
const LIQUID_SQUASH: f32 = 0.2;
const ZOOMS: [(Option<f32>, &str, &str); 4] = [
    (None, "Off", "normal video"),
    (Some(1.5), "1.5×", "subtle"),
    (Some(1.8), "1.8×", "default"),
    (Some(2.5), "2.5×", "close-up"),
];

enum Phase {
    Toolbar,
    Picking(StopHandle),
    Capturing,
    Recording { since: Instant, stop: StopHandle },
    Finishing,
    Rendering { done: usize, total: usize },
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone, Copy)]
struct Spring {
    pos: f32,
    vel: f32,
}

impl Spring {
    fn at(pos: f32) -> Self {
        Self { pos, vel: 0.0 }
    }

    fn step(&mut self, target: f32, omega: f32, dt: f32) {
        let accel = omega * omega * (target - self.pos) - 2.0 * LIQUID_DAMPING * omega * self.vel;
        self.vel += accel * dt;
        self.pos += self.vel * dt;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Zoom,
    Background,
}

pub struct Toolbar {
    phase: Phase,
    phase_since: Instant,
    menu: Option<Menu>,
    source: Source,
    zoom: Option<f32>,
    background: Background,
    events: Sender<Event>,
    inbox: Receiver<Event>,
    born: Instant,
    anchor: Option<Pos2>,
    indicator: Option<[Spring; 2]>,
    last_frame: Instant,
    dt: f32,
    window_mode: bool,
    move_requested: bool,
    pub regions: Vec<(Rect, f32)>,
    pub quit: bool,
}

impl Toolbar {
    pub fn new(window_mode: bool) -> Self {
        let (events, inbox) = mpsc::channel();
        Self {
            phase: Phase::Toolbar,
            phase_since: Instant::now(),
            menu: None,
            source: Source::Window,
            zoom: None,
            background: Background::Wallpaper,
            events,
            inbox,
            born: Instant::now(),
            anchor: None,
            indicator: None,
            last_frame: Instant::now(),
            dt: 0.0,
            window_mode,
            move_requested: false,
            regions: Vec::new(),
            quit: false,
        }
    }

    pub fn escape(&mut self) {
        if self.menu.take().is_some() {
            return;
        }
        match self.phase {
            Phase::Toolbar => self.quit = true,
            Phase::Done(_) | Phase::Failed(_) => self.set_phase(Phase::Toolbar),
            _ => {}
        }
    }

    pub fn take_move_request(&mut self) -> bool {
        mem::take(&mut self.move_requested)
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_since = Instant::now();
        self.menu = None;
    }

    fn poll(&mut self) {
        while let Ok(event) = self.inbox.try_recv() {
            match event {
                Event::RecordingStarted => {
                    if let Phase::Picking(_) = self.phase
                        && let Phase::Picking(stop) = mem::replace(&mut self.phase, Phase::Toolbar)
                    {
                        self.set_phase(Phase::Recording {
                            since: Instant::now(),
                            stop,
                        });
                    }
                }
                Event::RecordingFinished(Ok(dir)) => {
                    jobs::start_render(
                        dir,
                        self.zoom.unwrap_or(1.0),
                        self.background.clone(),
                        self.events.clone(),
                    );
                    self.set_phase(Phase::Rendering { done: 0, total: 0 });
                }
                Event::RecordingFinished(Err(e)) | Event::ScreenshotFinished(Err(e)) => {
                    if jobs::is_cancelled(&e) {
                        self.set_phase(Phase::Toolbar);
                    } else {
                        self.set_phase(Phase::Failed(format!("{e:#}")));
                    }
                }
                Event::ScreenshotFinished(Ok(image)) => self.set_phase(Phase::Done(image)),
                Event::RenderProgress(done, total) => {
                    if let Phase::Rendering { done: d, total: t } = &mut self.phase {
                        (*d, *t) = (done, total);
                    }
                }
                Event::RenderFinished(Ok(video)) => self.set_phase(Phase::Done(video)),
                Event::RenderFinished(Err(e)) => self.set_phase(Phase::Failed(format!("{e:#}"))),
                Event::BackgroundChosen(Some(path)) => self.background = Background::Image(path),
                Event::BackgroundChosen(None) => {}
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        self.poll();
        self.regions.clear();
        self.dt = self.last_frame.elapsed().as_secs_f32().min(1.0 / 30.0);
        self.last_frame = Instant::now();

        let entrance = ease_out(self.phase_since.elapsed().as_secs_f32() / 0.35);
        let mut painter = ui.painter().clone();
        painter.multiply_opacity(entrance);
        let mut glass = GlassPainter::new(self.born.elapsed().as_secs_f32());
        let screen = ui.max_rect();
        let lift = (1.0 - entrance) * 18.0;

        match &self.phase {
            Phase::Toolbar => self.toolbar(ui, &painter, &mut glass, screen, lift, entrance),
            Phase::Picking(_) | Phase::Capturing => {
                let text = match self.source {
                    Source::Window => "Pick a window in the dialog…",
                    Source::Screen => "Pick a screen in the dialog…",
                };
                self.waiting(ui, &painter, &mut glass, screen, lift, entrance, text);
            }
            Phase::Finishing => {
                self.waiting(
                    ui,
                    &painter,
                    &mut glass,
                    screen,
                    lift,
                    entrance,
                    "Finishing recording…",
                );
            }
            Phase::Recording { since, .. } => {
                let since = *since;
                self.recording(ui, &painter, &mut glass, screen, lift, entrance, since);
            }
            Phase::Rendering { done, total } => {
                let fraction = if *total == 0 {
                    0.0
                } else {
                    *done as f32 / *total as f32
                };
                self.rendering(ui, &painter, &mut glass, screen, lift, entrance, fraction);
            }
            Phase::Done(video) => {
                let video = video.clone();
                self.done(ui, &painter, &mut glass, screen, lift, entrance, video);
            }
            Phase::Failed(message) => {
                let message = message.clone();
                self.failed(ui, &painter, &mut glass, screen, lift, entrance, &message);
            }
        }
    }

    fn bottom_gap(&self) -> f32 {
        if self.window_mode {
            WINDOW_BOTTOM_GAP
        } else {
            BOTTOM_GAP
        }
    }

    fn panel_rect(
        &mut self,
        ui: &mut Ui,
        screen: Rect,
        width: f32,
        height: f32,
        lift: f32,
    ) -> Rect {
        let clamp = |anchor: Pos2| {
            pos2(
                anchor.x.clamp(
                    screen.left() + width / 2.0,
                    (screen.right() - width / 2.0).max(screen.left() + width / 2.0),
                ),
                anchor.y.clamp(
                    screen.top() + height,
                    screen.bottom().max(screen.top() + height),
                ),
            )
        };
        let anchor = clamp(
            self.anchor
                .unwrap_or(pos2(screen.center().x, screen.bottom() - self.bottom_gap())),
        );
        let rect = Rect::from_min_max(
            pos2(anchor.x - width / 2.0, anchor.y - height),
            pos2(anchor.x + width / 2.0, anchor.y),
        );
        self.regions.push((rect, height / 2.0));

        let drag = ui.interact(rect, ui.id().with("drag"), Sense::drag());
        if self.window_mode {
            self.move_requested |= drag.drag_started();
        } else if drag.dragged() {
            self.anchor = Some(clamp(anchor + drag.drag_delta()));
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        rect.translate(vec2(0.0, lift))
    }

    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
    ) {
        let font = FontId::proportional(13.5);
        let width_of = |text: &str| {
            painter
                .layout_no_wrap(text.to_owned(), font.clone(), TEXT)
                .size()
                .x
        };
        let zoom_value = ZOOMS
            .iter()
            .find(|(z, ..)| *z == self.zoom)
            .map_or("Off", |(_, label, _)| *label);
        let background_value = match &self.background {
            Background::Wallpaper => "Wallpaper",
            Background::Gradient => "Gradient",
            Background::Image(_) => "Image",
        };

        let source_w = |label: &str| 14.0 + 18.0 + 8.0 + width_of(label) + 14.0;
        let window_w = source_w("Window");
        let screen_w = source_w("Screen");
        let segmented_w = 3.0 + window_w + 2.0 + screen_w + 3.0;
        let menu_w = |label: &str, value: &str| {
            14.0 + 18.0 + 8.0 + width_of(label) + 8.0 + width_of(value) + 8.0 + 10.0 + 14.0
        };
        let zoom_w = menu_w("Zoom", zoom_value);
        let background_w = menu_w("Background", background_value);
        let screenshot_w = 14.0 + 18.0 + 8.0 + width_of("Screenshot") + 14.0;
        let record_w = 18.0 + 12.0 + 8.0 + width_of("Record") + 18.0;
        let divider_w = 9.0;
        let width = PAD * 2.0
            + segmented_w
            + divider_w
            + zoom_w
            + background_w
            + divider_w
            + record_w
            + ITEM
            + screenshot_w
            + GAP * 7.0;

        let panel = self.panel_rect(ui, screen, width, ITEM + PAD * 2.0, lift);
        glass.paint(painter, panel, PANEL_RADIUS, opacity);

        let mut x = panel.left() + PAD;
        let row = |x: f32, w: f32| Rect::from_min_size(pos2(x, panel.top() + PAD), vec2(w, ITEM));

        let segmented = row(x, segmented_w);
        painter.rect_filled(
            segmented,
            CornerRadius::same(18),
            Color32::from_black_alpha(46),
        );
        let mut sx = segmented.left() + 3.0;
        let options = [
            (Source::Window, "Window", window_w),
            (Source::Screen, "Screen", screen_w),
        ]
        .map(|(source, label, w)| {
            let rect = Rect::from_min_size(pos2(sx, segmented.top() + 3.0), vec2(w, ITEM - 6.0));
            sx += w + 2.0;
            (source, label, rect)
        });
        if let Some((.., selected)) = options.iter().find(|(source, ..)| *source == self.source) {
            let blob = self.liquid_indicator(*selected, segmented.left());
            liquid_blob(painter, blob);
        }
        for (source, label, rect) in options {
            if button(ui, painter, rect, label, false).clicked() {
                self.source = source;
                self.menu = None;
            }
            let icon = pos2(rect.left() + 14.0 + 9.0, rect.center().y);
            match source {
                Source::Window => window_icon(painter, icon),
                Source::Screen => screen_icon(painter, icon),
            }
            painter.text(
                pos2(rect.left() + 14.0 + 18.0 + 8.0, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                font.clone(),
                TEXT,
            );
        }
        x += segmented_w + GAP;

        divider(painter, row(x, divider_w));
        x += divider_w + GAP;

        let zoom_rect = row(x, zoom_w);
        if self.menu_button(
            ui,
            painter,
            zoom_rect,
            "Zoom",
            zoom_value,
            Menu::Zoom,
            &font,
            zoom_icon,
        ) {
            self.menu = toggle(self.menu, Menu::Zoom);
        }
        x += zoom_w + GAP;

        let background_rect = row(x, background_w);
        if self.menu_button(
            ui,
            painter,
            background_rect,
            "Background",
            background_value,
            Menu::Background,
            &font,
            image_icon,
        ) {
            self.menu = toggle(self.menu, Menu::Background);
        }
        x += background_w + GAP;

        divider(painter, row(x, divider_w));
        x += divider_w + GAP;

        let shot = row(x, screenshot_w);
        if button(ui, painter, shot, "screenshot", false).clicked() {
            jobs::take_screenshot(self.source, self.background.clone(), self.events.clone());
            self.set_phase(Phase::Capturing);
            return;
        }
        camera_icon(painter, pos2(shot.left() + 14.0 + 9.0, shot.center().y));
        painter.text(
            pos2(shot.left() + 14.0 + 18.0 + 8.0, shot.center().y),
            Align2::LEFT_CENTER,
            "Screenshot",
            font.clone(),
            TEXT,
        );
        x += screenshot_w + GAP;

        let record = row(x, record_w);
        let response = ui
            .interact(record, ui.id().with("record"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        let fill = if response.hovered() {
            Color32::from_rgb(255, 92, 82)
        } else {
            RECORD_RED
        };
        painter.rect_filled(record, CornerRadius::same(BUTTON_RADIUS), fill);
        shine(painter, record, f32::from(BUTTON_RADIUS), 0.3, 0.75);
        painter.circle_filled(
            pos2(record.left() + 18.0 + 6.0, record.center().y),
            6.0,
            Color32::WHITE,
        );
        painter.text(
            pos2(record.left() + 18.0 + 12.0 + 8.0, record.center().y),
            Align2::LEFT_CENTER,
            "Record",
            FontId::proportional(13.5),
            TEXT,
        );
        if response.clicked() {
            let stop = jobs::start_recording(self.source, self.events.clone());
            self.set_phase(Phase::Picking(stop));
            return;
        }
        x += record_w + GAP;

        let close = row(x, ITEM);
        if button(ui, painter, close, "close", false).clicked() {
            self.quit = true;
        }
        close_icon(painter, close.center(), TEXT);

        match self.menu {
            Some(Menu::Zoom) => self.zoom_menu(ui, painter, glass, panel, zoom_rect, opacity),
            Some(Menu::Background) => {
                self.background_menu(ui, painter, glass, panel, background_rect, opacity)
            }
            None => {}
        }
    }

    fn liquid_indicator(&mut self, target: Rect, origin: f32) -> Rect {
        let (left, right) = (target.left() - origin, target.right() - origin);
        let [start, end] = self
            .indicator
            .get_or_insert([Spring::at(left), Spring::at(right)]);
        let moving_right = (left + right) / 2.0 > (start.pos + end.pos) / 2.0;
        let (start_omega, end_omega) = if moving_right {
            (TRAIL_OMEGA, LEAD_OMEGA)
        } else {
            (LEAD_OMEGA, TRAIL_OMEGA)
        };
        start.step(left, start_omega, self.dt);
        end.step(right, end_omega, self.dt);

        let width = (end.pos - start.pos).max(target.height());
        let stretch = (width / target.width() - 1.0).clamp(0.0, 1.0);
        let height = target.height() * (1.0 - LIQUID_SQUASH * stretch);
        Rect::from_min_size(
            pos2(origin + start.pos, target.center().y - height / 2.0),
            vec2(width, height),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn menu_button(
        &self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        label: &str,
        value: &str,
        menu: Menu,
        font: &FontId,
        icon: fn(&Painter, egui::Pos2),
    ) -> bool {
        let clicked = button(ui, painter, rect, label, self.menu == Some(menu)).clicked();
        icon(painter, pos2(rect.left() + 14.0 + 9.0, rect.center().y));
        let label_x = rect.left() + 14.0 + 18.0 + 8.0;
        let label_rect = painter.text(
            pos2(label_x, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            TEXT,
        );
        let value_rect = painter.text(
            pos2(label_rect.right() + 8.0, rect.center().y),
            Align2::LEFT_CENTER,
            value,
            font.clone(),
            MUTED,
        );
        chevron_up(
            painter,
            pos2(value_rect.right() + 8.0 + 5.0, rect.center().y),
        );
        clicked
    }

    fn menu_panel(
        &mut self,
        painter: &Painter,
        glass: &mut GlassPainter,
        panel: Rect,
        anchor: Rect,
        rows: usize,
        opacity: f32,
    ) -> Rect {
        let size = vec2(210.0, 6.0 + 24.0 + rows as f32 * 38.0 + 6.0);
        let mut rect = Rect::from_min_size(
            pos2(
                anchor.center().x - size.x / 2.0,
                panel.top() - MENU_GAP - size.y,
            ),
            size,
        );
        rect = rect.translate(vec2((panel.left() - rect.left()).max(0.0), 0.0));
        if rect.top() < 0.0 {
            rect = rect.translate(vec2(0.0, panel.height() + 2.0 * MENU_GAP + size.y));
        }
        self.regions.push((rect, 20.0));
        glass.paint(painter, rect, 20.0, opacity);
        rect
    }

    fn zoom_menu(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        panel: Rect,
        anchor: Rect,
        opacity: f32,
    ) {
        let rect = self.menu_panel(painter, glass, panel, anchor, ZOOMS.len(), opacity);
        menu_title(painter, rect, "Follow cursor");
        for (i, (zoom, label, hint)) in ZOOMS.iter().enumerate() {
            if menu_row(ui, painter, rect, i, label, hint, self.zoom == *zoom) {
                self.zoom = *zoom;
                self.menu = None;
            }
        }
    }

    fn background_menu(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        panel: Rect,
        anchor: Rect,
        opacity: f32,
    ) {
        let rect = self.menu_panel(painter, glass, panel, anchor, 3, opacity);
        menu_title(painter, rect, "Behind the window");
        let image_hint = match &self.background {
            Background::Image(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if menu_row(
            ui,
            painter,
            rect,
            0,
            "Wallpaper",
            "your desktop",
            self.background == Background::Wallpaper,
        ) {
            self.background = Background::Wallpaper;
            self.menu = None;
        }
        if menu_row(
            ui,
            painter,
            rect,
            1,
            "Gradient",
            "",
            self.background == Background::Gradient,
        ) {
            self.background = Background::Gradient;
            self.menu = None;
        }
        if menu_row(
            ui,
            painter,
            rect,
            2,
            "Choose image…",
            &image_hint,
            matches!(self.background, Background::Image(_)),
        ) {
            jobs::choose_background(self.events.clone());
            self.menu = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn waiting(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
        text: &str,
    ) {
        let font = FontId::proportional(14.0);
        let text_w = painter
            .layout_no_wrap(text.to_owned(), font.clone(), MUTED)
            .size()
            .x;
        let pill = self.panel_rect(ui, screen, 18.0 + 16.0 + 12.0 + text_w + 20.0, 50.0, lift);
        glass.paint(painter, pill, 25.0, opacity);
        spinner(
            painter,
            pos2(pill.left() + 18.0 + 8.0, pill.center().y),
            self.born.elapsed().as_secs_f32(),
        );
        painter.text(
            pos2(pill.left() + 18.0 + 16.0 + 12.0, pill.center().y),
            Align2::LEFT_CENTER,
            text,
            font,
            MUTED,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn recording(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
        since: Instant,
    ) {
        let pill = self.panel_rect(
            ui,
            screen,
            16.0 + 10.0 + 12.0 + 48.0 + 12.0 + ITEM + 6.0,
            50.0,
            lift,
        );
        glass.paint(painter, pill, 25.0, opacity);

        let dot = pos2(pill.left() + 16.0 + 5.0, pill.center().y);
        let t = (self.born.elapsed().as_secs_f32() / 1.6).fract();
        let ring = 5.0 + 10.0 * ease_out(t / 0.8);
        let ring_alpha = ((1.0 - t / 0.8).max(0.0) * 0.65 * 255.0) as u8;
        painter.circle_filled(
            dot,
            ring,
            Color32::from_rgba_unmultiplied(255, 69, 58, ring_alpha),
        );
        painter.circle_filled(dot, 5.0, RECORD_RED);

        let seconds = since.elapsed().as_secs();
        painter.text(
            pos2(dot.x + 5.0 + 12.0, pill.center().y),
            Align2::LEFT_CENTER,
            format!("{:02}:{:02}", seconds / 60, seconds % 60),
            FontId::monospace(14.0),
            TEXT,
        );

        let stop_rect = Rect::from_min_size(
            pos2(pill.right() - 6.0 - ITEM, pill.top() + 6.0),
            Vec2::splat(ITEM),
        );
        let response = ui
            .interact(stop_rect, ui.id().with("stop"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        let fill = if response.hovered() {
            Color32::from_rgba_unmultiplied(255, 69, 58, 217)
        } else {
            Color32::from_white_alpha(36)
        };
        painter.circle_filled(stop_rect.center(), ITEM / 2.0, fill);
        shine(painter, stop_rect, ITEM / 2.0, 0.2, 0.6);
        painter.rect_filled(
            Rect::from_center_size(stop_rect.center(), Vec2::splat(12.0)),
            CornerRadius::same(3),
            TEXT,
        );
        if response.clicked()
            && let Phase::Recording { stop, .. } = &self.phase
        {
            stop.stop();
            self.set_phase(Phase::Finishing);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rendering(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
        fraction: f32,
    ) {
        let font = FontId::proportional(14.0);
        let label_w = painter
            .layout_no_wrap("Rendering 4K".to_owned(), font.clone(), TEXT)
            .size()
            .x;
        let pill = self.panel_rect(
            ui,
            screen,
            18.0 + label_w + 14.0 + 220.0 + 14.0 + 40.0 + 14.0,
            50.0,
            lift,
        );
        glass.paint(painter, pill, 25.0, opacity);
        painter.text(
            pos2(pill.left() + 18.0, pill.center().y),
            Align2::LEFT_CENTER,
            "Rendering 4K",
            font.clone(),
            TEXT,
        );

        let track = Rect::from_min_size(
            pos2(pill.left() + 18.0 + label_w + 14.0, pill.center().y - 3.0),
            vec2(220.0, 6.0),
        );
        painter.rect_filled(track, CornerRadius::same(3), Color32::from_black_alpha(64));
        let fill = Rect::from_min_size(
            track.min,
            vec2(track.width() * fraction.clamp(0.0, 1.0), track.height()),
        );
        painter.add(
            egui::Shadow {
                offset: [0, 0],
                blur: 10,
                spread: 0,
                color: Color32::from_rgba_unmultiplied(136, 192, 208, 120),
            }
            .as_shape(fill, CornerRadius::same(3)),
        );
        painter.rect_filled(
            fill,
            CornerRadius::same(3),
            Color32::from_rgb(150, 190, 214),
        );
        painter.text(
            pos2(track.right() + 14.0, pill.center().y),
            Align2::LEFT_CENTER,
            format!("{}%", (fraction * 100.0) as u32),
            FontId::monospace(13.0),
            MUTED,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn done(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
        video: PathBuf,
    ) {
        let font = FontId::proportional(14.0);
        let name = video
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let saved = format!("Saved {name}");
        let width_of = |text: &str| {
            painter
                .layout_no_wrap(text.to_owned(), font.clone(), TEXT)
                .size()
                .x
        };
        let open_w = width_of("Open") + 28.0;
        let folder_w = width_of("Show in folder") + 28.0;
        let pill = self.panel_rect(
            ui,
            screen,
            16.0 + 22.0
                + 12.0
                + width_of(&saved)
                + 12.0
                + open_w
                + 4.0
                + folder_w
                + 4.0
                + ITEM
                + 6.0,
            50.0,
            lift,
        );
        glass.paint(painter, pill, 25.0, opacity);

        let check = pos2(pill.left() + 16.0 + 11.0, pill.center().y);
        painter.add(CircleShape::filled(
            check,
            11.0,
            Color32::from_rgb(48, 209, 88),
        ));
        painter.add(Shape::line(
            vec![
                check + vec2(-4.5, 0.5),
                check + vec2(-1.0, 4.0),
                check + vec2(5.0, -3.5),
            ],
            Stroke::new(2.4, Color32::WHITE),
        ));
        painter.text(
            pos2(check.x + 11.0 + 12.0, pill.center().y),
            Align2::LEFT_CENTER,
            &saved,
            font.clone(),
            TEXT,
        );

        let mut x = pill.right() - 6.0 - ITEM;
        let close = Rect::from_min_size(pos2(x, pill.top() + 6.0), Vec2::splat(ITEM));
        if button(ui, painter, close, "dismiss", false).clicked() {
            self.set_phase(Phase::Toolbar);
        }
        close_icon(painter, close.center(), TEXT);
        x -= 4.0 + folder_w;
        let folder = Rect::from_min_size(pos2(x, pill.top() + 6.0), vec2(folder_w, ITEM));
        if button(ui, painter, folder, "folder", false).clicked()
            && let Some(dir) = video.parent()
        {
            jobs::open(dir);
        }
        painter.text(
            folder.center(),
            Align2::CENTER_CENTER,
            "Show in folder",
            font.clone(),
            TEXT,
        );
        x -= 4.0 + open_w;
        let open = Rect::from_min_size(pos2(x, pill.top() + 6.0), vec2(open_w, ITEM));
        if button(ui, painter, open, "open", false).clicked() {
            jobs::open(&video);
        }
        painter.text(open.center(), Align2::CENTER_CENTER, "Open", font, TEXT);
    }

    #[allow(clippy::too_many_arguments)]
    fn failed(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        glass: &mut GlassPainter,
        screen: Rect,
        lift: f32,
        opacity: f32,
        message: &str,
    ) {
        let font = FontId::proportional(14.0);
        let mut text: String = message
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(70)
            .collect();
        if text.is_empty() {
            text = "Something went wrong".to_owned();
        }
        let text_w = painter
            .layout_no_wrap(text.clone(), font.clone(), TEXT)
            .size()
            .x;
        let back_w = painter
            .layout_no_wrap("Back".to_owned(), font.clone(), TEXT)
            .size()
            .x
            + 28.0;
        let pill = self.panel_rect(
            ui,
            screen,
            16.0 + 22.0 + 12.0 + text_w + 12.0 + back_w + 6.0,
            50.0,
            lift,
        );
        glass.paint(painter, pill, 25.0, opacity);
        let badge = pos2(pill.left() + 16.0 + 11.0, pill.center().y);
        painter.circle_filled(badge, 11.0, RECORD_RED);
        painter.text(
            badge,
            Align2::CENTER_CENTER,
            "!",
            FontId::proportional(15.0),
            TEXT,
        );
        painter.text(
            pos2(badge.x + 11.0 + 12.0, pill.center().y),
            Align2::LEFT_CENTER,
            &text,
            font.clone(),
            TEXT,
        );
        let back = Rect::from_min_size(
            pos2(pill.right() - 6.0 - back_w, pill.top() + 6.0),
            vec2(back_w, ITEM),
        );
        if button(ui, painter, back, "back", false).clicked() {
            self.set_phase(Phase::Toolbar);
        }
        painter.text(back.center(), Align2::CENTER_CENTER, "Back", font, TEXT);
    }
}

fn button(ui: &mut Ui, painter: &Painter, rect: Rect, id: &str, pressed: bool) -> Response {
    let response = ui
        .interact(rect, ui.id().with(id), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    if pressed {
        liquid_blob(painter, rect);
    } else if response.hovered() {
        let radius = f32::from(BUTTON_RADIUS).min(rect.height() / 2.0);
        painter.rect_filled(
            rect,
            CornerRadius::same(radius as u8),
            Color32::from_white_alpha(36),
        );
        shine(painter, rect, radius, 0.12, 0.35);
    }
    response
}

fn liquid_blob(painter: &Painter, rect: Rect) {
    let radius = CornerRadius::same(BUTTON_RADIUS.min((rect.height() / 2.0) as u8));
    painter.rect_filled(rect, radius, Color32::from_white_alpha(66));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, Color32::from_white_alpha(28)),
        StrokeKind::Inside,
    );
    shine(painter, rect, f32::from(radius.nw), 0.22, 0.7);
}

fn shine(painter: &Painter, rect: Rect, radius: f32, gloss: f32, rim: f32) {
    let outline = rounded_outline(rect.shrink(0.5), (radius - 0.5).max(0.0));
    let gloss_at = |p: Pos2| {
        let alpha = gloss * (1.0 - (p.y - rect.top()) / (rect.height() * 0.6));
        Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8)
    };

    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.center(), gloss_at(rect.center()));
    for &point in &outline {
        mesh.colored_vertex(point, gloss_at(point));
    }
    let count = outline.len() as u32;
    for i in 0..count {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % count);
    }
    painter.add(mesh);

    for pair in outline.windows(2) {
        let mid = pair[0].lerp(pair[1], 0.5);
        let height = ((mid.y - rect.top()) / (rect.height() * 0.5)).clamp(0.0, 1.0);
        let along = ((mid.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let alpha = rim * (1.0 - height).powi(2) * (1.0 - 0.6 * along);
        if alpha > 0.01 {
            painter.line_segment(
                [pair[0], pair[1]],
                Stroke::new(1.2, Color32::from_white_alpha((alpha * 255.0) as u8)),
            );
        }
    }
}

fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (pos2(rect.left() + radius, rect.top() + radius), 0.5),
        (pos2(rect.right() - radius, rect.top() + radius), 0.75),
        (pos2(rect.right() - radius, rect.bottom() - radius), 0.0),
        (pos2(rect.left() + radius, rect.bottom() - radius), 0.25),
    ];
    let steps = 10;
    corners
        .iter()
        .flat_map(|&(center, start)| {
            (0..=steps).map(move |i| {
                let angle = (start + 0.25 * i as f32 / steps as f32) * TAU;
                center + vec2(angle.cos(), angle.sin()) * radius
            })
        })
        .collect()
}

fn menu_title(painter: &Painter, rect: Rect, title: &str) {
    painter.text(
        pos2(rect.left() + 18.0, rect.top() + 6.0 + 12.0),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        FontId::proportional(10.5),
        MUTED,
    );
}

fn menu_row(
    ui: &mut Ui,
    painter: &Painter,
    menu: Rect,
    index: usize,
    label: &str,
    hint: &str,
    checked: bool,
) -> bool {
    let row = Rect::from_min_size(
        pos2(
            menu.left() + 6.0,
            menu.top() + 6.0 + 24.0 + index as f32 * 38.0,
        ),
        vec2(menu.width() - 12.0, 36.0),
    );
    let response = ui
        .interact(row, ui.id().with(("menu", label)), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    if response.hovered() {
        painter.rect_filled(row, CornerRadius::same(12), Color32::from_white_alpha(36));
    }
    painter.text(
        pos2(row.left() + 12.0, row.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        TEXT,
    );
    let hint_right = if checked {
        let dot = pos2(row.right() - 14.0, row.center().y);
        painter.circle_filled(dot, 6.0, Color32::from_white_alpha(40));
        painter.circle_filled(dot, 3.5, TEXT);
        row.right() - 28.0
    } else {
        row.right() - 12.0
    };
    painter.text(
        pos2(hint_right, row.center().y),
        Align2::RIGHT_CENTER,
        hint,
        FontId::proportional(12.0),
        MUTED,
    );
    response.clicked()
}

fn toggle(current: Option<Menu>, menu: Menu) -> Option<Menu> {
    if current == Some(menu) {
        None
    } else {
        Some(menu)
    }
}

fn divider(painter: &Painter, rect: Rect) {
    let x = rect.center().x;
    let steps = 12;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let alpha = (1.0 - ((t0 + t1) - 1.0).abs()) * 80.0;
        painter.line_segment(
            [
                pos2(x, rect.top() + 6.0 + t0 * (rect.height() - 12.0)),
                pos2(x, rect.top() + 6.0 + t1 * (rect.height() - 12.0)),
            ],
            Stroke::new(1.0, Color32::from_white_alpha(alpha as u8)),
        );
    }
}

fn spinner(painter: &Painter, center: egui::Pos2, time: f32) {
    painter.circle_stroke(center, 7.0, Stroke::new(2.0, Color32::from_white_alpha(56)));
    let start = time * TAU / 0.9;
    let points: Vec<_> = (0..=16)
        .map(|i| {
            let angle = start + i as f32 / 16.0 * TAU * 0.28;
            center + vec2(angle.cos(), angle.sin()) * 7.0
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, Color32::WHITE)));
}

fn icon_stroke() -> Stroke {
    Stroke::new(ICON_STROKE, TEXT)
}

fn window_icon(painter: &Painter, c: egui::Pos2) {
    let rect = Rect::from_center_size(c, vec2(16.0, 13.0));
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        icon_stroke(),
        StrokeKind::Middle,
    );
    painter.line_segment(
        [
            pos2(rect.left(), rect.top() + 3.6),
            pos2(rect.right(), rect.top() + 3.6),
        ],
        icon_stroke(),
    );
}

fn screen_icon(painter: &Painter, c: egui::Pos2) {
    let rect = Rect::from_center_size(c + vec2(0.0, -2.0), vec2(16.0, 11.0));
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        icon_stroke(),
        StrokeKind::Middle,
    );
    painter.line_segment(
        [pos2(c.x, rect.bottom()), pos2(c.x, rect.bottom() + 3.5)],
        icon_stroke(),
    );
    painter.line_segment(
        [
            pos2(c.x - 3.5, rect.bottom() + 3.5),
            pos2(c.x + 3.5, rect.bottom() + 3.5),
        ],
        icon_stroke(),
    );
}

fn camera_icon(painter: &Painter, c: egui::Pos2) {
    let body = Rect::from_center_size(c + vec2(0.0, 1.0), vec2(17.0, 12.0));
    painter.rect_stroke(
        body,
        CornerRadius::same(3),
        icon_stroke(),
        StrokeKind::Middle,
    );
    painter.add(Shape::line(
        vec![
            pos2(c.x - 4.0, body.top()),
            pos2(c.x - 2.5, body.top() - 2.2),
            pos2(c.x + 2.5, body.top() - 2.2),
            pos2(c.x + 4.0, body.top()),
        ],
        icon_stroke(),
    ));
    painter.circle_stroke(body.center(), 3.2, icon_stroke());
}

fn zoom_icon(painter: &Painter, c: egui::Pos2) {
    let lens = c + vec2(-1.5, -1.5);
    painter.circle_stroke(lens, 5.8, icon_stroke());
    painter.line_segment(
        [lens + vec2(4.2, 4.2), lens + vec2(8.5, 8.5)],
        icon_stroke(),
    );
    painter.line_segment(
        [lens + vec2(-2.6, 0.0), lens + vec2(2.6, 0.0)],
        icon_stroke(),
    );
    painter.line_segment(
        [lens + vec2(0.0, -2.6), lens + vec2(0.0, 2.6)],
        icon_stroke(),
    );
}

fn image_icon(painter: &Painter, c: egui::Pos2) {
    let rect = Rect::from_center_size(c, vec2(16.0, 14.0));
    painter.rect_stroke(
        rect,
        CornerRadius::same(3),
        icon_stroke(),
        StrokeKind::Middle,
    );
    painter.circle_stroke(rect.min + vec2(5.0, 5.0), 1.6, icon_stroke());
    painter.add(Shape::line(
        vec![
            pos2(rect.left() + 1.0, rect.bottom() - 2.5),
            pos2(rect.left() + 6.0, rect.bottom() - 7.0),
            pos2(rect.left() + 10.0, rect.bottom() - 3.5),
            pos2(rect.left() + 12.5, rect.bottom() - 6.0),
            pos2(rect.right() - 1.0, rect.bottom() - 2.5),
        ],
        icon_stroke(),
    ));
}

fn chevron_up(painter: &Painter, c: egui::Pos2) {
    painter.add(Shape::line(
        vec![c + vec2(-3.5, 2.0), c + vec2(0.0, -1.8), c + vec2(3.5, 2.0)],
        Stroke::new(1.8, MUTED),
    ));
}

fn close_icon(painter: &Painter, c: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(1.9, color);
    painter.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], stroke);
    painter.line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], stroke);
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_indicator_stretches_then_settles() {
        let mut toolbar = Toolbar::new(false);
        toolbar.dt = 1.0 / 60.0;
        let window = Rect::from_min_size(pos2(103.0, 3.0), vec2(100.0, 32.0));
        let screen = Rect::from_min_size(pos2(205.0, 3.0), vec2(96.0, 32.0));
        toolbar.liquid_indicator(window, 100.0);

        let frames: Vec<Rect> = (0..90)
            .map(|_| toolbar.liquid_indicator(screen, 100.0))
            .collect();
        let widest = frames.iter().map(Rect::width).fold(0.0, f32::max);
        let flattest = frames.iter().map(Rect::height).fold(f32::MAX, f32::min);
        let settled = frames.last().unwrap();

        assert!(widest > 125.0, "widest {widest}");
        assert!(flattest < 31.0, "flattest {flattest}");
        assert!((settled.left() - screen.left()).abs() < 0.5, "{settled:?}");
        assert!(
            (settled.width() - screen.width()).abs() < 0.5,
            "{settled:?}"
        );
        assert!(
            (settled.height() - screen.height()).abs() < 0.1,
            "{settled:?}"
        );
    }

    #[test]
    fn rounded_outline_traces_the_button_edge() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(120.0, 38.0));
        let outline = rounded_outline(rect, 16.0);
        assert!(outline.iter().all(|p| rect.expand(0.01).contains(*p)));
        let top = outline.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        let left = outline.iter().map(|p| p.x).fold(f32::MAX, f32::min);
        assert!((top - rect.top()).abs() < 0.01 && (left - rect.left()).abs() < 0.01);
        assert!(outline[0].y > outline[10].y);
    }
}
