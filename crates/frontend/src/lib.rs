//! The window a recompiled game runs in: SDL3 on the calling thread for the window, the
//! keyboard and gamepads, and the runtime on a thread of its own with room for the native
//! stack, paced at the NTSC frame rate.
//!
//! The runtime cannot leave the thread it was built on, so [`run`] builds it there, and
//! builds it afresh there on a reset. Between the two threads go only the last finished
//! picture (a slot the run thread fills after every frame and the window empties when it
//! draws), the [`Pad`] and the window's requests: hold, reset, stop. The window only reads
//! the buttons into the pad; the game applies them where it chooses, so a run can take them
//! at fixed points of the emulated machine and replay them there. A hold stops the run
//! between frames, so a game that takes the pad at fixed points sees no difference.
//!
//! The run is held while it is paused, while the window is minimised and, if the
//! [`Controls`] say so, while the window does not have the keyboard; it keeps running while
//! the window is moved or resized. The run thread keeps its own time ([`pace`]), so a display
//! that is not at 59.92 Hz repeats or skips a picture now and then. The window shows each
//! picture by the [`Scaling`] in its [`Settings`], in a window or borderless fullscreen, and
//! presents with vsync. Sound ([`audio`]) plays the run's writes to the sound chips
//! ([`mix`]) at the run's own pace, so a run sounds the same whatever the device; the
//! [`Settings`] remember whether it is muted.

pub mod audio;
pub mod controls;
pub mod input;
pub mod mix;
pub mod pace;
pub mod settings;

use std::{
    fmt::Display,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU8, Ordering},
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
    mouse::MouseUtil,
    pixels::{Color, PixelFormat},
    render::{Canvas, FRect, ScaleMode, Texture},
    sys::render::{
        SDL_LOGICAL_PRESENTATION_INTEGER_SCALE, SDL_LOGICAL_PRESENTATION_LETTERBOX,
        SDL_LOGICAL_PRESENTATION_STRETCH,
    },
    video::Window,
};

use crate::{
    audio::Sound,
    controls::{Chord, Hotkey, Modifiers},
    mix::Mixer,
    pace::Pacer,
};
pub use crate::{
    controls::Controls,
    settings::{Scaling, Settings},
};

/// One NTSC frame: 262 lines of 3420 master clocks, about 16.688 ms (59.92 Hz).
pub const FRAME_PERIOD: Duration =
    Duration::from_nanos(FRAME_CLOCKS * 1_000_000_000 / MASTER_CLOCK_HZ);

/// Bytes per pixel of the XRGB8888 texture.
const PIXEL_BYTES: usize = 4;

/// What the window looks like when it opens and what it lets the player do.
#[derive(Debug, Clone)]
pub struct Config {
    pub title: String,
    /// Window pixels per picture pixel at first; the window can be resized.
    pub scale: u32,
    pub controls: Controls,
    /// What the window starts with of what it remembers.
    pub settings: Settings,
    /// Where the window writes its settings when the player changes them; nowhere if none.
    pub settings_path: Option<PathBuf>,
    /// Why the reset hotkey does nothing in this run, if it does not; printed when pressed.
    pub reset_refused: Option<String>,
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

/// Pad 1's buttons as the window last read them from the keyboard and gamepad; the run
/// thread reads them whenever it applies the pad.
#[derive(Debug, Clone, Default)]
pub struct Pad(Arc<AtomicU8>);

impl Pad {
    pub fn buttons(&self) -> Buttons {
        Buttons(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, buttons: Buttons) {
        self.0.store(buttons.0, Ordering::Relaxed);
    }
}

/// Why the run is held between frames; it runs when nothing holds it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Hold {
    paused: bool,
    minimised: bool,
    unfocused: bool,
}

impl Hold {
    fn any(self) -> bool {
        self.paused || self.minimised || self.unfocused
    }
}

/// What the window asks of the run thread, taken up between frames.
#[derive(Debug, Default)]
struct Requests {
    hold: Hold,
    /// Build the runtime afresh before the next frame.
    reset: bool,
    /// End the run before the next frame.
    stop: bool,
}

/// What the run thread does after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// The next frame, on schedule.
    Go,
    /// The next frame at once, after a hold.
    Resume,
    /// A fresh runtime, its first frame at once.
    Reset,
    Stop,
}

