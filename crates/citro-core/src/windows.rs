//! Does a process have a window? Asked of the macOS window server, which knows about every window, minimized and
//! hidden ones included. Used to notice that the user closed a browser's last window (the red button), which on
//! macOS leaves the app running.

/// `Some(true)` when `pid` owns at least one ordinary window, `Some(false)` when it owns none, `None` where this cannot
/// be asked (not macOS).
pub fn has_window(pid: u32) -> Option<bool> {
    imp::has_window(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(a: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(n: CFTypeRef, kind: isize, out: *mut c_void) -> bool;
        fn CFRelease(o: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
        fn CGRectMakeWithDictionaryRepresentation(d: CFTypeRef, rect: *mut CGRect) -> bool;
        static kCGWindowOwnerPID: CFTypeRef;
        static kCGWindowLayer: CFTypeRef;
        static kCGWindowBounds: CFTypeRef;
    }

    const ALL_WINDOWS: u32 = 0; // kCGWindowListOptionAll: minimized and hidden windows too
    const SINT64: isize = 4; // kCFNumberSInt64Type

    fn number(d: CFTypeRef, key: CFTypeRef) -> Option<i64> {
        // SAFETY: `d` is a live CFDictionary from the window list; the value, if any, is a CFNumber.
        unsafe {
            let v = CFDictionaryGetValue(d, key);
            let mut out: i64 = 0;
            (!v.is_null() && CFNumberGetValue(v, SINT64, (&raw mut out).cast())).then_some(out)
        }
    }

    pub fn has_window(pid: u32) -> Option<bool> {
        // SAFETY: the list is a CFArray of CFDictionary we own (Copy rule) and release before returning; the keys are
        // CoreGraphics constants.
        unsafe {
            let list = CGWindowListCopyWindowInfo(ALL_WINDOWS, 0);
            if list.is_null() {
                return None;
            }
            let mut found = false;
            for i in 0..CFArrayGetCount(list) {
                let w = CFArrayGetValueAtIndex(list, i);
                if number(w, kCGWindowOwnerPID) != Some(i64::from(pid)) || number(w, kCGWindowLayer) != Some(0) {
                    continue;
                }
                // a browser window, not one of the small helper surfaces Chromium keeps (1x1, or 30 px high strips)
                let b = CFDictionaryGetValue(w, kCGWindowBounds);
                let mut r = CGRect::default();
                if !b.is_null() && CGRectMakeWithDictionaryRepresentation(b, &mut r) && r.w >= 100.0 && r.h >= 100.0 {
                    found = true;
                    break;
                }
            }
            CFRelease(list);
            Some(found)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn has_window(_pid: u32) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_process_without_windows_has_none() {
        // this test process opens no window
        if let Some(w) = super::has_window(std::process::id()) {
            assert!(!w);
        }
        if let Some(w) = super::has_window(u32::MAX - 7) {
            assert!(!w);
        }
    }
}
