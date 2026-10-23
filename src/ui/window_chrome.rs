use super::chrome::CONTROL;
use irresponsible::error::{Result, invalid};
use winit::window::Window;

#[derive(Clone, Copy, Debug)]
pub struct WindowChrome {
    pub height: f64,
    pub controls_end: f64,
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSView, NSWindow, NSWindowButton, NSWindowStyleMask,
        NSWindowTitleVisibility,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn measure(window: &NSWindow) -> WindowChrome {
        let content = window
            .contentView()
            .expect("a constructed window owns a content view");
        content.layoutSubtreeIfNeeded();
        let bounds = content.bounds();
        let layout = window.contentLayoutRect();
        let mut height = bounds.size.height - layout.origin.y - layout.size.height;
        let mut controls_end = 0.0_f64;
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = window.standardWindowButton(kind)
                && !button.isHidden()
            {
                let rect = content.convertRect_fromView(button.bounds(), Some(&button));
                let top = if content.isFlipped() {
                    rect.origin.y
                } else {
                    bounds.size.height - rect.origin.y - rect.size.height
                };
                height = height.max(top * 2.0 + rect.size.height);
                controls_end = controls_end.max(rect.origin.x + rect.size.width);
            }
        }
        // The app's controls still need a band when macOS hides its own chrome.
        WindowChrome {
            height: height.max(CONTROL + 4.0),
            controls_end: controls_end + 12.0,
        }
    }
    pub fn from_window(window: &Window) -> Result<WindowChrome> {
        let handle = window.window_handle().map_err(|e| invalid(e.to_string()))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            unreachable!("macOS uses AppKit handles")
        };
        // SAFETY: winit owns this NSView for the lifetime of the borrowed Window;
        // all callers run inside the main-thread event loop.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        Ok(measure(
            &view.window().expect("winit's view belongs to its window"),
        ))
    }
    pub fn for_preview() -> Result<WindowChrome> {
        let main = MainThreadMarker::new().expect("window measurement runs on the main thread");
        let _application = NSApplication::sharedApplication(main);
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        // SAFETY: creation is on the main thread, with valid AppKit geometry and
        // standard style flags. The retained hidden window owns its content view.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1100.0, 860.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        Ok(measure(&window))
    }
}

#[cfg(target_os = "macos")]
pub use mac::{for_preview, from_window};

#[cfg(not(target_os = "macos"))]
pub fn from_window(_: &Window) -> Result<WindowChrome> {
    for_preview()
}
#[cfg(not(target_os = "macos"))]
pub fn for_preview() -> Result<WindowChrome> {
    Ok(WindowChrome {
        height: CONTROL + 4.0,
        controls_end: 12.0,
    })
}
