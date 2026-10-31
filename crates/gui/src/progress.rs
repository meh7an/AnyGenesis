//! Build progress: the `anygenesis --progress` lines, how far along the bar a build is, and
//! the design's dissolving-edge pixel bar and finishing burst (prototype `runBuild` and
//! `burst`), all as plain data for the canvas to draw.

use std::time::Duration;

/// The six build phases the design shows, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Read,
    Patch,
    Profile,
    Translate,
    Compile,
    Link,
}

impl Phase {
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::Patch,
        Self::Profile,
        Self::Translate,
        Self::Compile,
        Self::Link,
    ];

    /// The short name in the step line under the bar.
    pub const fn step(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Patch => "Patch",
            Self::Profile => "Profile",
            Self::Translate => "Translate",
            Self::Compile => "Compile",
            Self::Link => "Link",
        }
    }

    /// The noun for a failure in this phase: "{noun} failed after 2:03".
    pub const fn failure(self) -> &'static str {
        match self {
            Self::Read => "Reading the cartridge",
            Self::Patch => "Patching",
            Self::Profile => "Profiling",
            Self::Translate => "Translating",
            Self::Compile => "Compiling",
            Self::Link => "Linking",
        }
    }

    /// Where the phase starts and ends on the bar, as the prototype splits it.
    const fn span(self) -> (f32, f32) {
        match self {
            Self::Read => (0.0, 0.06),
            Self::Patch => (0.06, 0.14),
            Self::Profile => (0.14, 0.24),
            Self::Translate => (0.24, 0.36),
            Self::Compile => (0.36, 0.86),
            Self::Link => (0.86, 1.0),
        }
    }

    /// How fast an estimated phase fills: about 63% of its span after this long.
    const fn pace(self) -> f32 {
        match self {
            Self::Read | Self::Patch => 0.5,
            Self::Profile => 15.0,
            Self::Translate | Self::Link => 3.0,
            Self::Compile => 60.0,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "patch" => Self::Patch,
            "profile" => Self::Profile,
            "translate" => Self::Translate,
            "compile" => Self::Compile,
            "link" => Self::Link,
            _ => return None,
        })
    }
}

/// One line of `anygenesis --progress` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Phase(Phase),
    Compiled {
        done: usize,
        total: usize,
    },
    Done(String),
    Error(String),
    /// Any other output, for the log.
    Text(String),
}

impl Line {
    pub fn parse(line: &str) -> Self {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("ag-progress ") {
            let mut words = rest.split(' ');
            let phase = words.next().and_then(Phase::parse);
            let counts = (words.next(), words.next());
            match (phase, counts) {
                (Some(Phase::Compile), (Some(done), Some(total))) => {
                    if let (Ok(done), Ok(total)) = (done.parse(), total.parse()) {
                        return Self::Compiled { done, total };
                    }
                }
                (Some(phase), (None, None)) => return Self::Phase(phase),
                _ => {}
            }
        } else if let Some(exe) = line.strip_prefix("ag-done ") {
            return Self::Done(exe.to_owned());
        } else if let Some(message) = line.strip_prefix("ag-error ") {
            return Self::Error(message.to_owned());
        }
        Self::Text(line.to_owned())
    }
}

/// Where a running build is, and how far the bar shows it.
#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: Phase,
    /// C files compiled and the total, once compiling starts.
    pub compiled: Option<(usize, usize)>,
    /// Time since the build started when the phase began.
    phase_started: Duration,
    /// The fraction the bar shows, which eases toward the target and never goes back.
    shown: f32,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            phase: Phase::Read,
            compiled: None,
            phase_started: Duration::ZERO,
            shown: 0.0,
        }
    }
}

impl Progress {
    /// Moves to `phase` at `elapsed` into the build.
    pub fn enter(&mut self, phase: Phase, elapsed: Duration) {
        if phase != self.phase {
            self.phase = phase;
            self.phase_started = elapsed;
        }
    }

    /// Records `done` of `total` files compiled at `elapsed`.
    pub fn compiled(&mut self, done: usize, total: usize, elapsed: Duration) {
        self.enter(Phase::Compile, elapsed);
        self.compiled = Some((done, total));
    }

    /// The fraction the build has reached at `elapsed`: the compile count while compiling,
    /// else an estimate that slows toward the phase's end.
    pub fn target(&self, elapsed: Duration) -> f32 {
        let (start, end) = self.phase.span();
        let within = match (self.phase, self.compiled) {
            (Phase::Compile, Some((done, total))) if total > 0 => done as f32 / total as f32,
            _ => {
                let seconds = elapsed.saturating_sub(self.phase_started).as_secs_f32();
                0.9 * (1.0 - (-seconds / self.phase.pace()).exp())
            }
        };
        start + (end - start) * within.clamp(0.0, 1.0)
    }

