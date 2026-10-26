//! The window a recompiled game runs in: SDL3 on the calling thread for the window, the
//! keyboard and gamepads, and the runtime on a thread of its own with room for the native
//! stack, paced at the NTSC frame rate.
//!
//! The runtime cannot leave the thread it was built on, so [`run`] builds it there. Between
//! the two threads go only the last finished picture (a slot the run thread fills after
//! every frame and the window empties when it draws), the pad (read before every frame) and
//! a request to stop. The window shows each picture at the largest whole-number scale that
//! fits, with square pixels, and presents with vsync; the run thread keeps its own time, so
//! a display that is not at 59.92 Hz repeats or skips a picture now and then. Sound stays
//! silent.

pub mod input;

use std::{
    fmt::Display,
    io,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anygenesis_runtime::{
    Buttons, Exit, RUN_STACK, Runtime, Vdp,
    timing::{FRAME_CLOCKS, MASTER_CLOCK_HZ},
    vdp::{
        Frame,
        render::{HEIGHT, MAX_WIDTH},
    },
};
use sdl3::{
    GamepadSubsystem,
    event::Event,
    gamepad::Gamepad,
    keyboard::Scancode,
    pixels::{Color, PixelFormat},
    render::{FRect, ScaleMode, Texture},
    sys::render::SDL_LOGICAL_PRESENTATION_INTEGER_SCALE,
};

/// One NTSC frame: 262 lines of 3420 master clocks, about 16.688 ms (59.92 Hz).
pub const FRAME_PERIOD: Duration =
    Duration::from_nanos(FRAME_CLOCKS * 1_000_000_000 / MASTER_CLOCK_HZ);

/// How far the run may fall behind its schedule before it stops catching up.
const MAX_LAG: Duration = Duration::from_millis(100);

/// Bytes per pixel of the XRGB8888 texture.
const PIXEL_BYTES: usize = 4;

/// What the window looks like when it opens.
#[derive(Debug, Clone)]
pub struct Config {
    pub title: String,
    /// Window pixels per picture pixel at first; the window can be resized.
    pub scale: u32,
}

/// What running in a window can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("SDL: {0}")]
    Sdl(String),
    #[error("starting the 68000 thread: {0}")]
    Thread(io::Error),
    #[error("the 68000 thread panicked")]
    Panicked,
}

/// Wraps an error SDL reported.
fn sdl(error: impl Display) -> Error {
    Error::Sdl(error.to_string())
}

/// What the window and the run thread share.
#[derive(Debug, Default)]
struct Shared {
    /// The last finished picture the window has not drawn yet.
    picture: Mutex<Option<Frame>>,
    /// The buttons held, as [`Buttons`] bits.
    pad: AtomicU8,
    /// Set when the window closes; the run ends before its next frame.
    stop: AtomicBool,
}

/// Runs a game in a window until the window closes or the run ends by itself.
///
/// `start` builds the runtime on the run thread; the runtime must hold a [`Vdp`]. After the
/// run, `finish` sees the runtime and the exit of its last frame, and its result is
/// returned.
///
/// # Errors
/// SDL failing to set up or draw, or the run thread failing to start or panicking.
pub fn run<R: Send + 'static>(
    config: &Config,
    start: impl FnOnce() -> Runtime + Send + 'static,
    finish: impl FnOnce(&Runtime, Exit) -> R + Send + 'static,
) -> Result<R, Error> {
    let context = sdl3::init().map_err(sdl)?;
    let video = context.video().map_err(sdl)?;
    let gamepads = context.gamepad().map_err(sdl)?;
    let mut events = context.event_pump().map_err(sdl)?;
    sdl3::hint::set("SDL_RENDER_VSYNC", "1");
    let window = video
        .window(
            &config.title,
            MAX_WIDTH as u32 * config.scale,
            HEIGHT as u32 * config.scale,
        )
        .position_centered()
        .resizable()
        .build()
        .map_err(sdl)?;
    let mut canvas = window.into_canvas();
    let creator = canvas.texture_creator();
    let mut texture = creator
        .create_texture_streaming(PixelFormat::XRGB8888, MAX_WIDTH as u32, HEIGHT as u32)
        .map_err(sdl)?;
    texture.set_scale_mode(ScaleMode::Nearest);

    let shared = Arc::new(Shared::default());
    let worker = {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("68000".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let mut runtime = start();
                let exit = run_paced(&mut runtime, &shared);
                finish(&runtime, exit)
            })
            .map_err(Error::Thread)?
    };

    let mut pad: Option<Gamepad> = None;
    let mut width = 0;
    let drawn = loop {
        let mut closed = false;
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    scancode: Some(Scancode::Escape),
                    ..
                } => closed = true,
                Event::GamepadAdded { which, .. } if pad.is_none() => {
                    pad = gamepads.open(which).ok();
                }
                Event::GamepadRemoved { which, .. }
                    if pad.as_ref().and_then(|pad| pad.id().ok()) == Some(which) =>
                {
                    pad = first_gamepad(&gamepads);
                }
                _ => {}
            }
        }
        if closed || worker.is_finished() {
            break Ok(());
        }
        let buttons = input::read(&events.keyboard_state(), pad.as_ref());
        shared.pad.store(buttons.0, Ordering::Relaxed);
        let picture = shared
            .picture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(picture) = picture {
            if let Err(error) = upload(&mut texture, &picture) {
                break Err(error);
            }
            if picture.width() != width {
                width = picture.width();
                let shown = canvas.set_logical_size(
                    width as u32,
                    HEIGHT as u32,
                    SDL_LOGICAL_PRESENTATION_INTEGER_SCALE,
                );
                if let Err(error) = shown {
                    break Err(sdl(error));
                }
            }
        }
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        if width != 0 {
            let shown = FRect::new(0.0, 0.0, width as f32, HEIGHT as f32);
            if let Err(error) = canvas.copy(&texture, shown, shown) {
                break Err(sdl(error));
            }
        }
        canvas.present();
    };
    shared.stop.store(true, Ordering::Relaxed);
    let result = worker.join().map_err(|_| Error::Panicked)?;
    drawn.map(|()| result)
}

