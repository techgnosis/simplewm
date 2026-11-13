//! Local time formatted with the C library's `strftime`, so simplewm needs no date crate.
//! simplewm never calls `setlocale`, so names are always English ("Mon", "Jan").

use std::ffi::CString;
use std::sync::Once;

unsafe extern "C" {
    // Not in the libc crate for Linux. Reads TZ or /etc/localtime once, for localtime_r.
    fn tzset();
}

/// The current local time in a `strftime` format, e.g. "%H:%M:%S".
pub fn now(format: &str) -> String {
    static TZ: Once = Once::new();
    // SAFETY: tzset only sets libc's time zone globals; Once keeps it to one call.
    TZ.call_once(|| unsafe { tzset() });

    let Ok(format) = CString::new(format) else {
        return String::new();
    };
    let mut buf = [0u8; 128];
    // SAFETY: localtime_r writes only into `tm`; strftime writes at most buf.len() bytes
    // and returns how many it wrote (0 if they didn't fit).
    let len = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), format.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::now;

    #[test]
    fn formats_the_current_time() {
        let stamp = now("%Y-%m-%d-%H%M%S");
        assert_eq!(stamp.len(), 17, "{stamp}");
        assert!(stamp.starts_with("20"), "{stamp}");
        let clock = now(crate::ui::CLOCK_FORMAT);
        assert!(clock.contains(':'), "{clock}");
        assert_eq!(now(""), "");
    }
}
