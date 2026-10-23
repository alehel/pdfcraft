use std::fmt;

// PdfCraft patch: no Wayland on illumos and Solaris, so the `wayland` feature is ignored there
// (Cargo.toml leaves its crates out).

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

#[derive(Debug)]
#[non_exhaustive]
pub enum WindowIdentifier {
    #[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
    Wayland(WaylandWindowIdentifier),
    X11(WindowIdentifierType),
}

impl std::fmt::Display for WindowIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
            Self::Wayland(identifier) => identifier.fmt(f),
            Self::X11(identifier) => identifier.fmt(f),
        }
    }
}

impl WindowIdentifier {
    pub async fn from_raw_handle(
        window_handle: &RawWindowHandle,
        display_handle: Option<&RawDisplayHandle>,
    ) -> Option<Self> {
        use raw_window_handle::RawWindowHandle::{Xcb, Xlib};
        #[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
        use raw_window_handle::{
            RawDisplayHandle::Wayland as DisplayHandle, RawWindowHandle::Wayland,
        };
        match (window_handle, display_handle) {
            #[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
            (Wayland(wl_handle), Some(DisplayHandle(wl_display))) => unsafe {
                Self::from_wayland_raw(wl_handle.surface.as_ptr(), wl_display.display.as_ptr())
                    .await
            },
            (Xlib(x_handle), _) => Some(Self::from_xid(x_handle.window)),
            (Xcb(x_handle), _) => Some(Self::from_xid(x_handle.window.get().into())),
            _ => None,
        }
    }

    pub fn from_xid(xid: std::os::raw::c_ulong) -> Self {
        Self::X11(WindowIdentifierType::X11(xid))
    }

    #[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
    pub async unsafe fn from_wayland_raw(
        surface_ptr: *mut std::ffi::c_void,
        display_ptr: *mut std::ffi::c_void,
    ) -> Option<Self> {
        WaylandWindowIdentifier::from_raw(surface_ptr, display_ptr)
            .await
            .map(Self::Wayland)
    }
}

/// Supported WindowIdentifier kinds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowIdentifierType {
    /// X11.
    X11(std::os::raw::c_ulong),
    #[allow(dead_code)]
    /// Wayland.
    Wayland(String),
}

impl fmt::Display for WindowIdentifierType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::X11(xid) => {
                f.write_str("x11:")?;
                write!(f, "{xid:x}")
            }
            Self::Wayland(handle) => {
                f.write_str("wayland:")?;
                f.write_str(handle)
            }
        }
    }
}

#[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
mod wayland;

#[cfg(all(feature = "wayland", not(any(target_os = "illumos", target_os = "solaris"))))]
pub use self::wayland::WaylandWindowIdentifier;
