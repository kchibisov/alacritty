#[rustfmt::skip]
#[cfg(not(any(target_os = "macos", windows)))]
use glutin::platform::unix::{WindowExtUnix};

#[rustfmt::skip]
#[cfg(not(any(target_os = "macos", windows)))]
use {
    std::sync::atomic::AtomicBool,
    std::sync::Arc,
};

#[rustfmt::skip]
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use {
    wayland_client::protocol::wl_surface::WlSurface,
    wayland_client::{Attached, EventQueue, Proxy},
};

use std::fmt::{self, Display, Formatter};

#[cfg(target_os = "macos")]
use cocoa::base::{id, NO, YES};
use glutin::dpi::{PhysicalPosition, PhysicalSize};
#[cfg(target_os = "macos")]
use glutin::platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS};
#[cfg(windows)]
use glutin::platform::windows::IconExtWindows;
use glutin::window::{CursorIcon, Fullscreen, UserAttentionType, Window as GlutinWindow, WindowId};
use glutin::{self, Rect};
#[cfg(target_os = "macos")]
use objc::{msg_send, sel, sel_impl};
#[cfg(target_os = "macos")]
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
#[cfg(windows)]
use winapi::shared::minwindef::WORD;

use alacritty_terminal::index::Point;

use crate::display::renderer_context::RendererContext;
use crate::display::SizeInfo;
use crate::gl;

/// Window errors.
#[derive(Debug)]
pub enum Error {
    /// Error creating the window.
    ContextCreation(glutin::CreationError),

    /// Error dealing with fonts.
    Font(crossfont::Error),

    /// Error manipulating the rendering context.
    Context(glutin::ContextError),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::ContextCreation(err) => err.source(),
            Error::Context(err) => err.source(),
            Error::Font(err) => err.source(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::ContextCreation(err) => write!(f, "Error creating GL context; {}", err),
            Error::Context(err) => write!(f, "Error operating on render context; {}", err),
            Error::Font(err) => err.fmt(f),
        }
    }
}

impl From<glutin::CreationError> for Error {
    fn from(val: glutin::CreationError) -> Self {
        Error::ContextCreation(val)
    }
}

impl From<glutin::ContextError> for Error {
    fn from(val: glutin::ContextError) -> Self {
        Error::Context(val)
    }
}

impl From<crossfont::Error> for Error {
    fn from(val: crossfont::Error) -> Self {
        Error::Font(val)
    }
}

/// A window which can be used for displaying the terminal.
///
/// Wraps the underlying windowing library to provide a stable API in Alacritty.
pub struct Window {
    /// Flag tracking frame redraw requests from Wayland compositor.
    #[cfg(not(any(target_os = "macos", windows)))]
    pub should_draw: Arc<AtomicBool>,

    /// Attached Wayland surface to request new frame events.
    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    pub wayland_surface: Option<Attached<WlSurface>>,

    /// Cached scale factor for quickly scaling pixel sizes.
    pub scale_factor: f64,

    /// Current window title.
    title: String,

    /// Rendering context associated with the particular [`Window`]
    renderer_context: RendererContext,

    current_mouse_cursor: CursorIcon,
    mouse_visible: bool,
}

impl Window {
    /// Create a new window.
    ///
    /// This creates a window and fully initializes a window.
    pub fn new(
        title: &str,
        renderer_context: RendererContext,
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        wayland_event_queue: Option<&EventQueue>,
    ) -> Self {
        // Text cursor.
        let current_mouse_cursor = CursorIcon::Text;
        renderer_context.window().set_cursor_icon(current_mouse_cursor);

        let is_wayland = wayland_event_queue.is_some();

        // Set OpenGL symbol loader. This call MUST be after window.make_current on windows.
        gl::load_with(|symbol| renderer_context.get_proc_address(symbol) as *const _);

        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        let wayland_surface = if is_wayland {
            // Attach surface to Alacritty's internal wayland queue to handle frame callbacks.
            let surface = renderer_context.window().wayland_surface().unwrap();
            let proxy: Proxy<WlSurface> = unsafe { Proxy::from_c_ptr(surface as _) };
            Some(proxy.attach(wayland_event_queue.as_ref().unwrap().token()))
        } else {
            None
        };

        let scale_factor = renderer_context.window().scale_factor();

        Self {
            current_mouse_cursor,
            renderer_context,
            mouse_visible: true,
            title: title.to_owned(),
            #[cfg(not(any(target_os = "macos", windows)))]
            should_draw: Arc::new(AtomicBool::new(true)),
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland_surface,
            scale_factor,
        }
    }

    #[inline]
    pub fn set_inner_size(&self, size: PhysicalSize<u32>) {
        self.window().set_inner_size(size);
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        self.window().inner_size()
    }

    #[inline]
    pub fn set_visible(&self, visibility: bool) {
        self.window().set_visible(visibility);
    }

    /// Set the window title.
    #[inline]
    pub fn set_title(&mut self, title: String) {
        self.title = title;
        self.window().set_title(&self.title);
    }

