//! anygenesis-gui: the AnyGenesis Builder. Pick a ROM, choose local patches, and build a
//! native game program by clicking; the build runs as a child `anygenesis --progress` next
//! to this program. Built games are for the user who built them: never distribute one.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod assets;
mod child;
mod model;
mod progress;
mod settings;
mod theme;
mod view;

use std::borrow::Cow;

use gpui_kit::{AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::{assets::Assets, view::Builder};

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        let fonts = assets::FONTS
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect();
        if let Err(error) = cx.text_system().add_fonts(fonts) {
            eprintln!("loading the bundled fonts: {error:#}");
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(960.), px(600.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("AnyGenesis Builder".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(960.), px(600.))),
            app_id: Some("anygenesis-gui".into()),
            ..Default::default()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Builder::new(window, cx))
        }) {
            eprintln!("opening the window: {error:#}");
            cx.quit();
        }
    });
}
