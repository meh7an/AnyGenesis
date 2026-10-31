//! The builder window, drawn after plan/design (DESIGN.md and the prototype): a 44 px
//! header with the steps, the step pages, a 56 px bottom bar, and the Settings and About
//! dialogs. All state lives in [`Model`]; this file only draws it and forwards input.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::{
    Animation, AnimationExt, AppContext, ClickEvent, ClipboardItem, Context, Decorations, Div,
    ElementId, ExternalPaths, FontWeight, HighlightStyle, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, PathPromptOptions, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, StyledText, Svg, Window, WindowAppearance,
    WindowControlArea, canvas, div, ease_out_quint, fill, img, point, prelude::FluentBuilder, px,
    size, svg,
};

use anygenesis_patch::market::{self, IndexPack, Status};

use crate::{
    child::{self, Child},
    model::{
        Build, LogLine, Market, Model, NextAction, Notice, Origin, Overlay, Pane, RomState, Step,
        StepState, Tab,
    },
    progress::{self, Burst, PAD, PixelBar},
    settings::{self, Settings, ThemeChoice},
    theme::{Theme, Tone},
};

const SANS: &str = "IBM Plex Sans";
const MONO: &str = "IBM Plex Mono";
/// Window width from which the Patches tab shows both panes.
const TWO_PANES: f32 = 1040.0;
const POLL: Duration = Duration::from_millis(50);

pub struct Builder {
    model: Model,
    system_dark: bool,
    system_reduce: bool,
    child: Option<Child>,
    bar: Rc<RefCell<PixelBar>>,
    burst: Option<Burst>,
    epoch: Instant,
    last_frame: Instant,
    log_scroll: ScrollHandle,
    log_lines: usize,
    /// Counts step changes, so each change replays the page animation.
    page: usize,
    /// +1 when the last step change went forward, -1 back.
    direction: f32,
    /// Counts finished builds, so each one replays the Play fill.
    finished: usize,
    /// Set by a header press, so the next mouse move drags the window (Linux).
    drag_pending: bool,
}

