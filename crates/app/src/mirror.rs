//! Makes the canvas usable by assistive technology.
//!
//! GPUI draws the whole interface into one `<canvas>`, which screen readers,
//! Switch Control and Voice Control cannot see into. GPUI does build an
//! accessibility tree on desktop, but that code is compiled out for the
//! browser. So the app describes itself: every button, field and piece of
//! text the UI draws also registers a [`mark`], and after each frame the marks
//! are written to the page as real, invisible HTML elements placed exactly
//! over what they describe.
//!
//! * A screen reader reads those elements and "clicks" them directly.
//! * Voice Control and Switch Control tap the element's centre; the elements
//!   let touches pass through, so the tap lands on the canvas underneath.
//! * A hardware keyboard can Tab between them and press Enter.
//!
//! Flutter's web renderer and Google Docs use the same technique.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{AnyWindowHandle, App, Bounds, Pixels, Window, canvas};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

/// What a mark is, in terms assistive technology understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Button,
    /// One of the pane buttons at the bottom; `current` is the open pane.
    Tab {
        current: bool,
    },
    TextField {
        multiline: bool,
    },
    /// The page title.
    Heading,
    /// The title of the open pane or of a part of it.
    Section,
    Text,
    /// Text that is announced when it changes (save state, notices).
    Status,
    /// The terminal's output: new output is announced as it arrives.
    Log,
}

/// What happens when the element is activated.
pub type Action = Rc<dyn Fn(&mut Window, &mut App)>;

struct Node {
    key: String,
    role: Role,
    label: String,
    disabled: bool,
    rect: [f32; 4],
    action: Option<Action>,
}

/// The attributes last written for an element, to skip unchanged ones.
#[derive(PartialEq, Clone)]
struct Written {
    role: Role,
    label: String,
    disabled: bool,
    rect: [i32; 4],
}

struct Layer {
    container: web_sys::Element,
    elements: HashMap<String, (web_sys::Element, Written)>,
    order: Vec<String>,
}

thread_local! {
    static FRAME: RefCell<Vec<Node>> = const { RefCell::new(Vec::new()) };
    static ACTIONS: RefCell<HashMap<String, Action>> = RefCell::new(HashMap::new());
    static LAYER: RefCell<Option<Layer>> = const { RefCell::new(None) };
    static WINDOW: Cell<Option<AnyWindowHandle>> = const { Cell::new(None) };
}

