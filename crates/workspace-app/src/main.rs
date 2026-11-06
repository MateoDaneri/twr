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
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(560.))),
                titlebar: Some(TitlebarOptions {
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