impl Builder {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_appearance(window, |this, window, cx| {
            this.system_dark = is_dark(window.appearance());
            cx.notify();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let now = Instant::now();
        let mut model = Model::new(Settings::load());
        model.market_root = settings::market_folder();
        Self {
            model,
            system_dark: is_dark(window.appearance()),
            system_reduce: cx.reduce_motion(),
            child: None,
            bar: Rc::default(),
            burst: None,
            epoch: now,
            last_frame: now,
            log_scroll: ScrollHandle::new(),
            log_lines: 0,
            page: 0,
            direction: 1.0,
            finished: 0,
            drag_pending: false,
        }
    }

    fn theme(&self) -> Theme {
        let dark = match self.model.settings.theme {
            ThemeChoice::System => self.system_dark,
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        };
        if dark { Theme::dark() } else { Theme::light() }
    }

    fn reduce_motion(&self) -> bool {
        self.model
            .settings
            .reduce_motion
            .unwrap_or(self.system_reduce)
    }

    fn now_ms(&self) -> f32 {
        self.epoch.elapsed().as_secs_f32() * 1000.0
    }

    /// Reads the running build's output and notices its end.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(child) = &mut self.child else {
            return;
        };
        let now = Instant::now();
        let lines = child.lines();
        let exit = child.exit();
        let changed = !lines.is_empty() || exit.is_some();
        for line in &lines {
            self.model.on_line(line, now);
        }
        if let Some((code, rest)) = exit {
            for line in &rest {
                self.model.on_line(line, now);
            }
            self.model.on_exit(code, now);
            self.child = None;
        }
        if matches!(self.model.build, Build::Done { .. }) && self.child.is_none() {
            self.finished += 1;
            if !self.reduce_motion() {
                self.burst = Some(Burst::new(self.finished as u64, self.now_ms()));
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        if step != self.model.step && self.model.can_go(step) {
            self.direction = if step > self.model.step { 1.0 } else { -1.0 };
            self.page += 1;
        }
        self.model.go(step);
        if self.model.step == Step::Patches && matches!(self.model.market, Market::Idle) {
            self.fetch_market(cx);
        }
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let step = self.model.step;
        self.model.back();
        if self.model.step != step {
            self.direction = -1.0;
            self.page += 1;
        }
        cx.notify();
    }

    fn load_rom(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.model.load_rom(&path, &self.folders());
        self.burst = None;
        cx.notify();
    }

    /// The folders scanned for packs: `patches/` next to the app, then the Marketplace's.
    fn folders(&self) -> Vec<(PathBuf, Origin)> {
        let local = child::patches_folder().map(|folder| (folder, Origin::Folder));
        let market = self
            .model
            .market_root
            .clone()
            .map(|root| (root, Origin::Market));
        local.into_iter().chain(market).collect()
    }

    /// Fetches the Marketplace index on a background thread.
    fn fetch_market(&mut self, cx: &mut Context<Self>) {
        self.model.market = Market::Loading;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { market::fetch_index(&*crate::market::source()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.model.market = match result {
                    Ok(index) => Market::Ready(index),
                    Err(error) => Market::Offline(error.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn ask_install(&mut self, pack: IndexPack, cx: &mut Context<Self>) {
        self.model.confirm = Some(pack);
        self.set_overlay(Some(Overlay::Install), cx);
    }

    /// Installs the confirmed pack on a background thread, then lists it.
    fn install_pack(&mut self, cx: &mut Context<Self>) {
        self.model.overlay = None;
        let (Some(pack), Some(root), Market::Ready(index)) = (
            self.model.confirm.take(),
            self.model.market_root.clone(),
            &self.model.market,
        ) else {
            cx.notify();
            return;
        };
        let index = index.clone();
        let name = pack.name.clone();
        self.model.installing = Some(pack.id.clone());
        self.model.market_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            market::install(&root, &index, &pack, &*crate::market::source()).map(|_| ())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.model.installing = None;
                if let Err(error) = result {
                    this.model.market_error =
                        Some(format!("{name} couldn't be installed: {error}."));
                }
                let folders = this.folders();
                this.model.rescan(&folders);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_pack(&mut self, pack: &IndexPack, cx: &mut Context<Self>) {
        let Some(root) = self.model.market_root.clone() else {
            return;
        };
        self.model.market_error = market::remove(&root, pack)
            .err()
            .map(|error| format!("{} couldn't be removed: {error}.", pack.name));
        let folders = self.folders();
        self.model.rescan(&folders);
        cx.notify();
    }

    fn pick_rom(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Pick a ROM file".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(path) = paths.pop()
            {
                let _ = this.update(cx, |this, cx| this.load_rom(path, cx));
            }
        })
        .detach();
    }

    fn pick_patches(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add patch".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| {
                    for path in &paths {
                        this.model.add_patch(path);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn pick_games_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(path) = paths.pop()
            {
                let _ = this.update(cx, |this, cx| {
                    this.model.settings.games = Some(path);
                    this.model.settings.save();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn drop_paths(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let paths = paths.paths();
        if self.model.step == Step::Patches && self.model.rom().is_some() {
            for path in paths {
                self.model.add_patch(path);
            }
            cx.notify();
        } else if let Some(path) = paths.first() {
            self.load_rom(path.clone(), cx);
        }
    }

    fn start_build(&mut self, cx: &mut Context<Self>) {
        if self.model.is_building() {
            return;
        }
        let now = Instant::now();
        self.model.start_build(now);
        self.burst = None;
        let spawned = child::anygenesis()
            .and_then(|program| Child::spawn(&program, &self.model.build_arguments()));
        match spawned {
            Ok(child) => self.child = Some(child),
            Err(error) => {
                self.model
                    .on_line(&format!("ag-error starting anygenesis: {error}"), now);
                self.model.on_exit(None, now);
            }
        }
        cx.notify();
    }

    fn cancel_build(&mut self, cx: &mut Context<Self>) {
        if let Some(child) = self.child.take() {
            child.kill();
        }
        self.model.cancel_build();
        cx.notify();
    }

    fn play(&mut self) {
        if let Build::Done { exe, .. } = &self.model.build
            && let Err(error) = child::launch(exe)
        {
            self.model.log.push(LogLine {
                text: format!("starting {}: {error}", exe.display()),
                error: true,
            });
        }
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        match self.model.next().action {
            Some(NextAction::Go(step)) => self.go(step, cx),
            Some(NextAction::Build) => self.start_build(cx),
            Some(NextAction::Play) => self.play(),
            None => {}
        }
    }

    fn set_overlay(&mut self, overlay: Option<Overlay>, cx: &mut Context<Self>) {
        self.model.overlay = overlay;
        cx.notify();
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// A Phosphor icon from the bundled assets, `size` px square, in `color`.
fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// A filled primary button.
fn primary(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Theme,
) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(16.))
        .py(px(7.))
        .rounded(px(4.))
        .bg(t.primary)
        .text_color(t.on_primary)
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|style| style.opacity(0.92))
        .child(label.into())
}

/// The disabled look of a primary button.
fn primary_off(label: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .px(px(16.))
        .py(px(7.))
        .rounded(px(4.))
        .bg(t.track)
        .text_color(t.muted)
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into())
}

/// An outlined button with `size` px text.
fn secondary(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    size: f32,
    t: &Theme,
) -> gpui_kit::Stateful<Div> {
    let (x, y) = if size < 13.0 { (10., 4.) } else { (14., 6.) };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .px(px(x))
        .py(px(y))
        .rounded(px(4.))
        .border_1()
        .border_color(t.control)
        .bg(t.surface)
        .text_color(t.text)
        .text_size(px(size))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .child(label.into())
}

/// An underlined text button.
fn link(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    size: f32,
    color: Hsla,
) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .text_size(px(size))
        .text_color(color)
        .underline()
        .cursor_pointer()
        .child(label.into())
}

/// A bold lead followed by plain text, wrapping as one paragraph.
fn lead_text(notice: &Notice) -> StyledText {
    let text = if notice.rest.is_empty() {
        notice.lead.clone()
    } else {
        format!("{} {}", notice.lead, notice.rest)
    };
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    StyledText::new(text).with_highlights([(0..notice.lead.len(), bold)])
}

/// A notice box in `tone` with a 16 px icon.
fn notice(tone: Tone, icon_name: &str, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_none()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(4.))
        .border_1()
        .border_color(tone.border)
        .bg(tone.bg)
        .text_color(tone.text)
        .line_height(px(13. * 1.45))
        .child(div().pt(px(1.)).child(icon(icon_name, 16., tone.text)))
        .child(div().flex_1().min_w_0().child(content))
}

/// The red error card with a title, a message and an optional button.
fn error_card(t: &Theme, title: &str, message: &str, action: Option<gpui_kit::AnyElement>) -> Div {
    div()
        .flex()
        .flex_none()
        .gap(px(12.))
        .px(px(16.))
        .py(px(14.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.error.border)
        .bg(t.error.bg)
        .text_color(t.error.text)
        .child(icon("warning-circle", 20., t.error.text))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(4.))
                .line_height(px(13. * 1.45))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(message.to_owned()),
        )
        .children(action.map(|action| div().flex_none().self_center().child(action)))
}

/// A card: surface, border, 6 px corners.
fn card(t: &Theme) -> Div {
    div()
        .flex_none()
        .border_1()
        .border_color(t.border)
        .rounded(px(6.))
        .bg(t.surface)
}

/// A 32 × 18 switch.
fn switch(on: bool, t: &Theme) -> Div {
    div()
        .w(px(32.))
        .h(px(18.))
        .flex_none()
        .flex()
        .items_center()
        .px(px(2.))
        .rounded(px(9.))
        .border_1()
        .border_color(if on { t.primary } else { t.control })
        .bg(if on { t.primary } else { t.surface })
        .when(on, |this| this.justify_end())
        .child(
            div()
                .size(px(12.))
                .rounded(px(6.))
                .bg(if on { t.on_primary } else { t.control }),
        )
}

/// What is dragged when a patch row moves.
#[derive(Clone)]
struct DraggedPatch {
    index: usize,
    name: SharedString,
    theme: Theme,
}

impl Render for DraggedPatch {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(t.control)
            .bg(t.surface)
            .text_color(t.text)
            .font_family(SANS)
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .child(self.name.clone())
    }
}

impl Builder {
    fn header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        t: &Theme,
    ) -> gpui_kit::AnyElement {
        let is_windows = cfg!(target_os = "windows");
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        let steps = Step::ALL.iter().map(|&step| {
            let state = self.model.step_state(step);
            let number = (step.index() + 1).to_string();
            let badge = div()
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .text_size(px(11.));
            let badge = match state {
                StepState::Current => badge.bg(t.primary).text_color(t.on_primary).child(number),
                StepState::Done => {
                    badge
                        .bg(t.step_done)
                        .child(icon("check-bold", 12., t.on_step_done))
                }
                StepState::Todo { .. } => badge.border_1().border_color(t.control).child(number),
            };
            let locked = matches!(state, StepState::Todo { locked: true });
            let button = div()
                .id(("step", step.index()))
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(4.))
                .text_size(px(13.))
                .occlude()
                .child(badge)
                .child(step.label())
                .map(|this| match state {
                    StepState::Current => this
                        .bg(t.pill)
                        .text_color(t.text)
                        .font_weight(FontWeight::SEMIBOLD),
                    _ if locked => this.text_color(t.text2).opacity(0.5),
                    _ => this
                        .text_color(t.text2)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.hover)),
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(step, cx)));
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(step != Step::Rom, |this| {
                    this.child(div().w(px(1.)).h(px(16.)).bg(t.control))
                })
                .child(button)
        });
        let control = |id: &'static str,
                       area: WindowControlArea,
                       content: gpui_kit::AnyElement,
                       hover: Hsla| {
            div()
                .id(id)
                .w(px(46.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(move |style| style.bg(hover))
                .occlude()
                .when(is_windows, |this| this.window_control_area(area))
                .when(!is_windows, |this| {
                    this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        match area {
                            WindowControlArea::Min => window.minimize_window(),
                            WindowControlArea::Max => window.zoom_window(),
                            _ => window.remove_window(),
                        }
                    })
                })
                .child(content)
        };
        let controls = (is_windows || client_decorated).then(|| {
            div()
                .flex()
                .h_full()
                .flex_none()
                .child(control(
                    "minimize",
                    WindowControlArea::Min,
                    div().w(px(10.)).h(px(1.)).bg(t.text2).into_any_element(),
                    t.hover,
                ))
                .child(control(
                    "maximize",
                    WindowControlArea::Max,
                    div()
                        .size(px(9.))
                        .border_1()
                        .border_color(t.text2)
                        .into_any_element(),
                    t.hover,
                ))
                .child(control(
                    "close",
                    WindowControlArea::Close,
                    icon("x", 14., t.text2).into_any_element(),
                    t.close_hover,
                ))
        });
        div()
            .id("header")
            .relative()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(12.))
            .border_b_1()
            .border_color(t.border)
            .bg(t.chrome)
            .when(is_windows, |this| {
                this.window_control_area(WindowControlArea::Drag)
            })
            .when(!is_windows, |this| {
                this.on_click(|event: &ClickEvent, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.drag_pending = true),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.drag_pending = false),
                )
                .on_mouse_move(cx.listener(|this, _, window, _| {
                    if this.drag_pending {
                        this.drag_pending = false;
                        window.start_window_move();
                    }
                }))
            })
            // Centred on the whole header, not on the space between the unequal side groups.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap(px(8.))
                    .children(steps),
            )
            .child(img("icon.png").size(px(20.)).rounded(px(4.)).flex_none())
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_nowrap()
                    .child("AnyGenesis Builder"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("settings")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .text_size(px(12.))
                    .text_color(t.text2)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .occlude()
                    .child(icon("gear-six", 16., t.text2))
                    .child("Settings")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.set_overlay(Some(Overlay::Settings), cx)
                    })),
            )
            .child(
                div()
                    .id("about")
                    .flex()
                    .p(px(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .occlude()
                    .child(icon("info", 16., t.text2))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.set_overlay(Some(Overlay::About), cx)
                    })),
            )
            .children(controls)
            .into_any_element()
    }

    fn rom_page(&mut self, cx: &mut Context<Self>, t: &Theme) -> gpui_kit::AnyElement {
        match &self.model.rom {
            RomState::Empty => {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .px(px(28.))
                    .py(px(24.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(10.))
                            .p(px(24.))
                            .rounded(px(8.))
                            .border_2()
                            .border_dashed()
                            .border_color(t.control)
                            .bg(t.surface)
                            .text_center()
                            .child(icon("upload-simple", 40., t.primary))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Insert cartridge"),
                            )
                            .child(
                                div()
                                    .max_w(px(420.))
                                    .text_color(t.text2)
                                    .line_height(px(13. * 1.5))
                                    .child(
                                        "Drag in your Sonic, Streets of Rage or any other Genesis \
                                     ROM and we'll take it from there.",
                                    ),
                            )
                            .child(
                                div().mt(px(4.)).child(
                                    primary("pick-rom", "Pick a ROM file", t)
                                        .py(px(8.))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.pick_rom(cx)
                                        })),
                                ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(t.muted)
                                    .child(".bin, .md or .gen"),
                            ),
                    )
                    .into_any_element()
            }
            RomState::Error {
                title,
                message,
                file,
            } => div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(12.))
                .px(px(28.))
                .py(px(24.))
                .child(error_card(t, title, message, None))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child(file.clone()),
                )
                .child(
                    div().flex().child(
                        secondary("try-another", "Try another file", 13., t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_rom(cx))),
                    ),
                )
                .into_any_element(),
            RomState::Loaded(rom) => {
                let found = self
                    .model
                    .patches
                    .iter()
                    .filter(|patch| patch.origin == Origin::Folder)
                    .count();
                let sha1 = rom.sha1.to_string();
                let fields = rom.fields.iter().map(|(label, value)| {
                    [
                        div()
                            .text_color(t.muted)
                            .whitespace_nowrap()
                            .child(*label)
                            .into_any_element(),
                        div()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .child(value.clone())
                            .into_any_element(),
                    ]
                });
                let mut grid = div()
                    .grid()
                    .grid_cols(4)
                    .gap_x(px(12.))
                    .gap_y(px(8.))
                    .text_size(px(12.));
                for cells in fields {
                    grid = grid.children(cells);
                }
                div()
                    .id("rom-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(28.))
                    .py(px(20.))
                    .child(
                        card(t).flex().gap(px(20.)).px(px(18.)).py(px(16.)).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .items_start()
                                        .gap(px(12.))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap(px(2.))
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .text_size(px(20.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(rom.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(12.))
                                                        .text_color(t.muted)
                                                        .child(rom.file_name.clone()),
                                                ),
                                        )
                                        .child(
                                            secondary("change-rom", "Change ROM\u{2026}", 12., t)
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| this.pick_rom(cx),
                                                )),
                                        ),
                                )
                                .child(grid)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.))
                                        .pt(px(10.))
                                        .border_t_1()
                                        .border_color(t.row)
                                        .text_size(px(12.))
                                        .child(div().text_color(t.muted).child("SHA-1"))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .font_family(MONO)
                                                .text_size(px(11.))
                                                .child(sha1.clone()),
                                        )
                                        .child(
                                            secondary("copy-sha1", "Copy", 12., t)
                                                .py(px(3.))
                                                .child(icon("copy", 14., t.text2))
                                                .flex_row_reverse()
                                                .on_click(move |_, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(sha1.clone()),
                                                    )
                                                }),
                                        ),
                                ),
                        ),
                    )
                    .children(
                        rom.warnings
                            .iter()
                            .map(|warning| notice(t.warning, "warning", lead_text(warning))),
                    )
                    .when(found > 0, |this| {
                        let lead = format!(
                            "{found} {} for this game {} in your patches folder.",
                            if found == 1 { "patch" } else { "patches" },
                            if found == 1 { "is" } else { "are" },
                        );
                        this.child(
                            notice(
                                t.info,
                                "info",
                                lead_text(&Notice {
                                    lead,
                                    rest: String::new(),
                                }),
                            )
                            .items_center()
                            .child(
                                link("take-a-look", "Take a look", 12., t.info.text).on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.go(Step::Patches, cx)
                                    }),
                                ),
                            ),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn patches_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        t: &Theme,
    ) -> gpui_kit::AnyElement {
        let wide = window.viewport_size().width >= px(TWO_PANES);
        let tab = self.model.tab;
        let tabs = [(Tab::Patches, "Patches"), (Tab::Mods, "Mods")].map(|(id, label)| {
            let on = tab == id;
            div()
                .id(label)
                .pt(px(6.))
                .pb(px(7.))
                .border_b_2()
                .border_color(if on {
                    t.primary
                } else {
                    gpui_kit::transparent_black()
                })
                .text_size(px(13.))
                .cursor_pointer()
                .map(|this| {
                    if on {
                        this.text_color(t.text).font_weight(FontWeight::SEMIBOLD)
                    } else {
                        this.text_color(t.text2)
                            .hover(|style| style.text_color(t.text))
                    }
                })
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.model.tab = id;
                    cx.notify();
                }))
        });
        let pane = self.model.pane;
        let switcher = (!wide && tab == Tab::Patches).then(|| {
            let count = self.model.patches.len();
            let options = [
                (Pane::Local, format!("On this computer \u{b7} {count}")),
                (Pane::Marketplace, "Marketplace".to_owned()),
            ];
            div()
                .flex()
                .mb(px(6.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.control)
                .overflow_hidden()
                .text_size(px(12.))
                .children(options.into_iter().map(|(id, label)| {
                    segment(("pane", id as usize), label, pane == id, t).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.model.pane = id;
                            cx.notify();
                        },
                    ))
                }))
        });
        let bar = div()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(20.))
            .px(px(24.))
            .pt(px(12.))
            .border_b_1()
            .border_color(t.border)
            .children(tabs)
            .child(div().flex_1())
            .children(switcher);
        let body = match tab {
            Tab::Mods => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(24.))
                .text_center()
                .child(icon("puzzle-piece", 36., t.muted))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Mods are coming later"),
                )
                .child(
                    div()
                        .max_w(px(380.))
                        .text_color(t.text2)
                        .line_height(px(13. * 1.5))
                        .child(
                            "Mods will let you add new stuff on top of your game. We're still \
                             building them, so check back after an update.",
                        ),
                )
                .into_any_element(),
            Tab::Patches => {
                let show_local = wide || pane == Pane::Local;
                let show_market = wide || pane == Pane::Marketplace;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(show_local, |this| this.child(self.local_pane(wide, cx, t)))
                    .when(show_market, |this| {
                        this.child(self.market_pane(wide, cx, t))
                    })
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    fn local_pane(
        &mut self,
        wide: bool,
        cx: &mut Context<Self>,
        t: &Theme,
    ) -> gpui_kit::AnyElement {
        let rows = self.model.patches.iter().enumerate().map(|(index, patch)| {
            let order = self.model.order(index);
            let dragged = DraggedPatch {
                index,
                name: patch.name.clone().into(),
                theme: *t,
            };
            let pill = t.pill;
            div()
                .id(("patch", index))
                .h(px(44.))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .bg(t.surface)
                .when(index > 0, |this| this.border_t_1().border_color(t.row))
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedPatch>(move |style, _, _, _| style.bg(pill))
                .on_drop(cx.listener(move |this, dragged: &DraggedPatch, _, cx| {
                    this.model.move_patch(dragged.index, index);
                    cx.notify();
                }))
                .child(
                    div()
                        .cursor_grab()
                        .child(icon("dots-six-vertical", 16., t.control)),
                )
                .child(
                    div()
                        .id(("check", index))
                        .size(px(16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .border_1()
                        .border_color(if patch.on { t.primary } else { t.control })
                        .bg(if patch.on { t.primary } else { t.surface })
                        .cursor_pointer()
                        .when(patch.on, |this| {
                            this.child(icon("check-bold", 12., t.on_primary))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.model.toggle_patch(index);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .text_color(if patch.on { t.text } else { t.muted })
                                .child(patch.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(t.muted)
                                .truncate()
                                .child(patch.meta()),
                        ),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .child(order.map_or("\u{2013}".to_owned(), |order| order.to_string())),
                )
                .child(
                    div()
                        .id(("remove", index))
                        .flex()
                        .p(px(6.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.hover))
                        .child(icon("trash", 16., t.text2))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.model.remove_patch(index);
                            cx.notify();
                        })),
                )
        });
        let trusted: Vec<String> = self
            .model
            .enabled_patches()
            .filter(|patch| patch.runs_code)
            .map(|patch| patch.name.clone())
            .collect();
        let has_patches = !self.model.patches.is_empty();
        div()
            .id("local-pane")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(10.))
            .pt(px(14.))
            .pb(px(14.))
            .pl(px(24.))
            .pr(px(if wide { 16. } else { 24. }))
            .when(wide, |this| this.border_r_1().border_color(t.border))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("On this computer"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(t.muted)
                                    .child("Patches go on top first. Drag to change the order."),
                            ),
                    )
                    .child(
                        secondary("add-file", "Add from file\u{2026}", 12., t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.pick_patches(cx)),
                        ),
                    ),
            )
            .children(self.model.patch_error.clone().map(|message| {
                notice(
                    t.error,
                    "warning-circle",
                    lead_text(&Notice {
                        lead: message,
                        rest: String::new(),
                    }),
                )
            }))
            .when(has_patches, |this| {
                this.child(card(t).overflow_hidden().children(rows))
            })
            .when(!has_patches, |this| {
                this.child(empty_box(
                    t,
                    "No patches yet. Add a file, get one from the Marketplace, or drop packs \
                     into the patches folder next to the app.",
                ))
            })
            .when(!trusted.is_empty(), |this| {
                this.child(notice(
                    t.trust,
                    "shield-warning",
                    lead_text(&Notice {
                        lead: "Heads up: these patches run their own native code.".into(),
                        rest: format!(
                            "{}. Build with them only if you trust their authors, like any program.",
                            trusted.join(", ")
                        ),
                    }),
                ))
            })
            .into_any_element()
    }

    fn build_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        t: &Theme,
    ) -> gpui_kit::AnyElement {
        let Some(rom) = self.model.rom() else {
            return div().into_any_element();
        };
        let title = rom.title.clone();
        let exe_name = self.model.exe_name();
        let games = self.model.settings.games_label();
        let reduce = self.reduce_motion();
        let now = Instant::now();
        let step = now.saturating_duration_since(self.last_frame);
        self.last_frame = now;
        let now_ms = self.now_ms();
        let summary = card(t)
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(16.))
            .py(px(12.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.text2)
                            .truncate()
                            .child(self.model.patch_summary()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .items_end()
                    .min_w_0()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .whitespace_nowrap()
                            .child(exe_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .child(format!("in {games}")),
                    ),
            );
        let state: gpui_kit::AnyElement =
            match &mut self.model.build {
                Build::Ready => {
                    card(t)
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(10.))
                        .p(px(20.))
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Ready when you are"),
                        )
                        .child(div().text_color(t.text2).line_height(px(13. * 1.5)).child(
                            ready_text(self.model.patches.iter().filter(|p| p.on).count()),
                        ))
                        .child(div().mt(px(4.)).child(
                            primary("build-game", "Build game", t).py(px(8.)).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.start_build(cx)),
                            ),
                        ))
                        .into_any_element()
                }
                Build::Running { progress, started } => {
                    let fraction = progress.advance(now.saturating_duration_since(*started), step);
                    let elapsed = progress::clock(now.saturating_duration_since(*started));
                    let title = progress.title();
                    let steps = progress.steps();
                    let bar = self.bar.clone();
                    let color = t.primary;
                    window.request_animation_frame();
                    card(t)
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .p(px(16.))
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(20.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(13.))
                                        .text_color(t.text2)
                                        .child(elapsed),
                                ),
                        )
                        .child(
                            div().h(px(12.)).relative().bg(t.track).child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, (), window, _| {
                                        let width = f32::from(bounds.size.width);
                                        let rects =
                                            bar.borrow_mut().frame(width, fraction, now_ms, reduce);
                                        paint_rects(window, bounds.origin, &rects, color);
                                    },
                                )
                                .absolute()
                                .left_0()
                                .top(px(-PAD))
                                .w_full()
                                .h(px(12. + 2. * PAD)),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .text_size(px(12.))
                                .text_color(t.muted)
                                .child(div().flex_1().child(steps))
                                .child(
                                    secondary("cancel", "Cancel build", 13., t)
                                        .py(px(5.))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.cancel_build(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Build::Done { exe, took } => {
                    let exe_file = exe
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or(exe_name);
                    let burst = self.burst.as_ref().filter(|burst| burst.is_running(now_ms));
                    let sparks: Vec<progress::Rect> = burst
                        .map(|burst| burst.frame(0.0, now_ms))
                        .unwrap_or_default();
                    if burst.is_some() {
                        window.request_animation_frame();
                    }
                    let color = t.primary;
                    let exe = exe.clone();
                    card(t)
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .p(px(16.))
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .text_size(px(20.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(icon("check-bold", 20., t.success))
                                        .child("Game on!"),
                                )
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(13.))
                                        .text_color(t.text2)
                                        .child(progress::clock(*took)),
                                ),
                        )
                        .child(
                            div().h(px(12.)).relative().bg(t.primary).child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, (), window, _| {
                                        let origin = point(
                                            bounds.origin.x + bounds.size.width,
                                            bounds.origin.y,
                                        );
                                        paint_rects(window, origin, &sparks, color);
                                    },
                                )
                                .absolute()
                                .left_0()
                                .top(px(-PAD))
                                .w_full()
                                .h(px(12. + 2. * PAD)),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_x(px(4.))
                                .text_color(t.text2)
                                .line_height(px(13. * 1.5))
                                .child(format!("{title} is built."))
                                .child(div().font_family(MONO).text_size(px(12.)).child(exe_file))
                                .child(format!("is in {games}.")),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    primary("press-start", "Press Start", t)
                                        .py(px(8.))
                                        .flex_row_reverse()
                                        .child(icon("play-fill", 14., t.on_primary))
                                        .on_click(
                                            cx.listener(|this, _: &ClickEvent, _, _| this.play()),
                                        ),
                                )
                                .child(
                                    secondary("show-folder", "Show in folder", 13., t)
                                        .py(px(7.))
                                        .flex_row_reverse()
                                        .child(icon("folder-open", 14., t.text2))
                                        .on_click(move |_, _, cx| cx.reveal_path(&exe)),
                                ),
                        )
                        .into_any_element()
                }
                Build::Failed { .. } => {
                    let text = self.model.failure_text().unwrap_or_default();
                    error_card(
                        t,
                        "The build hit a snag",
                        &text,
                        Some(
                            secondary("try-again", "Try again", 13., t)
                                .flex_row_reverse()
                                .child(icon("arrow-clockwise", 14., t.text2))
                                .on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| this.start_build(cx)),
                                )
                                .into_any_element(),
                        ),
                    )
                    .into_any_element()
                }
            };
        let show_log = !matches!(self.model.build, Build::Ready);
        if self.model.log.len() != self.log_lines {
            self.log_lines = self.model.log.len();
            self.log_scroll.scroll_to_bottom();
        }
        let log_open = self.model.log_open;
        let log = show_log.then(|| {
            let log_text = self.model.log_text();
            card(t)
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .bg(t.surface)
                        .when(log_open, |this| this.border_b_1().border_color(t.border))
                        .child(
                            div()
                                .id("toggle-log")
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .py(px(2.))
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .child(icon("caret-down", 14., t.text2).with_transformation(
                                    gpui_kit::Transformation::rotate(gpui_kit::radians(
                                        if log_open {
                                            0.0
                                        } else {
                                            -std::f32::consts::FRAC_PI_2
                                        },
                                    )),
                                ))
                                .child("Build log")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.model.log_open = !this.model.log_open;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("copy-log")
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .px(px(4.))
                                .py(px(2.))
                                .text_size(px(12.))
                                .text_color(t.text2)
                                .cursor_pointer()
                                .child(icon("copy", 14., t.text2))
                                .child("Copy log")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        log_text.clone(),
                                    ))
                                }),
                        ),
                )
                .when(log_open, |this| {
                    this.child(
                        div()
                            .id("log")
                            .track_scroll(&self.log_scroll)
                            .overflow_y_scroll()
                            .max_h(px(132.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .px(px(12.))
                            .py(px(8.))
                            .bg(t.log)
                            .font_family(MONO)
                            .text_size(px(11.))
                            .children(self.model.log.iter().map(|line| {
                                div()
                                    .text_color(if line.error { t.error.text } else { t.text2 })
                                    .child(line.text.clone())
                            })),
                    )
                })
        });
        div()
            .id("build-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(28.))
            .py(px(20.))
            .child(summary)
            .child(state)
            .children(log)
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child("Your build includes your ROM, so keep it to yourself."),
            )
            .into_any_element()
    }

    fn bottom_bar(&mut self, cx: &mut Context<Self>, t: &Theme) -> gpui_kit::AnyElement {
        let can_back = self.model.step != Step::Rom;
        let next = self.model.next();
        let finished = self.finished;
        let primary_button = match next.action {
            Some(action) => {
                let button = primary("next", next.label, t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next(cx)));
                if action == NextAction::Play && !self.reduce_motion() {
                    let (from, to) = (t.track, t.primary);
                    button
                        .with_animation(
                            ("play-fill", finished),
                            Animation::new(Duration::from_millis(240)),
                            move |button, delta| button.bg(from.blend(to.opacity(delta))),
                        )
                        .into_any_element()
                } else {
                    button.into_any_element()
                }
            }
            None => primary_off(next.label, t).into_any_element(),
        };
        div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(28.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.chrome)
            .child(if can_back {
                secondary("back", "Back", 13., t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.back(cx)))
                    .into_any_element()
            } else {
                div()
                    .px(px(14.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.border)
                    .text_color(t.muted)
                    .child("Back")
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_center()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .truncate()
                    .child(self.model.status()),
            )
            .child(primary_button)
            .into_any_element()
    }

    fn settings_dialog(&mut self, cx: &mut Context<Self>, t: &Theme) -> gpui_kit::AnyElement {
        let settings = &self.model.settings;
        let reduce = self.reduce_motion();
        let themes = div()
            .flex()
            .self_start()
            .rounded(px(4.))
            .border_1()
            .border_color(t.control)
            .overflow_hidden()
            .children(ThemeChoice::ALL.into_iter().map(|choice| {
                segment(
                    ("theme", choice as usize),
                    choice.label(),
                    settings.theme == choice,
                    t,
                )
                .px(px(14.))
                .py(px(6.))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.model.settings.theme = choice;
                    this.model.settings.save();
                    cx.notify();
                }))
            }));
        let section = |title: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        };
        dialog(t, 520.)
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(20.))
                    .py(px(16.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .id("close-settings")
                            .flex()
                            .p(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.hover))
                            .child(icon("x", 16., t.text2))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.set_overlay(None, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .px(px(20.))
                    .py(px(16.))
                    .child(section("Appearance").child(themes))
                    .child(
                        section("Where games go").child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .px(px(10.))
                                        .py(px(6.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(t.border)
                                        .bg(t.surface)
                                        .font_family(MONO)
                                        .text_size(px(12.))
                                        .truncate()
                                        .child(settings.games_folder().display().to_string()),
                                )
                                .child(
                                    secondary("change-games", "Change\u{2026}", 12., t)
                                        .px(px(12.))
                                        .py(px(5.))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.pick_games_folder(cx)
                                        })),
                                ),
                        ),
                    )
                    .child(
                        section("Motion").child(
                            div()
                                .id("reduce-motion")
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .py(px(2.))
                                .cursor_pointer()
                                .child(switch(reduce, t))
                                .child("Reduce motion")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.model.settings.reduce_motion = Some(!reduce);
                                    this.model.settings.save();
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(section("Marketplace").child(
                        div().text_size(px(12.)).text_color(t.text2).child(
                            self.model.market_root.as_ref().map_or_else(
                                || "The Marketplace needs a per-user folder.".to_owned(),
                                |root| {
                                    format!(
                                        "Patches from AnyGenesis-Patches on GitHub, checked by \
                                         SHA-256, installed in {}.",
                                        root.display()
                                    )
                                },
                            ),
                        ),
                    ))
                    .child(
                        section("Language").child(
                            div()
                                .text_size(px(12.))
                                .text_color(t.text2)
                                .child("English. Persian, Arabic and Hebrew are on the way."),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .px(px(20.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.chrome)
                    .child(primary("settings-done", "Done", t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.set_overlay(None, cx)),
                    )),
            )
            .into_any_element()
    }

    fn about_dialog(&mut self, cx: &mut Context<Self>, t: &Theme) -> gpui_kit::AnyElement {
        dialog(t, 420.)
            .items_center()
            .gap(px(10.))
            .p(px(24.))
            .text_center()
            .child(img("icon.png").size(px(72.)).rounded(px(16.)))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("AnyGenesis Builder"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div().text_color(t.text2).line_height(px(13. * 1.5)).child(
                    "Turns your own Genesis cartridge dump into a game you can run on your PC.",
                ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child("Your build includes your ROM, so keep it to yourself."),
            )
            .child(
                div().flex().gap(px(16.)).child(
                    link("source", "Source code", 12., t.primary)
                        .on_click(|_, _, cx| cx.open_url("https://github.com/meh7an/AnyGenesis")),
                ),
            )
            .child(div().mt(px(6.)).child(
                primary("about-done", "Done", t).px(px(20.)).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.set_overlay(None, cx)),
                ),
            ))
            .into_any_element()
    }
}