/// What the window and the run thread share.
#[derive(Debug, Default)]
struct Shared {
    /// The last finished picture the window has not drawn yet.
    picture: Mutex<Option<Frame>>,
    pad: Pad,
    requests: Mutex<Requests>,
    /// Wakes the run thread while a hold keeps it waiting.
    wake: Condvar,
}

impl Shared {
    /// Changes the requests and wakes the run thread to see them.
    fn request(&self, change: impl FnOnce(&mut Requests)) {
        change(&mut lock(&self.requests));
        self.wake.notify_all();
    }

    /// Waits while the run is held, then says what comes next.
    fn next(&self) -> Next {
        let mut requests = lock(&self.requests);
        let held = requests.hold.any();
        while requests.hold.any() && !requests.stop {
            requests = self
                .wake
                .wait(requests)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if requests.stop {
            Next::Stop
        } else if std::mem::take(&mut requests.reset) {
            Next::Reset
        } else if held {
            Next::Resume
        } else {
            Next::Go
        }
    }
}

/// Locks `mutex`, whose data stays usable even if a thread panicked holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs a game in a window until the window closes or the run ends by itself.
///
/// `start` builds the runtime on the run thread, with the pad to apply to it (nothing
/// applies it otherwise), and anything the run should hand to `finish`; the runtime must
/// hold a [`Vdp`]. A reset calls it again and drops what it built before. After the run,
/// `finish` sees the last runtime, the exit of its last frame and what `start` handed over
/// with it, and its result is returned.
///
/// # Errors
/// SDL failing to set up or draw, or the run thread failing to start or panicking.
pub fn run<S, R: Send + 'static>(
    config: &Config,
    mut start: impl FnMut(Pad) -> (Runtime, S) + Send + 'static,
    finish: impl FnOnce(&Runtime, Exit, S) -> R + Send + 'static,
) -> Result<R, Error> {
    let controls = &config.controls;
    let context = sdl3::init().map_err(sdl)?;
    let video = context.video().map_err(sdl)?;
    let gamepads = context.gamepad().map_err(sdl)?;
    let mouse = context.mouse();
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
    let mut fullscreen = controls.fullscreen;
    if fullscreen {
        show_fullscreen(&mut canvas, &mouse, true)?;
    }

    let mut settings = config.settings;
    let sound = Arc::new(Sound::new(settings.muted));
    // Kept open for the whole run: dropping the stream closes the device.
    let stream = context
        .audio()
        .inspect_err(|error| eprintln!("sound: no audio, running silent: {error}"))
        .ok()
        .and_then(|audio| audio::open(&audio, &sound));
    let playing = stream.is_some().then(|| Arc::clone(&sound));

    let shared = Arc::new(Shared::default());
    let worker = {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("68000".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let (runtime, exit, handed) = run_paced(&mut start, &shared, playing.as_deref());
                finish(&runtime, exit, handed)
            })
            .map_err(Error::Thread)?
    };

    let mut pad: Option<Gamepad> = None;
    let mut width = 0;
    let mut paused = false;
    let mut hold = Hold::default();
    let drawn = 'window: loop {
        let mut closed = false;
        let mut reset = false;
        for event in events.poll_iter() {
            let hotkey = match event {
                Event::Quit { .. } => Some(Hotkey::Quit),
                Event::KeyDown {
                    scancode: Some(key),
                    keymod,
                    repeat: false,
                    ..
                } => controls.hotkey(Chord {
                    key,
                    modifiers: Modifiers::held(keymod),
                }),
                Event::GamepadButtonDown { which, button, .. }
                    if pad.as_ref().and_then(|pad| pad.id().ok()) == Some(which) =>
                {
                    controls.button_hotkey(button)
                }
                Event::GamepadAdded { which, .. } if pad.is_none() => {
                    pad = gamepads.open(which).ok();
                    None
                }
                Event::GamepadRemoved { which, .. }
                    if pad.as_ref().and_then(|pad| pad.id().ok()) == Some(which) =>
                {
                    pad = first_gamepad(&gamepads);
                    None
                }
                _ => None,
            };
            match hotkey {
                Some(Hotkey::Quit) => closed = true,
                Some(Hotkey::Pause) => paused = !paused,
                Some(Hotkey::Reset) => match &config.reset_refused {
                    Some(reason) => eprintln!("reset ignored: {reason}"),
                    None => {
                        reset = true;
                        paused = false;
                    }
                },
                Some(Hotkey::Fullscreen) => {
                    fullscreen = !fullscreen;
                    if let Err(error) = show_fullscreen(&mut canvas, &mouse, fullscreen) {
                        break 'window Err(error);
                    }
                }
                Some(Hotkey::Scale) => {
                    settings.scaling = settings.scaling.next();
                    eprintln!("picture scaling: {}", settings.scaling.name());
                    remember(settings, config.settings_path.as_deref());
                    if width != 0
                        && let Err(error) = show_as(&mut canvas, width, settings.scaling)
                    {
                        break 'window Err(error);
                    }
                }
                Some(Hotkey::Mute) => {
                    settings.muted = !settings.muted;
                    sound.set_muted(settings.muted);
                    eprintln!("sound: {}", if settings.muted { "off" } else { "on" });
                    remember(settings, config.settings_path.as_deref());
                }
                None => {}
            }
        }
        if closed || worker.is_finished() {
            break Ok(());
        }
        let window = canvas.window();
        let held = Hold {
            paused,
            minimised: window.is_minimized(),
            unfocused: controls.pause_on_focus_loss && !window.has_input_focus(),
        };
        if held != hold || reset {
            sound.set_held(held.any());
            shared.request(|requests| {
                requests.hold = held;
                requests.reset |= reset;
            });
        }
        if held.any() != hold.any() {
            let title = if held.any() {
                format!("{} - paused", config.title)
            } else {
                config.title.clone()
            };
            if let Err(error) = canvas.window_mut().set_title(&title) {
                break Err(sdl(error));
            }
        }
        hold = held;
        shared.pad.set(input::read(
            controls,
            &events.keyboard_state(),
            pad.as_ref(),
        ));
        let picture = lock(&shared.picture).take();
        if let Some(picture) = picture {
            if let Err(error) = upload(&mut texture, &picture) {
                break Err(error);
            }
            if picture.width() != width {
                width = picture.width();
                if let Err(error) = show_as(&mut canvas, width, settings.scaling) {
                    break Err(error);
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
    shared.request(|requests| requests.stop = true);
    let result = worker.join().map_err(|_| Error::Panicked)?;
    if stream.is_some() {
        eprintln!(
            "sound: {} underruns, {} samples dropped on a full buffer",
            sound.underruns(),
            sound.dropped() / audio::CHANNELS as u64
        );
    }
    drawn.map(|()| result)
}

/// Puts the window in borderless fullscreen with the mouse pointer hidden, or back.
fn show_fullscreen(canvas: &mut Canvas<Window>, mouse: &MouseUtil, on: bool) -> Result<(), Error> {
    canvas.window_mut().set_fullscreen(on).map_err(sdl)?;
    mouse.show_cursor(!on);
    Ok(())
}

/// Shows a picture `width` pixels wide by `scaling` from now on.
fn show_as(canvas: &mut Canvas<Window>, width: usize, scaling: Scaling) -> Result<(), Error> {
    let presentation = match scaling {
        Scaling::Exact => SDL_LOGICAL_PRESENTATION_INTEGER_SCALE,
        Scaling::Fit => SDL_LOGICAL_PRESENTATION_LETTERBOX,
        Scaling::Stretch => SDL_LOGICAL_PRESENTATION_STRETCH,
    };
    canvas
        .set_logical_size(width as u32, HEIGHT as u32, presentation)
        .map_err(sdl)
}

/// Writes `settings` to `path`, if there is one; a failure costs only the memory of them, so
/// it is reported and play goes on.
fn remember(settings: Settings, path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = settings.save(path) {
        eprintln!("settings not saved to {}: {error}", path.display());
    }
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

/// Runs frame after frame at the NTSC rate, leaving each finished picture in the slot and
/// taking up the window's requests between frames, until `shared` asks to stop or the run
/// ends by itself; returns the last runtime, its last frame's exit and what `start` handed
/// over with it. With `sound`, every frame's sound goes there, and it starts afresh with each
/// new epoch of the schedule.
fn run_paced<S>(
    start: &mut impl FnMut(Pad) -> (Runtime, S),
    shared: &Shared,
    sound: Option<&Sound>,
) -> (Runtime, Exit, S) {
    let mut begin = || {
        let (mut runtime, handed) = start(shared.pad.clone());
        if sound.is_some() {
            runtime.record_sound(true);
        }
        (runtime, handed, Mixer::default())
    };
    let (mut runtime, mut handed, mut mixer) = begin();
    let mut samples = Vec::new();
    let mut pacer = Pacer::new(Instant::now());
    loop {
        thread::sleep(pacer.due().saturating_duration_since(Instant::now()));
        let exit = runtime.run_frames(1);
        if let Some(vdp) = runtime.hardware::<Vdp>() {
            *lock(&shared.picture) = Some(vdp.frame().clone());
        }
        if let Some(sound) = sound {
            samples.clear();
            mixer.play(&runtime.take_sound_writes(), runtime.clock(), &mut samples);
            sound.push(&samples);
        }
        if !matches!(exit, Exit::Frames { .. }) {
            return (runtime, exit, handed);
        }
        let epoch = pacer.epoch();
        match shared.next() {
            Next::Go => pacer.advance(Instant::now()),
            Next::Resume => pacer.restart(Instant::now()),
            Next::Reset => {
                (runtime, handed, mixer) = begin();
                pacer.restart(Instant::now());
            }
            Next::Stop => return (runtime, exit, handed),
        }
        if pacer.epoch() != epoch
            && let Some(sound) = sound
        {
            sound.restart();
        }
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
    fn test_next_goes_on_when_nothing_is_asked() {
        assert_eq!(Shared::default().next(), Next::Go);
    }

    #[test]
    fn test_next_takes_a_reset_once() {
        let shared = Shared::default();
        shared.request(|requests| requests.reset = true);
        assert_eq!(shared.next(), Next::Reset);
        assert_eq!(shared.next(), Next::Go);
    }

    #[test]
    fn test_next_stops_before_anything_else() {
        let shared = Shared::default();
        shared.request(|requests| {
            requests.reset = true;
            requests.hold.paused = true;
            requests.stop = true;
        });
        assert_eq!(shared.next(), Next::Stop);
    }

    #[test]
    fn test_next_waits_out_a_hold_then_resumes() {
        let shared = Arc::new(Shared::default());
        shared.request(|requests| requests.hold.minimised = true);
        let waiter = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.next())
        };
        thread::sleep(Duration::from_millis(50));
        assert!(!waiter.is_finished(), "the run thread went on while held");
        shared.request(|requests| requests.hold = Hold::default());
        assert_eq!(waiter.join().expect("joins"), Next::Resume);
    }

    #[test]
    fn test_next_stop_ends_a_hold() {
        let shared = Arc::new(Shared::default());
        shared.request(|requests| requests.hold.paused = true);
        let waiter = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.next())
        };
        shared.request(|requests| requests.stop = true);
        assert_eq!(waiter.join().expect("joins"), Next::Stop);
    }
}
