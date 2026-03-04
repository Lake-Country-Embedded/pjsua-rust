use std::ffi::CString;

use crate::ffi;

/// A helper that owns a CString and provides a pj_str_t pointing into it.
///
/// The pj_str_t is valid for the lifetime of this struct. Use `.as_pj_str()`
/// to get the raw pj_str_t to pass to PJSIP functions.
pub struct PjString {
    _cstring: CString,
    pj_str: ffi::pj_str_t,
}

impl PjString {
    /// Create a new PjString from a Rust string.
    ///
    /// Panics if the input contains interior null bytes.
    pub fn new(s: &str) -> Self {
        let cstring = CString::new(s).expect("PjString::new: interior null byte");
        let pj_str = ffi::pj_str_t {
            ptr: cstring.as_ptr() as *mut std::ffi::c_char,
            slen: s.len() as ffi::pj_ssize_t,
        };
        PjString {
            _cstring: cstring,
            pj_str,
        }
    }

    /// Get the raw pj_str_t value. The returned value borrows from this PjString.
    pub fn as_pj_str(&self) -> ffi::pj_str_t {
        self.pj_str
    }

    /// Get a mutable pointer to the internal pj_str_t (needed by some PJSIP APIs).
    pub fn as_mut_pj_str(&mut self) -> *mut ffi::pj_str_t {
        &mut self.pj_str as *mut ffi::pj_str_t
    }
}

/// Convert a pj_str_t to a Rust String.
///
/// Returns an empty string if the pointer is null or the length is zero/negative.
/// Non-UTF8 data is replaced using lossy conversion.
pub fn pj_str_to_string(pj_str: &ffi::pj_str_t) -> String {
    if pj_str.ptr.is_null() || pj_str.slen <= 0 {
        return String::new();
    }
    let len = pj_str.slen as usize;
    let slice = unsafe { std::slice::from_raw_parts(pj_str.ptr as *const u8, len) };
    String::from_utf8_lossy(slice).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let input = "sip:user@example.com";
        let pj = PjString::new(input);
        let pj_str = pj.as_pj_str();
        let output = pj_str_to_string(&pj_str);
        assert_eq!(input, output);
    }

    #[test]
    fn null_handling() {
        let pj_str = ffi::pj_str_t {
            ptr: std::ptr::null_mut(),
            slen: 0,
        };
        assert_eq!(pj_str_to_string(&pj_str), "");
    }

    #[test]
    fn empty_string() {
        let pj = PjString::new("");
        let pj_str = pj.as_pj_str();
        assert_eq!(pj_str.slen, 0);
        let output = pj_str_to_string(&pj_str);
        assert_eq!(output, "");
    }

    #[test]
    fn negative_slen() {
        let pj_str = ffi::pj_str_t {
            ptr: std::ptr::null_mut(),
            slen: -1,
        };
        assert_eq!(pj_str_to_string(&pj_str), "");
    }
}