    /// Get the window title.
    #[inline]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[inline]
    pub fn request_redraw(&self) {
        self.window().request_redraw();
    }

    #[inline]
    pub fn set_mouse_cursor(&mut self, cursor: CursorIcon) {
        if cursor != self.current_mouse_cursor {
            self.current_mouse_cursor = cursor;
            self.window().set_cursor_icon(cursor);
        }
    }

    /// Set mouse cursor visible.
    pub fn set_mouse_visible(&mut self, visible: bool) {
        if visible != self.mouse_visible {
            self.mouse_visible = visible;
            self.window().set_cursor_visible(visible);
        }
    }

    pub fn set_urgent(&self, is_urgent: bool) {
        let attention = if is_urgent { Some(UserAttentionType::Critical) } else { None };

        self.window().request_user_attention(attention);
    }

    #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
    pub fn x11_window_id(&self) -> Option<usize> {
        self.window().xlib_window().map(|xlib_window| xlib_window as usize)
    }

    #[cfg(any(not(feature = "x11"), target_os = "macos", windows))]
    pub fn x11_window_id(&self) -> Option<usize> {
        None
    }

    pub fn id(&self) -> WindowId {
        self.window().id()
    }

    pub fn set_maximized(&self, maximized: bool) {
        self.window().set_maximized(maximized);
    }

    pub fn set_minimized(&self, minimized: bool) {
        self.window().set_minimized(minimized);
    }

    /// Toggle the window's fullscreen state.
    pub fn toggle_fullscreen(&self) {
        self.set_fullscreen(self.window().fullscreen().is_none());
    }

    /// Toggle the window's maximized state.
    pub fn toggle_maximized(&self) {
        self.set_maximized(!self.window().is_maximized());
    }

    #[cfg(target_os = "macos")]
    pub fn toggle_simple_fullscreen(&self) {
        self.set_simple_fullscreen(!self.window().simple_fullscreen());
    }

    pub fn set_fullscreen(&self, fullscreen: bool) {
        if fullscreen {
            self.window().set_fullscreen(Some(Fullscreen::Borderless(None)));
        } else {
            self.window().set_fullscreen(None);
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_simple_fullscreen(&self, simple_fullscreen: bool) {
        self.window().set_simple_fullscreen(simple_fullscreen);
    }

    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    pub fn wayland_surface(&self) -> Option<&Attached<WlSurface>> {
        self.wayland_surface.as_ref()
    }

    /// Adjust the IME editor position according to the new location of the cursor.
    pub fn update_ime_position(&self, point: Point, size: &SizeInfo) {
        let nspot_x = f64::from(size.padding_x() + point.column.0 as f32 * size.cell_width());
        let nspot_y = f64::from(size.padding_y() + (point.line.0 + 1) as f32 * size.cell_height());

        self.window().set_ime_position(PhysicalPosition::new(nspot_x, nspot_y));
    }

    pub fn swap_buffers(&self) {
        self.renderer_context.swap_buffers().expect("swap buffers");
    }

    pub fn swap_buffers_with_damage(&self, damage: &[Rect]) {
        self.renderer_context.swap_buffers_with_damage(damage).expect("swap buffes with damage");
    }

    #[cfg(any(target_os = "macos", windows))]
    pub fn swap_buffers_with_damage_supported(&self) -> bool {
        // Disable damage tracking on macOS/Windows since there's no observation of it working.
        false
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn swap_buffers_with_damage_supported(&self) -> bool {
        // On X11 damage tracking is behaving in unexpected ways on some NVIDIA systems. Since
        // there's no compositor supporting it, damage tracking is disabled on X11.
        //
        // For more see https://github.com/alacritty/alacritty/issues/6051.
        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        if self.window().xlib_window().is_some() {
            return false;
        }

        self.renderer_context.swap_buffers_with_damage_supported()
    }

    pub fn resize(&self, size: PhysicalSize<u32>) {
        self.renderer_context.resize(size);
    }

    pub fn make_not_current(&mut self) {
        if self.renderer_context.is_current() {
            self.renderer_context.replace_with(|context| unsafe {
                // We do ensure that context is current before any rendering operation due to multi
                // window support, so we don't need extra "type aid" from glutin here.
                context.make_not_current().expect("context swap").treat_as_current()
            });
        }
    }

    pub fn make_current(&mut self) {
        if !self.renderer_context.is_current() {
            self.renderer_context
                .replace_with(|context| unsafe { context.make_current().expect("context swap") });
        }
    }

    /// Disable macOS window shadows.
    ///
    /// This prevents rendering artifacts from showing up when the window is transparent.
    #[cfg(target_os = "macos")]
    pub fn set_has_shadow(&self, has_shadows: bool) {
        let raw_window = match self.window().raw_window_handle() {
            RawWindowHandle::AppKit(handle) => handle.ns_window as id,
            _ => return,
        };

        let value = if has_shadows { YES } else { NO };
        unsafe {
            let _: () = msg_send![raw_window, setHasShadow: value];
        }
    }

    fn window(&self) -> &GlutinWindow {
        self.renderer_context.window()
    }
}
