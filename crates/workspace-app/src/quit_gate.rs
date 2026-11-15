//! Routes every application quit through the view's `can_quit()` flow.
//!
//! GPUI 0.2.2 registers `GPUIApplicationDelegate` with
//! `applicationWillTerminate:` only (platform/mac/platform.rs), so AppKit
//! quits that do not pass through our `Quit` action (Dock → Quit, logout,
//! restart, AppleScript `quit`) terminate without asking about unsaved work.
//! `install` adds `applicationShouldTerminate:` to that delegate class at
//! runtime. A terminate request that the app has not approved is cancelled
//! and forwarded to the view, which runs the normal save / discard / cancel
//! flow and, when that flow approves, calls `quit` here: the flag is set
//! first, so the second `terminate:` from `App::quit` passes.
//!
//! When nothing would be asked (`NEEDS_PROMPT` false, kept current by the
//! view on every poll), the request is answered NSTerminateNow at once, so a
//! logout or restart with nothing unsaved proceeds. Residual window: the flag
//! can be up to one poll interval stale. Stale true cancels the logout as
//! before; stale false lets a just-typed edit be covered only by the
//! quit-time recovery flush (applicationWillTerminate -> on_app_quit).
//!
//! Known limit: a logout or restart cancelled because something must be asked
//! is not resumed by macOS; after the app quits the user repeats the logout.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static APPROVED: AtomicBool = AtomicBool::new(false);
/// True while quitting would need the view's prompt or wait. Conservative
/// (true) until the view computes it the first time.
static NEEDS_PROMPT: AtomicBool = AtomicBool::new(true);
static REQUESTS: OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> =
    OnceLock::new();

fn channel() -> &'static (async_channel::Sender<()>, async_channel::Receiver<()>) {
    // Capacity 1: repeated requests while one is pending collapse into it.
    REQUESTS.get_or_init(|| async_channel::bounded(1))
}

/// Asks the view to run its quit flow (save / discard / cancel).
pub fn request() {
    let _ = channel().0.try_send(());
}

/// Receiver polled by the view; a request means "run `can_quit()` now".
pub fn requests() -> async_channel::Receiver<()> {
    channel().1.clone()
}

/// Records whether `can_quit()` would ask or wait instead of returning true
/// immediately. Called by the view on every poll.
pub fn set_needs_prompt(needs: bool) {
    NEEDS_PROMPT.store(needs, Ordering::SeqCst);
}

/// Quits after the caller's `can_quit()` (or an equivalent approved flow)
/// returned true. Never call this to bypass that flow.
pub fn quit(cx: &gpui::App) {
    APPROVED.store(true, Ordering::SeqCst);
    cx.quit();
}

/// Adds `applicationShouldTerminate:` to the GPUI application delegate.
/// Call once, after `Application::run` has installed the delegate. Returns
/// false (and changes nothing) when the delegate is missing or already
/// answers that selector.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub fn install() -> bool {
    use objc::runtime::{class_addMethod, class_getInstanceMethod, object_getClass, Class, Imp, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    // NSApplicationTerminateReply: NSTerminateCancel = 0, NSTerminateNow = 1.
    extern "C" fn should_terminate(_: &Object, _: Sel, _: *mut Object) -> usize {
        if APPROVED.load(Ordering::SeqCst) || !NEEDS_PROMPT.load(Ordering::SeqCst) {
            return 1;
        }
        request();
        0
    }

    let _ = channel();
    // SAFETY: called on the main thread inside `Application::run`, after GPUI
    // set the shared application's delegate. `object_getClass` on a live
    // object returns its class; the selector is checked first so an existing
    // implementation is never replaced. The IMP signature matches the type
    // encoding "Q@:@" (NSUInteger return, self, _cmd, sender), the same
    // pattern GPUI uses for its own delegate methods.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return false;
        }
        let delegate: *mut Object = msg_send![app, delegate];
        if delegate.is_null() {
            return false;
        }
        let class = object_getClass(delegate) as *mut Class;
        let selector = sel!(applicationShouldTerminate:);
        if class.is_null() || !class_getInstanceMethod(class, selector).is_null() {
            return false;
        }
        let imp: Imp = std::mem::transmute::<
            extern "C" fn(&Object, Sel, *mut Object) -> usize,
            Imp,
        >(should_terminate);
        class_addMethod(class, selector, imp, c"Q@:@".as_ptr()) != objc::runtime::NO
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install() -> bool {
    false
}
