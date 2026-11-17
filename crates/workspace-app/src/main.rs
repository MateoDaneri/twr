mod quit_gate;
mod repeat_input;
mod view;
use gpui::*;
use std::path::PathBuf;
use view::WorkspaceView;
actions!(workspace_app, [Quit]);
fn main() {
    configure_key_repeat();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Resources/Demo/projects");
    let bundled = std::env::current_exe().ok().and_then(|p| {
        p.parent()?
            .parent()
            .map(|p| p.join("Resources/Demo/projects"))
    });
    let demo = bundled.filter(|p| p.is_dir()).unwrap_or(source);
    let validation = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()?
                .parent()
                .map(|p| p.join("Resources/validation-profile"))
        })
        .is_some_and(|p| p.is_file());
    let instance = validation
        .then(|| {
            std::env::current_exe().ok().and_then(|p| {
                let path = p.parent()?.parent()?.join("Resources/validation-instance");
                std::fs::read_to_string(path)
                    .ok()
                    .map(|s| s.trim().to_owned())
            })
        })
        .flatten();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let layout = workspace_app::storage::Layout::resolve(
        home.as_deref(),
        &std::env::var("USER").unwrap_or_else(|_| "local".into()),
        validation,
    )
    .expect("resolve private app directories");
    let layout = if let Some(instance) = &instance {
        layout
            .validation_instance(instance)
            .expect("validation instance identifier")
    } else {
        layout
    };
    Application::new().run(move |cx: &mut App| {
        // App-local registration, never installs a font in macOS.
        let nerd_font_ready = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!(
                "../../../Resources/Fonts/JetBrainsMonoNerdFont-Regular.ttf"
            ))])
            .is_ok();
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        // Dock, logout, restart and AppleScript quits reach AppKit's
        // terminate: directly; route them through the view's quit flow.
        quit_gate::install();
        // Fallback for Cmd+Q when the root view's Quit handler is not on the
        // dispatch path (GPUI runs global handlers only if no element handled
        // the action), so the action is never handled twice.
        cx.on_action(|_: &Quit, _cx| quit_gate::request());
        cx.on_window_closed(|cx| {
            // The window closed only after on_window_should_close ran
            // can_quit() and allowed it.
            if cx.windows().is_empty() {
                quit_gate::quit(cx);
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.), px(12.))),
                    title: Some(
                        if let Some(instance) = &instance {
                            format!("Terminal Workspace · Validación {instance}")
                        } else if validation {
                            "Terminal Workspace · Validación sintética".to_owned()
                        } else {
                            "Terminal Workspace · Flujo familiar".to_owned()
                        }
                        .into(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    WorkspaceView::new(demo, validation, layout, nerd_font_ready, window, cx)
                })
            },
        )
        .expect("open native workspace");
        install_titlebar_input();
        cx.activate(true);
    });
}

// Only register a fallback for this process; do not write system preferences.
// Explicit user defaults keep precedence over the registration domain.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn configure_key_repeat() {
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};
    unsafe {
        let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
        let key: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c"ApplePressAndHoldEnabled".as_ptr()];
        let disabled: *mut Object = msg_send![class!(NSNumber), numberWithBool: false];
        let values: *mut Object =
            msg_send![class!(NSDictionary), dictionaryWithObject: disabled forKey: key];
        let defaults: *mut Object = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let _: () = msg_send![defaults, registerDefaults: values];
        let _: () = msg_send![pool, drain];
    }
}

#[cfg(not(target_os = "macos"))]
fn configure_key_repeat() {}

// AppKit requires the original mouse event to begin native window dragging.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn drag_native_titlebar() {
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut Object = msg_send![app, currentEvent];
        if !event.is_null() {
            let window: *mut Object = msg_send![event, window];
            if !window.is_null() {
                let _: () = msg_send![window, performWindowDragWithEvent: event];
            }
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn drag_native_titlebar() {}

// Backport GPUI's app-owned titlebar region for AppKit on macOS 27.
// GPUI 0.2.2 lacks app_owns_titlebar_drag; this app handles dragging explicitly.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn install_titlebar_input() {
    use objc::{msg_send, sel, sel_impl, Encode, Encoding};
    use objc::runtime::{Class, Object, Sel, Imp, class_addMethod};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect { x: f64, y: f64, width: f64, height: f64 }
    unsafe impl Encode for Rect {
        fn encode() -> Encoding { unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") } }
    }
    extern "C" fn app_owned_region(view: &Object, _: Sel) -> Rect {
        unsafe { msg_send![view, bounds] }
    }
    // Runs on the main thread after GPUI creates its view class. Never replace
    // an implementation supplied by GPUI itself. CGRect ABI is four doubles.
    unsafe {
        if let Some(view_class) = Class::get("GPUIView") {
            let selector = sel!(_opaqueRectForWindowMoveWhenInTitlebar);
            {
                // class_addMethod overrides an inherited implementation, but
                // returns NO rather than replacing a method owned by GPUIView.
                let imp: Imp = std::mem::transmute::<extern "C" fn(&Object, Sel) -> Rect, Imp>(app_owned_region);
                class_addMethod(view_class as *const Class as *mut Class, selector, imp,
                    c"{CGRect={CGPoint=dd}{CGSize=dd}}@:".as_ptr());
            }
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn install_titlebar_input() {}
