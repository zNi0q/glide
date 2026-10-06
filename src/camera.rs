pub type Point = (f32, f32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

const LOOK_BEHIND: f32 = 0.5;
const LOOK_AHEAD: f32 = 1.0;
const ACTIVITY_PX: f32 = 40.0;
const FILL: f32 = 0.6;
const CAMERA_OMEGA: f32 = 4.0;
const CURSOR_OMEGA: f32 = 30.0;

#[derive(Clone, Copy)]
struct Spring {
    pos: f32,
    vel: f32,
}

impl Spring {
    fn new(pos: f32) -> Self {
        Self { pos, vel: 0.0 }
    }

    fn step(&mut self, target: f32, omega: f32, dt: f32) -> f32 {
        let accel = omega * omega * (target - self.pos) - 2.0 * omega * self.vel;
        self.vel += accel * dt;
        self.pos += self.vel * dt;
        self.pos
    }
}

pub fn plan(
    cursor: &[Option<Point>],
    width: f32,
    height: f32,
    max_zoom: f32,
    fps: u32,
) -> Vec<Camera> {
    let dt = 1.0 / fps as f32;
    let behind = (LOOK_BEHIND * fps as f32) as usize;
    let ahead = (LOOK_AHEAD * fps as f32) as usize;
    let overview = Camera {
        x: width / 2.0,
        y: height / 2.0,
        zoom: 1.0,
    };

    let targets = (0..cursor.len()).map(|i| {
        let window = &cursor[i.saturating_sub(behind)..(i + ahead + 1).min(cursor.len())];
        target_for(window, width, height, max_zoom).unwrap_or(overview)
    });

    let (mut x, mut y, mut zoom) = (
        Spring::new(overview.x),
        Spring::new(overview.y),
        Spring::new(1.0),
    );
    targets
        .map(|t| {
            let zoom = zoom.step(t.zoom, CAMERA_OMEGA, dt).max(1.0);
            let (half_w, half_h) = (width / (2.0 * zoom), height / (2.0 * zoom));
            Camera {
                x: x.step(t.x, CAMERA_OMEGA, dt).clamp(half_w, width - half_w),
                y: y.step(t.y, CAMERA_OMEGA, dt).clamp(half_h, height - half_h),
                zoom,
            }
        })
        .collect()
}

fn target_for(window: &[Option<Point>], width: f32, height: f32, max_zoom: f32) -> Option<Camera> {
    let points: Vec<Point> = window.iter().flatten().copied().collect();
    let travel: f32 = points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum();
    if travel < ACTIVITY_PX {
        return None;
    }

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(px, py) in &points {
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    let fit_w = FILL * width / (max_x - min_x).max(1.0);
    let fit_h = FILL * height / (max_y - min_y).max(1.0);
    Some(Camera {
        x: (min_x + max_x) / 2.0,
        y: (min_y + max_y) / 2.0,
        zoom: fit_w.min(fit_h).clamp(1.0, max_zoom),
    })
}

pub fn smooth_cursor(cursor: &[Option<Point>], fps: u32) -> Vec<Option<Point>> {
    let dt = 1.0 / fps as f32;
    let mut state: Option<(Spring, Spring)> = None;
    cursor
        .iter()
        .map(|&p| {
            let Some((px, py)) = p else {
                state = None;
                return None;
            };
            let (sx, sy) = state.get_or_insert((Spring::new(px), Spring::new(py)));
            Some((sx.step(px, CURSOR_OMEGA, dt), sy.step(py, CURSOR_OMEGA, dt)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1920.0;
    const H: f32 = 1080.0;

    #[test]
    fn idle_cursor_keeps_overview() {
        let cursor = vec![Some((300.0, 300.0)); 120];
        let cams = plan(&cursor, W, H, 2.0, 60);
        assert!(
            cams.iter()
                .all(|c| c.zoom == 1.0 && c.x == W / 2.0 && c.y == H / 2.0)
        );
    }

    #[test]
    fn local_activity_zooms_in_and_follows() {
        let cursor: Vec<_> = (0..240)
            .map(|i| {
                let a = i as f32 * 0.2;
                Some((500.0 + 30.0 * a.cos(), 400.0 + 30.0 * a.sin()))
            })
            .collect();
        let last = *plan(&cursor, W, H, 2.0, 60).last().unwrap();
        assert!((last.zoom - 2.0).abs() < 0.05, "zoom {}", last.zoom);
        assert!(
            (last.x - 500.0).abs() < 10.0 && (last.y - 400.0).abs() < 10.0,
            "{last:?}"
        );
    }

    #[test]
    fn camera_stays_inside_scene() {
        let cursor: Vec<_> = (0..240)
            .map(|i| Some((5.0 + (i % 20) as f32 * 3.0, 5.0)))
            .collect();
        for c in plan(&cursor, W, H, 2.0, 60) {
            assert!(
                c.x - W / (2.0 * c.zoom) >= -0.01 && c.y - H / (2.0 * c.zoom) >= -0.01,
                "{c:?}"
            );
        }
    }

    #[test]
    fn smoothed_cursor_converges_and_hides() {
        let mut cursor = vec![Some((0.0, 0.0)); 1];
        cursor.extend(vec![Some((100.0, 50.0)); 60]);
        cursor.push(None);
        let smooth = smooth_cursor(&cursor, 60);
        let (x, y) = smooth[60].unwrap();
        assert!((x - 100.0).abs() < 0.5 && (y - 50.0).abs() < 0.5);
        assert_eq!(smooth[61], None);
    }
}