/// One option of a segmented control.
fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on: bool,
    t: &Theme,
) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .px(px(12.))
        .py(px(5.))
        .text_size(px(12.))
        .cursor_pointer()
        .map(|this| {
            if on {
                this.bg(t.primary)
                    .text_color(t.on_primary)
                    .font_weight(FontWeight::SEMIBOLD)
            } else {
                this.bg(t.surface)
                    .text_color(t.text2)
                    .hover(|style| style.bg(t.hover))
            }
        })
        .child(label.into())
}

/// A dialog box `width` px wide.
fn dialog(t: &Theme, width: f32) -> gpui_kit::Stateful<Div> {
    div()
        .id("dialog")
        .w(px(width))
        .max_h_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .rounded(px(8.))
        .border_1()
        .border_color(t.window_border)
        .bg(t.bg)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A dashed box with a centred message, for an empty or waiting list.
fn empty_box(t: &Theme, message: impl Into<SharedString>) -> Div {
    div()
        .p(px(20.))
        .rounded(px(6.))
        .border_1()
        .border_dashed()
        .border_color(t.control)
        .text_center()
        .text_color(t.text2)
        .line_height(px(13. * 1.5))
        .child(message.into())
}

/// `bytes` as people read file sizes.
fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

impl Builder {
    /// The Marketplace pane: the index's packs for the loaded ROM, with their install state.
    fn market_pane(
        &mut self,
        wide: bool,
        cx: &mut Context<Self>,
        t: &Theme,
    ) -> gpui_kit::AnyElement {
        let entries = self.model.market_entries();
        let installing = self.model.installing.clone();
        let rows = entries.into_iter().enumerate().map(|(index, entry)| {
            let pack = entry.pack;
            let kind = if pack.runs_code {
                "runs native code"
            } else {
                "data only"
            };
            let files = match pack.files.len() {
                1 => "1 file".to_owned(),
                count => format!("{count} files"),
            };
            let state = match entry.status {
                Status::Absent => None,
                Status::Installed { modified: true, .. } => Some("Changed on this computer"),
                Status::Installed { update: true, .. } => Some("Update available"),
                Status::Installed { .. } => Some("Installed"),
            };
            let size = size_text(pack.size());
            let meta = [Some(files.as_str()), Some(size.as_str()), Some(kind), state]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" \u{b7} ");
            let busy = installing.as_deref() == Some(pack.id.as_str());
            let install_label = match entry.status {
                Status::Absent => Some("Install"),
                Status::Installed { update: true, .. } => Some("Update"),
                Status::Installed { modified: true, .. } => Some("Reinstall"),
                Status::Installed { .. } => None,
            };
            let action = if busy {
                div()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child("Installing\u{2026}")
                    .into_any_element()
            } else if installing.is_some() {
                install_label.map_or_else(
                    || div().into_any_element(),
                    |label| primary_off(label, t).into_any_element(),
                )
            } else {
                let ask = pack.clone();
                install_label.map_or_else(
                    || div().into_any_element(),
                    |label| {
                        primary(("install", index), label, t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ask_install(ask.clone(), cx)
                            }))
                            .into_any_element()
                    },
                )
            };
            let installed = entry.status != Status::Absent && !busy;
            let remove = pack.clone();
            div()
                .id(("market", index))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(10.))
                .bg(t.surface)
                .when(index > 0, |this| this.border_t_1().border_color(t.row))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(pack.name.clone()),
                        )
                        .children(pack.description.clone().map(|description| {
                            div()
                                .text_size(px(12.))
                                .text_color(t.text2)
                                .line_height(px(12. * 1.45))
                                .child(description)
                        }))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(t.muted)
                                .truncate()
                                .child(meta),
                        ),
                )
                .child(action)
                .when(installed, |this| {
                    this.child(
                        div()
                            .id(("uninstall", index))
                            .flex()
                            .p(px(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.hover))
                            .child(icon("trash", 16., t.text2))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_pack(&remove, cx)
                            })),
                    )
                })
        });
        let rows: Vec<_> = rows.collect();
        let body = match &self.model.market {
            Market::Idle | Market::Loading => {
                empty_box(t, "Looking for patches for this game\u{2026}").into_any_element()
            }
            Market::Offline(reason) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .p(px(20.))
                .rounded(px(6.))
                .border_1()
                .border_dashed()
                .border_color(t.control)
                .text_center()
                .child(icon("warning-circle", 28., t.muted))
                .child(
                    div().font_weight(FontWeight::MEDIUM).child(
                        "The Marketplace can't be reached. Patches you installed still work.",
                    ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .child(reason.clone()),
                )
                .child(
                    secondary("market-retry", "Try again", 12., t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fetch_market(cx))),
                )
                .into_any_element(),
            Market::Ready(_) if rows.is_empty() => {
                empty_box(t, "No Marketplace patches for this game yet.").into_any_element()
            }
            Market::Ready(_) => card(t).overflow_hidden().children(rows).into_any_element(),
        };
        div()
            .id("market-pane")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(10.))
            .py(px(14.))
            .pr(px(24.))
            .pl(px(if wide { 16. } else { 24. }))
            .bg(t.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Marketplace for this game"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .child("From AnyGenesis-Patches on GitHub \u{b7} matched by SHA-1"),
                    ),
            )
            .child(body)
            .children(self.model.market_error.clone().map(|message| {
                notice(
                    t.error,
                    "warning-circle",
                    lead_text(&Notice {
                        lead: message,
                        rest: String::new(),
                    }),
                )
            }))
            .into_any_element()
    }

    /// What the user reads before a pack is installed: its files, where they come from, and
    /// the native-code notice for a pack with C code.
    fn install_dialog(&mut self, cx: &mut Context<Self>, t: &Theme) -> gpui_kit::AnyElement {
        let (Some(pack), Market::Ready(index)) = (&self.model.confirm, &self.model.market) else {
            return div().into_any_element();
        };
        let files = pack.files.iter().enumerate().map(|(index, file)| {
            div()
                .flex()
                .justify_between()
                .gap(px(12.))
                .px(px(12.))
                .py(px(6.))
                .bg(t.surface)
                .when(index > 0, |this| this.border_t_1().border_color(t.row))
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(12.))
                        .truncate()
                        .child(file.path.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child(size_text(file.size)),
                )
        });
        dialog(t, 460.)
            .gap(px(12.))
            .p(px(24.))
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Install {}?", pack.name)),
            )
            .children(pack.description.clone().map(|description| {
                div()
                    .text_color(t.text2)
                    .line_height(px(13. * 1.5))
                    .child(description)
            }))
            .child(div().text_size(px(11.)).text_color(t.muted).child(format!(
                "{} from AnyGenesis-Patches at commit {}, each file checked by SHA-256",
                pack.id,
                &index.commit[..7]
            )))
            .child(card(t).overflow_hidden().children(files))
            .when(pack.runs_code, |this| {
                this.child(notice(
                    t.trust,
                    "shield-warning",
                    lead_text(&Notice {
                        lead: "Heads up: this pack runs its own native code.".into(),
                        rest: "Its C code is built into your game and runs like any program on \
                               this PC. Install it only if you trust its authors."
                            .into(),
                    }),
                ))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        secondary("install-cancel", "Cancel", 13., t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.model.confirm = None;
                                this.set_overlay(None, cx)
                            },
                        )),
                    )
                    .child(primary("install-confirm", "Install", t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.install_pack(cx)),
                    )),
            )
            .into_any_element()
    }
}

