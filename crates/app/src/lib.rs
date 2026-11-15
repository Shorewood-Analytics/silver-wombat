//! Spike: prove GPUI + gpui-kit run in a browser from this workspace.
use std::borrow::Cow;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::label::Label;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, WindowOptions,
};

struct Spike {
    editor: Entity<EditorState>,
    taps: usize,
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(Label::new(format!("silver-wombat spike. Taps: {}", self.taps)))
            .child(
                Button::new("tap")
                    .primary()
                    .label("Tap me")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.taps += 1;
                        cx.notify();
                    })),
            )
            .child(Editor::new(&self.editor).h_full())
    }
}

fn launch(cx: &mut App) {
    gpui_kit::init(cx);
    let fonts = vec![
        Cow::Borrowed(include_bytes!("../fonts/Inter-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/JetBrainsMono-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/IBMPlexSans-Regular.ttf").as_slice()),
    ];
    cx.text_system().add_fonts(fonts).expect("bundled fonts load");
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = "Inter Variable".into();
        theme.mono_font_family = "JetBrains Mono".into();
    });
    gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
        cx.new(|cx| Spike {
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("python")
                    .line_number(true)
                    .default_value("def greet(name):\n    return f\"Hello, {name}\"\n")
            }),
            taps: 0,
        })
    })
    .expect("window opens");
    cx.activate(true);
}

#[cfg(target_family = "wasm")]
mod web {
    use std::cell::RefCell;

    use gpui_kit::ApplicationHandle;
    use wasm_bindgen::prelude::*;

    thread_local! {
        static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
    }

    /// Starts the app. Called once by `boot.js`.
    #[wasm_bindgen]
    pub fn run() {
        console_error_panic_hook::set_once();
        gpui_kit::platform::web_init();
        let handle = gpui_kit::platform::single_threaded_web().run_embedded(super::launch);
        APPLICATION.with(|application| *application.borrow_mut() = Some(handle));
    }
}

/// Native entry point, for running the same UI on a desktop while developing.
#[cfg(not(target_family = "wasm"))]
pub fn run_native() {
    gpui_kit::application().run(launch);
}