    /// Eases the shown fraction toward the target over `step` and returns it.
    pub fn advance(&mut self, elapsed: Duration, step: Duration) -> f32 {
        let pull = 1.0 - (-step.as_secs_f32() / 0.25).exp();
        let target = self.target(elapsed);
        self.shown = (self.shown + (target - self.shown) * pull).max(self.shown);
        self.shown
    }

    /// The step line: each finished phase with a check mark after it.
    pub fn steps(&self) -> String {
        Phase::ALL
            .iter()
            .map(|phase| {
                if *phase < self.phase {
                    format!("{} \u{2713}", phase.step())
                } else {
                    phase.step().to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    }

    /// The card title for the current phase.
    pub fn title(&self) -> String {
        match (self.phase, self.compiled) {
            (Phase::Read, _) => "Reading your cartridge".into(),
            (Phase::Patch, _) => "Applying patches".into(),
            (Phase::Profile, _) => "Profiling the game".into(),
            (Phase::Translate, _) => "Translating to native code".into(),
            (Phase::Compile, Some((done, total))) => {
                format!("Compiling {} of {total} files", done.max(1).min(total))
            }
            (Phase::Compile, None) => "Compiling".into(),
            (Phase::Link, _) => "Linking".into(),
        }
    }
}

/// `m:ss` for a build time.
pub fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Distance between cell centres, in logical pixels.
pub const PITCH: f32 = 5.0;
/// Rows of cells; each row is 4 px tall.
pub const ROWS: usize = 3;
/// Room above and below the 12 px bar for cells that zoom in.
pub const PAD: f32 = 18.0;
const WINDOW: f32 = 8.0;
const DROP_MS: f32 = 300.0;
const POP_MS: f32 = 160.0;
const LEVELS: [f32; 5] = [0.0, 1.5, 2.0, 3.0, 4.0];
/// The level at which a cell joins the solid fill.
const SOLID: usize = LEVELS.len();

/// The prototype's hash: a stable pseudo-random value in [0, 1) for a cell and a slot, in
/// f64 as JavaScript computes it.
fn hash(a: f32, b: f32, c: f32) -> f32 {
    let (a, b, c) = (f64::from(a), f64::from(b), f64::from(c));
    let x = (a * 12.9898 + b * 78.233 + c * 37.719).sin() * 43758.5453;
    (x - x.floor()) as f32
}

/// A filled rectangle in the bar canvas, its top-left at (`x`, `y`), `PAD` above the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Opacity, 1 for the bar's cells.
    pub alpha: f32,
}

impl Rect {
    fn square(cx: f32, cy: f32, size: f32) -> Self {
        Self {
            x: cx - size / 2.0,
            y: PAD + cy - size / 2.0,
            w: size,
            h: size,
            alpha: 1.0,
        }
    }
}

/// The dissolving-edge bar: each cell appears at a random moment within the `WINDOW`
/// columns ahead of the solid front, zooming in, then grows in random steps until it joins
/// the fill.
#[derive(Debug, Default)]
pub struct PixelBar {
    columns: usize,
    /// Per cell: the front distances at which it reaches each level (index 0 unused).
    thresholds: Vec<[f32; SOLID + 1]>,
    /// Per cell: its level and when it got there, in milliseconds.
    reached: Vec<(usize, f32)>,
}

impl PixelBar {
    fn layout(&mut self, width: f32) {
        let columns = (width / PITCH).ceil().max(0.0) as usize;
        if columns == self.columns {
            return;
        }
        self.columns = columns;
        self.thresholds = (0..columns)
            .flat_map(|column| (0..ROWS).map(move |row| (column as f32, row as f32)))
            .map(|(c, r)| {
                let mut slots = [f32::INFINITY; SOLID + 1];
                let mut previous = 0.4 + WINDOW * hash(c, r, 1.0);
                slots[1] = previous;
                for (level, slot) in slots.iter_mut().enumerate().take(SOLID).skip(2) {
                    previous -= (previous - 0.1) * (0.2 + 0.45 * hash(c, r, level as f32));
                    *slot = previous;
                }
                slots[SOLID] = (previous - 0.05).min(-0.1 + 0.3 * hash(c, r, 9.0));
                slots
            })
            .collect();
        self.reached = vec![(0, 0.0); self.thresholds.len()];
    }

    /// The rectangles to draw for a bar `width` wide at `fraction` done, `now_ms` being any
    /// running clock; `reduce_motion` skips the zoom and pop.
    pub fn frame(
        &mut self,
        width: f32,
        fraction: f32,
        now_ms: f32,
        reduce_motion: bool,
    ) -> Vec<Rect> {
        self.layout(width);
        let front = -10.0 + fraction * (self.columns as f32 + 10.0);
        let mut solid = Vec::new();
        let mut settled = Vec::new();
        let mut moving = Vec::new();
        for (cell, slots) in self.thresholds.iter().enumerate() {
            let (column, row) = (cell / ROWS, cell % ROWS);
            let distance = column as f32 - front;
            let level = (1..=SOLID)
                .rev()
                .find(|&level| distance < slots[level])
                .unwrap_or(0);
            let (previous, since) = &mut self.reached[cell];
            if level != *previous {
                *previous = level;
                *since = now_ms;
            }
            let x = column as f32 * PITCH;
            if level == SOLID {
                let h = 4.0 + if row < ROWS - 1 { 1.0 } else { 0.0 };
                solid.push(Rect {
                    x,
                    y: PAD + row as f32 * 4.0,
                    w: PITCH.min(width - x) + 1.0,
                    h,
                    alpha: 1.0,
                });
                continue;
            }
            if level == 0 {
                continue;
            }
            let (cx, cy) = (x + 3.0, row as f32 * 4.0 + 2.0);
            let duration = if level == 1 { DROP_MS } else { POP_MS };
            let age = now_ms - *since;
            if reduce_motion || age >= duration {
                settled.push(Rect::square(cx, cy, LEVELS[level]));
                continue;
            }
            if level > 1 {
                settled.push(Rect::square(cx, cy, LEVELS[level - 1]));
            }
            let eased = 1.0 - (1.0 - age / duration).powi(3);
            let zoom = if level == 1 { 2.6 } else { 0.8 };
            moving.push(Rect::square(
                cx,
                cy,
                LEVELS[level] * (1.0 + zoom * (1.0 - eased)),
            ));
        }
        solid.extend(settled);
        solid.extend(moving);
        solid
    }
}

/// One square of the finishing burst.
#[derive(Debug, Clone, Copy)]
struct Spark {
    top: f32,
    size: f32,
    dx: f32,
    dy: f32,
    duration_ms: f32,
}

/// The ~22 squares that fly from the end of the finished bar.
#[derive(Debug)]
pub struct Burst {
    sparks: Vec<Spark>,
    started_ms: f32,
}

impl Burst {
    pub const SPARKS: usize = 22;

    /// A burst starting at `now_ms`, its sparks drawn from `seed`.
    pub fn new(seed: u64, now_ms: f32) -> Self {
        let seed = (seed % 10_007) as f32;
        let sparks = (0..Self::SPARKS)
            .map(|index| {
                let random = |slot: f32| hash(seed, index as f32, slot);
                Spark {
                    top: random(1.0) * 12.0,
                    size: 3.0 + (random(2.0) * 3.0).round(),
                    dx: random(3.0) * 90.0 - 60.0,
                    dy: random(4.0) * -70.0 - 10.0,
                    duration_ms: 650.0 + random(5.0) * 300.0,
                }
            })
            .collect();
        Self {
            sparks,
            started_ms: now_ms,
        }
    }

    /// Whether any spark is still in the air at `now_ms`.
    pub fn is_running(&self, now_ms: f32) -> bool {
        let age = now_ms - self.started_ms;
        self.sparks.iter().any(|spark| age < spark.duration_ms)
    }

    /// The sparks at `now_ms` for a bar `width` wide, in the bar canvas's coordinates.
    pub fn frame(&self, width: f32, now_ms: f32) -> Vec<Rect> {
        let age = now_ms - self.started_ms;
        self.sparks
            .iter()
            .filter(|spark| age < spark.duration_ms)
            .map(|spark| {
                let t = (age / spark.duration_ms).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                Rect {
                    x: width - spark.size + spark.dx * eased,
                    y: PAD + spark.top + spark.dy * eased,
                    w: spark.size,
                    h: spark.size,
                    alpha: 1.0 - eased,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_parses_the_protocol_and_keeps_other_lines() {
        assert_eq!(Line::parse("ag-progress read"), Line::Phase(Phase::Read));
        assert_eq!(
            Line::parse("ag-progress link\r\n"),
            Line::Phase(Phase::Link)
        );
        assert_eq!(
            Line::parse("ag-progress compile 3 177"),
            Line::Compiled {
                done: 3,
                total: 177
            }
        );
        assert_eq!(
            Line::parse("ag-done C:\\Games\\s1 (Wide).exe"),
            Line::Done("C:\\Games\\s1 (Wide).exe".into())
        );
        assert_eq!(
            Line::parse("ag-error boom: bad"),
            Line::Error("boom: bad".into())
        );
        for text in [
            "ag-progress warp",
            "ag-progress compile x 2",
            "built in 3 s",
            "",
        ] {
            assert_eq!(Line::parse(text), Line::Text(text.into()), "{text:?}");
        }
    }

    #[test]
    fn test_progress_target_follows_phases_and_compile_counts() {
        let mut progress = Progress::default();
        let start = progress.target(Duration::ZERO);
        assert_eq!(start, 0.0);
        let later = progress.target(Duration::from_secs(10));
        assert!(later > 0.0 && later < 0.06, "{later}");
        progress.compiled(50, 100, Duration::from_secs(40));
        assert!((progress.target(Duration::from_secs(40)) - 0.61).abs() < 1e-6);
        progress.enter(Phase::Link, Duration::from_secs(90));
        let link = progress.target(Duration::from_secs(1000));
        assert!(link > 0.86 && link < 1.0, "{link}");
        assert_eq!(progress.steps().matches('\u{2713}').count(), 5);
        assert_eq!(progress.title(), "Linking");
    }

    #[test]
    fn test_progress_shown_eases_forward_and_never_back() {
        let mut progress = Progress::default();
        progress.compiled(10, 10, Duration::ZERO);
        let first = progress.advance(Duration::ZERO, Duration::from_millis(16));
        assert!(first > 0.0 && first < 0.86);
        let mut shown = first;
        for _ in 0..200 {
            shown = progress.advance(Duration::ZERO, Duration::from_millis(16));
        }
        assert!((shown - 0.86).abs() < 1e-3, "{shown}");
        progress.enter(Phase::Link, Duration::ZERO);
        assert!(progress.advance(Duration::ZERO, Duration::from_millis(16)) >= shown);
    }

    #[test]
    fn test_compile_title_counts_from_one() {
        let mut progress = Progress::default();
        progress.compiled(0, 177, Duration::ZERO);
        assert_eq!(progress.title(), "Compiling 1 of 177 files");
        progress.compiled(177, 177, Duration::ZERO);
        assert_eq!(progress.title(), "Compiling 177 of 177 files");
    }

    #[test]
    fn test_clock_is_minutes_and_seconds() {
        assert_eq!(clock(Duration::from_secs(0)), "0:00");
        assert_eq!(clock(Duration::from_millis(161_900)), "2:41");
        assert_eq!(clock(Duration::from_secs(3600)), "60:00");
    }

    #[test]
    fn test_pixel_bar_is_empty_at_zero_and_solid_at_one() {
        let mut bar = PixelBar::default();
        assert!(bar.frame(300.0, 0.0, 0.0, false).is_empty());
        let full = bar.frame(300.0, 1.0, 1000.0, false);
        assert_eq!(full.len(), 60 * ROWS);
        assert!(full.iter().all(|rect| rect.h >= 4.0));
        let covered: f32 = full
            .iter()
            .filter(|rect| rect.y == PAD)
            .map(|rect| rect.w - 1.0)
            .sum();
        assert_eq!(covered, 300.0);
    }

    #[test]
    fn test_pixel_bar_cells_zoom_in_then_settle() {
        let mut bar = PixelBar::default();
        let half = bar.frame(200.0, 0.5, 0.0, false);
        let zooming = half
            .iter()
            .filter(|rect| rect.w > 4.0 && rect.h > 5.0)
            .count();
        assert!(zooming > 0, "new cells start zoomed");
        let settled = bar.frame(200.0, 0.5, 1000.0, false);
        assert!(settled.iter().all(|rect| rect.w <= PITCH + 1.0));
        let mut reduced = PixelBar::default();
        let still = reduced.frame(200.0, 0.5, 0.0, true);
        assert!(still.iter().all(|rect| rect.w <= PITCH + 1.0));
    }

    #[test]
    fn test_hash_matches_the_prototype() {
        // fract(sin(1 * 12.9898 + 2 * 78.233 + 3 * 37.719) * 43758.5453) in JavaScript.
        let expected = {
            let x = (12.9898f64 + 2.0 * 78.233 + 3.0 * 37.719).sin() * 43758.5453;
            (x - x.floor()) as f32
        };
        assert!((hash(1.0, 2.0, 3.0) - expected).abs() < 1e-6);
    }

    #[test]
    fn test_burst_flies_up_from_the_bar_end_and_ends() {
        let burst = Burst::new(7, 100.0);
        assert!(burst.is_running(100.0));
        let start = burst.frame(400.0, 100.0);
        assert_eq!(start.len(), Burst::SPARKS);
        assert!(
            start
                .iter()
                .all(|spark| spark.x >= 394.0 && spark.alpha == 1.0)
        );
        let mid = burst.frame(400.0, 500.0);
        assert!(
            mid.iter()
                .all(|spark| spark.y < PAD + 12.0 && spark.alpha < 1.0)
        );
        assert!(!burst.is_running(1100.0));
        assert!(burst.frame(400.0, 1100.0).is_empty());
    }
}