fn ready_text(patches: usize) -> String {
    let apply = match patches {
        0 => "build it as it is".to_owned(),
        1 => "apply 1 patch".to_owned(),
        count => format!("apply {count} patches"),
    };
    format!(
        "We'll read your cartridge, {apply} and turn it into a game for this PC. Builds take one \
         to four minutes; you can keep using your computer."
    )
}

/// Fills `rects` (in canvas coordinates) at `origin` in `color`.
fn paint_rects(
    window: &mut Window,
    origin: gpui_kit::Point<gpui_kit::Pixels>,
    rects: &[progress::Rect],
    color: Hsla,
) {
    for rect in rects {
        let bounds = gpui_kit::Bounds::new(
            point(origin.x + px(rect.x), origin.y + px(rect.y)),
            size(px(rect.w), px(rect.h)),
        );
        window.paint_quad(fill(bounds, color.opacity(rect.alpha)));
    }
}

impl Render for Builder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        let header = self.header(window, cx, &t);
        let page = match self.model.step {
            Step::Rom => self.rom_page(cx, &t),
            Step::Patches => self.patches_page(window, cx, &t),
            Step::Build => self.build_page(window, cx, &t),
        };
        let reduce = self.reduce_motion();
        let direction = self.direction;
        let content = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .relative()
            .child(page);
        let content = if self.page == 0 {
            content.into_any_element()
        } else if reduce {
            content
                .with_animation(
                    ("page", self.page),
                    Animation::new(Duration::from_millis(120)),
                    |content, delta| content.opacity(delta),
                )
                .into_any_element()
        } else {
            content
                .with_animation(
                    ("page", self.page),
                    Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()),
                    move |content, delta| {
                        content
                            .opacity(delta)
                            .left(px(24. * direction * (1. - delta)))
                    },
                )
                .into_any_element()
        };
        let overlay = self.model.overlay.map(|overlay| {
            let dialog = match overlay {
                Overlay::Settings => self.settings_dialog(cx, &t).into_any_element(),
                Overlay::About => self.about_dialog(cx, &t).into_any_element(),
                Overlay::Install => self.install_dialog(cx, &t),
            };
            div()
                .id("scrim")
                .absolute()
                .top(px(44.))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .bg(t.scrim)
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.set_overlay(None, cx)),
                )
                .child(dialog)
        });
        let bottom = self.bottom_bar(cx, &t);
        div()
            .id("builder")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .font_family(SANS)
            .text_size(px(13.))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.drop_paths(paths, cx)))
            .child(header)
            .child(content)
            .child(bottom)
            .children(overlay)
    }
}