/// An invisible element that fills its parent and records where it is. Put it
/// inside a `relative()` container next to the thing it describes.
pub fn mark(
    key: impl Into<String>,
    role: Role,
    label: impl Into<String>,
    disabled: bool,
    action: Option<Action>,
) -> impl IntoElement {
    let key = key.into();
    let label = label.into();
    canvas(
        move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
            FRAME.with(|frame| {
                frame.borrow_mut().push(Node {
                    key,
                    role,
                    label,
                    disabled,
                    rect: [
                        bounds.origin.x.into(),
                        bounds.origin.y.into(),
                        bounds.size.width.into(),
                        bounds.size.height.into(),
                    ],
                    action,
                });
            });
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Put this last in the root element. It runs after every [`mark`] of the
/// frame has been recorded and writes them to the page.
pub fn flush() -> impl IntoElement {
    canvas(
        |_, _: &mut Window, _: &mut App| {},
        |_, (), window: &mut Window, _: &mut App| {
            WINDOW.with(|cell| cell.set(Some(window.window_handle())));
            let nodes = FRAME.with(|frame| std::mem::take(&mut *frame.borrow_mut()));
            write(nodes);
        },
    )
    .absolute()
    .size_0()
}

fn tag_for(role: Role) -> &'static str {
    match role {
        Role::Button | Role::Tab { .. } => "button",
        Role::TextField { .. } | Role::Status | Role::Log => "div",
        Role::Heading => "h1",
        Role::Section => "h2",
        Role::Text => "p",
    }
}

fn set(element: &web_sys::Element, name: &str, value: Option<&str>) {
    let _ = match value {
        Some(value) => element.set_attribute(name, value),
        None => element.remove_attribute(name),
    };
}

fn apply(element: &web_sys::Element, node: &Written) {
    let [x, y, w, h] = node.rect;
    set(
        element,
        "style",
        Some(&format!("left:{x}px;top:{y}px;width:{w}px;height:{h}px")),
    );
    match node.role {
        Role::Button => {
            set(element, "type", Some("button"));
            set(element, "aria-label", Some(&node.label));
        }
        Role::Tab { current } => {
            set(element, "type", Some("button"));
            set(element, "aria-label", Some(&node.label));
            set(element, "aria-current", current.then_some("page"));
        }
        Role::TextField { multiline } => {
            set(element, "role", Some("textbox"));
            set(element, "tabindex", Some("0"));
            set(element, "aria-label", Some(&node.label));
            set(element, "aria-multiline", multiline.then_some("true"));
        }
        Role::Status => set(element, "role", Some("status")),
        Role::Log => {
            set(element, "role", Some("log"));
            set(element, "aria-label", Some("Terminal output"));
        }
        Role::Heading | Role::Section | Role::Text => {}
    }
    set(element, "disabled", node.disabled.then_some(""));
    set(element, "aria-disabled", node.disabled.then_some("true"));
    // Buttons and fields are named by `aria-label`; everything else is read
    // from its text.
    let text = match node.role {
        Role::TextField { .. } => "",
        _ => node.label.as_str(),
    };
    if element.text_content().as_deref() != Some(text) {
        element.set_text_content(Some(text));
    }
}

fn layer() -> Option<Layer> {
    let document = web_sys::window()?.document()?;
    // A landmark, so a screen reader can jump straight to the app.
    let container = document.create_element("main").ok()?;
    container.set_id("a11y");
    document.body()?.append_child(&container).ok()?;
    // The canvas itself has nothing a screen reader can use.
    if let Ok(Some(surface)) = document.query_selector("body > canvas") {
        set(&surface, "aria-hidden", Some("true"));
    }
    // GPUI receives typing through a hidden text area it adds to the page.
    // It is what a screen reader lands on while a field is being edited.
    if let Ok(Some(typing)) = document.query_selector("body > textarea") {
        set(
            &typing,
            "aria-label",
            Some("Text entry for the selected field"),
        );
    }
    let on_click = Closure::<dyn Fn(web_sys::Event)>::new(|event: web_sys::Event| activate(&event));
    let _ = container.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
    on_click.forget();
    let on_key = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(|event: web_sys::KeyboardEvent| {
        // A button turns Enter and Space into a click by itself; a text box does not.
        let is_field = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|element| element.get_attribute("role").as_deref() == Some("textbox"));
        if is_field && matches!(event.key().as_str(), "Enter" | " ") {
            event.prevent_default();
            activate(&event);
        }
    });
    let _ = container.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
    on_key.forget();
    Some(Layer {
        container,
        elements: HashMap::new(),
        order: Vec::new(),
    })
}

fn activate(event: &web_sys::Event) {
    let Some(key) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.get_attribute("data-key"))
    else {
        return;
    };
    let action = ACTIONS.with(|actions| actions.borrow().get(&key).cloned());
    let (Some(action), Some(window)) = (action, WINDOW.with(Cell::get)) else {
        return;
    };
    crate::with_app(|cx| {
        let _ = window.update(cx, |_, window, cx| action(window, cx));
    });
}

fn write(nodes: Vec<Node>) {
    LAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = layer();
        }
        let Some(layer) = slot.as_mut() else { return };
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        let mut actions = HashMap::new();
        let mut order = Vec::with_capacity(nodes.len());
        for node in nodes {
            // The same key twice in one frame is a bug in the caller; keep the first.
            if order.contains(&node.key) {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let written = Written {
                role: node.role,
                label: node.label,
                disabled: node.disabled,
                rect: node.rect.map(|value| value.round() as i32),
            };
            let stale = layer
                .elements
                .get(&node.key)
                .is_some_and(|(_, old)| tag_for(old.role) != tag_for(written.role));
            if stale && let Some((element, _)) = layer.elements.remove(&node.key) {
                element.remove();
            }
            if let Some((element, old)) = layer.elements.get_mut(&node.key) {
                if *old != written {
                    apply(element, &written);
                    *old = written;
                }
            } else {
                let Ok(element) = document.create_element(tag_for(written.role)) else {
                    continue;
                };
                set(&element, "data-key", Some(&node.key));
                apply(&element, &written);
                layer.elements.insert(node.key.clone(), (element, written));
            }
            if let Some(action) = node.action {
                actions.insert(node.key.clone(), action);
            }
            order.push(node.key);
        }

        layer.elements.retain(|key, (element, _)| {
            let keep = order.contains(key);
            if !keep {
                element.remove();
            }
            keep
        });
        // Reading order follows drawing order. Elements are only moved when
        // the order really changed, because moving one drops a screen
        // reader's focus on it.
        if layer.order != order {
            for key in &order {
                if let Some((element, _)) = layer.elements.get(key) {
                    let _ = layer.container.append_child(element);
                }
            }
            layer.order = order;
        }
        ACTIONS.with(|slot| *slot.borrow_mut() = actions);
    });
}
