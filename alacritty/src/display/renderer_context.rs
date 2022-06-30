#[rustfmt::skip]
#[cfg(not(any(target_os = "macos", windows)))]
use glutin::platform::unix::WindowBuilderExtUnix;

#[rustfmt::skip]
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use glutin::platform::unix::EventLoopWindowTargetExtUnix;

#[rustfmt::skip]
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use {
    std::io::Cursor,

    glutin::platform::unix::WindowExtUnix,
    x11_dl::xlib::{Display as XDisplay, PropModeReplace, XErrorEvent, Xlib},
    glutin::window::{Icon, Window},
    png::Decoder,
};

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU8, Ordering};

use bitflags::bitflags;
#[cfg(target_os = "macos")]
use cocoa::base::{id, NO, YES};
use glutin::dpi::{PhysicalPosition, PhysicalSize};
use glutin::event_loop::EventLoopWindowTarget;
#[cfg(target_os = "macos")]
use glutin::platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS};
#[cfg(windows)]
use glutin::platform::windows::IconExtWindows;
use glutin::window::WindowBuilder;
use glutin::{self, ContextBuilder, PossiblyCurrent, WindowedContext};
#[cfg(target_os = "macos")]
use objc::{msg_send, sel, sel_impl};
#[cfg(target_os = "macos")]
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
#[cfg(windows)]
use winapi::shared::minwindef::WORD;

use crate::config::window::{Decorations, Identity, WindowConfig};
use crate::config::UiConfig;
use crate::display::window::Error;

/// Window icon for `_NET_WM_ICON` property.
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
static WINDOW_ICON: &[u8] = include_bytes!("../../alacritty.png");

/// This should match the definition of IDI_ICON from `windows.rc`.
#[cfg(windows)]
const IDI_ICON: WORD = 0x101;

/// Context creation flags from probing config.
static GL_CONTEXT_CREATION_FLAGS: AtomicU8 = AtomicU8::new(GlContextFlags::SRGB.bits);

bitflags! {
    pub struct GlContextFlags: u8 {
        const EMPTY      = 0b000000000;
        const SRGB       = 0b0000_0001;
        const DEEP_COLOR = 0b0000_0010;
    }
}

pub struct RendererContext {
    windowed_context: Replaceable<WindowedContext<PossiblyCurrent>>,
}

/// Result of fallible operations concerning a RenderableContext.
type Result<T> = std::result::Result<T, Error>;

fn create_gl_window_context<E>(
    mut window: WindowBuilder,
    event_loop: &EventLoopWindowTarget<E>,
    flags: GlContextFlags,
    vsync: bool,
    dimensions: Option<PhysicalSize<u32>>,
) -> Result<WindowedContext<PossiblyCurrent>> {
    if let Some(dimensions) = dimensions {
        window = window.with_inner_size(dimensions);
    }

    let mut windowed_context_builder = ContextBuilder::new()
        .with_srgb(flags.contains(GlContextFlags::SRGB))
        .with_vsync(vsync)
        .with_hardware_acceleration(None);

    if flags.contains(GlContextFlags::DEEP_COLOR) {
        windowed_context_builder = windowed_context_builder.with_pixel_format(30, 2);
    }

    let windowed_context = windowed_context_builder.build_windowed(window, event_loop)?;

    // Make the context current so OpenGL operations can run.
    let windowed_context = unsafe { windowed_context.make_current().map_err(|(_, err)| err)? };

    Ok(windowed_context)
}