/// The first gamepad connected, opened, if any.
fn first_gamepad(gamepads: &GamepadSubsystem) -> Option<Gamepad> {
    let id = *gamepads.gamepads().ok()?.first()?;
    gamepads.open(id).ok()
}

/// Copies `picture` into the top left of `texture`.
fn upload(texture: &mut Texture<'_>, picture: &Frame) -> Result<(), Error> {
    texture
        .with_lock(None, |buffer, pitch| {
            let (rows, _) = picture.pixels().as_chunks::<MAX_WIDTH>();
            for (target, row) in buffer.chunks_mut(pitch).zip(rows) {
                let (pixels, _) = target[..MAX_WIDTH * PIXEL_BYTES].as_chunks_mut::<PIXEL_BYTES>();
                for (bytes, pixel) in pixels.iter_mut().zip(row) {
                    *bytes = pixel.to_ne_bytes();
                }
            }
        })
        .map_err(sdl)
}

/// Runs frame after frame at the NTSC rate until `shared` asks to stop or the run ends by
/// itself, applying the pad before each frame and leaving each finished picture in the
/// slot; returns the last frame's exit.
fn run_paced(runtime: &mut Runtime, shared: &Shared) -> Exit {
    let mut due = Instant::now();
    loop {
        runtime.set_pad(Buttons(shared.pad.load(Ordering::Relaxed)));
        let exit = runtime.run_frames(1);
        if let Some(vdp) = runtime.hardware::<Vdp>() {
            *shared
                .picture
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(vdp.frame().clone());
        }
        if !matches!(exit, Exit::Frames { .. }) || shared.stop.load(Ordering::Relaxed) {
            return exit;
        }
        due = next_due(due, Instant::now());
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
}

/// When the frame after one due at `due` is due, given that it is `now`: one period later,
/// unless the run has fallen more than [`MAX_LAG`] behind, when it starts afresh from `now`.
fn next_due(due: Instant, now: Instant) -> Instant {
    let next = due + FRAME_PERIOD;
    if now.saturating_duration_since(next) > MAX_LAG {
        now
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_period_is_the_ntsc_frame_rate() {
        assert_eq!(FRAME_PERIOD, Duration::from_nanos(16_688_154));
        let rate = 1.0 / FRAME_PERIOD.as_secs_f64();
        assert!((rate - 59.9227).abs() < 0.0001, "{rate}");
    }

    #[test]
    fn test_next_due_keeps_the_schedule_while_on_time() {
        let start = Instant::now();
        assert_eq!(next_due(start, start), start + FRAME_PERIOD);
        let late = start + FRAME_PERIOD + MAX_LAG;
        assert_eq!(next_due(start, late), start + FRAME_PERIOD);
    }

    #[test]
    fn test_next_due_starts_afresh_when_far_behind() {
        let start = Instant::now();
        let late = start + FRAME_PERIOD + MAX_LAG + Duration::from_millis(1);
        assert_eq!(next_due(start, late), late);
    }
}