impl RendererContext {
    /// Create a new renderer context window.
    ///
    /// This creates a window and fully initializes a window.
    pub fn new<E>(
        event_loop: &EventLoopWindowTarget<E>,
        config: &UiConfig,
        identity: &Identity,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self> {
        let identity = identity.clone();
        let mut window_builder = Self::get_platform_window(&identity, &config.window);

        if let Some(position) = config.window.position {
            window_builder = window_builder
                .with_position(PhysicalPosition::<i32>::from((position.x, position.y)));
        }

        // Check if we're running Wayland to disable vsync.
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        let is_wayland = event_loop.is_wayland();
        #[cfg(any(not(feature = "wayland"), target_os = "macos", windows))]
        let is_wayland = false;

        let mut windowed_context = None;
        let current_flags =
            GlContextFlags::from_bits_truncate(GL_CONTEXT_CREATION_FLAGS.load(Ordering::Relaxed));
        for flags in [
            current_flags,
            GlContextFlags::EMPTY,
            GlContextFlags::SRGB | GlContextFlags::DEEP_COLOR,
            GlContextFlags::DEEP_COLOR,
        ] {
            windowed_context = Some(create_gl_window_context(
                window_builder.clone(),
                event_loop,
                flags,
                !is_wayland,
                size,
            ));
            if windowed_context.as_ref().unwrap().is_ok() {
                GL_CONTEXT_CREATION_FLAGS.store(flags.bits, Ordering::Relaxed);
                break;
            }
        }

        let windowed_context = Replaceable::new(windowed_context.unwrap()?);

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        if !is_wayland {
            // On X11, embed the window inside another if the parent ID has been set.
            if let Some(parent_window_id) = config.window.embed {
                x_embed_window(windowed_context.window(), parent_window_id);
            }
        }

        Ok(Self { windowed_context })
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn get_platform_window(identity: &Identity, window_config: &WindowConfig) -> WindowBuilder {
        #[cfg(feature = "x11")]
        let icon = {
            let decoder = Decoder::new(Cursor::new(WINDOW_ICON));
            let (info, mut reader) = decoder.read_info().expect("invalid embedded icon");
            let mut buf = vec![0; info.buffer_size()];
            let _ = reader.next_frame(&mut buf);
            Icon::from_rgba(buf, info.width, info.height)
        };

        let builder = WindowBuilder::new()
            .with_title(&identity.title)
            .with_visible(false)
            .with_transparent(true)
            .with_decorations(window_config.decorations != Decorations::None)
            .with_maximized(window_config.maximized())
            .with_fullscreen(window_config.fullscreen());

        #[cfg(feature = "x11")]
        let builder = builder.with_window_icon(icon.ok());

        #[cfg(feature = "wayland")]
        let builder = builder.with_app_id(identity.class.instance.to_owned());

        #[cfg(feature = "x11")]
        let builder = builder
            .with_class(identity.class.instance.to_owned(), identity.class.general.to_owned());

        #[cfg(feature = "x11")]
        let builder = match &window_config.gtk_theme_variant {
            Some(val) => builder.with_gtk_theme_variant(val.clone()),
            None => builder,
        };

        builder
    }

    #[cfg(windows)]
    pub fn get_platform_window(identity: &Identity, window_config: &WindowConfig) -> WindowBuilder {
        let icon = glutin::window::Icon::from_resource(IDI_ICON, None);

        WindowBuilder::new()
            .with_title(&identity.title)
            .with_visible(false)
            .with_decorations(window_config.decorations != Decorations::None)
            .with_transparent(true)
            .with_maximized(window_config.maximized())
            .with_fullscreen(window_config.fullscreen())
            .with_window_icon(icon.ok())
    }

    #[cfg(target_os = "macos")]
    pub fn get_platform_window(identity: &Identity, window_config: &WindowConfig) -> WindowBuilder {
        let window = WindowBuilder::new()
            .with_title(&identity.title)
            .with_visible(false)
            .with_transparent(true)
            .with_maximized(window_config.maximized())
            .with_fullscreen(window_config.fullscreen());

        match window_config.decorations {
            Decorations::Full => window,
            Decorations::Transparent => window
                .with_title_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true),
            Decorations::Buttonless => window
                .with_title_hidden(true)
                .with_titlebar_buttons_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true),
            Decorations::None => window.with_titlebar_hidden(true),
        }
    }
}

impl Deref for RendererContext {
    type Target = Replaceable<WindowedContext<PossiblyCurrent>>;

    fn deref(&self) -> &Self::Target {
        &self.windowed_context
    }
}

impl DerefMut for RendererContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.windowed_context
    }
}

/// Struct for safe in-place replacement.
///
/// This struct allows easily replacing struct fields that provide `self -> Self` methods in-place,
/// without having to deal with constantly unwrapping the underlying [`Option`].
pub struct Replaceable<T>(Option<T>);

impl<T> Replaceable<T> {
    pub fn new(inner: T) -> Self {
        Self(Some(inner))
    }

    /// Replace the contents of the container.
    pub fn replace_with<F: FnMut(T) -> T>(&mut self, f: F) {
        self.0 = self.0.take().map(f);
    }

    /// Get immutable access to the wrapped value.
    pub fn get(&self) -> &T {
        self.0.as_ref().unwrap()
    }

    /// Get mutable access to the wrapped value.
    pub fn get_mut(&mut self) -> &mut T {
        self.0.as_mut().unwrap()
    }
}

impl<T> Deref for Replaceable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T> DerefMut for Replaceable<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_mut()
    }
}

#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
fn x_embed_window(window: &Window, parent_id: std::os::raw::c_ulong) {
    let (xlib_display, xlib_window) = match (window.xlib_display(), window.xlib_window()) {
        (Some(display), Some(window)) => (display, window),
        _ => return,
    };

    let xlib = Xlib::open().expect("get xlib");

    unsafe {
        let atom = (xlib.XInternAtom)(xlib_display as *mut _, "_XEMBED".as_ptr() as *const _, 0);
        (xlib.XChangeProperty)(
            xlib_display as _,
            xlib_window as _,
            atom,
            atom,
            32,
            PropModeReplace,
            [0, 1].as_ptr(),
            2,
        );

        // Register new error handler.
        let old_handler = (xlib.XSetErrorHandler)(Some(xembed_error_handler));

        // Check for the existence of the target before attempting reparenting.
        (xlib.XReparentWindow)(xlib_display as _, xlib_window as _, parent_id, 0, 0);

        // Drain errors and restore original error handler.
        (xlib.XSync)(xlib_display as _, 0);
        (xlib.XSetErrorHandler)(old_handler);
    }
}

#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
unsafe extern "C" fn xembed_error_handler(_: *mut XDisplay, _: *mut XErrorEvent) -> i32 {
    log::error!("Could not embed into specified window.");
    std::process::exit(1);
}
