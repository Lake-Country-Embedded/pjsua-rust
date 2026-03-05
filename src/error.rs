use crate::ffi;

/// Result type alias using PjError.
pub type Result<T> = std::result::Result<T, PjError>;

/// Errors originating from PJSIP or this crate's operations.
#[derive(Debug, thiserror::Error)]
pub enum PjError {
    /// A PJSIP function returned a non-success status code.
    #[error("PJSIP error {status}: {message}")]
    Pjsip { status: i32, message: String },

    /// An invalid argument was provided.
    #[error("Invalid argument: {0}")]
    InvalidArg(String),

    /// A requested resource was not found.
    #[error("Not found: {0}")]
    NotFound(String),

    /// PJSUA is already initialized.
    #[error("PJSUA already initialized")]
    AlreadyInitialized,

    /// A resource is already in use.
    #[error("Already in use: {0}")]
    AlreadyInUse(String),

    /// PJSUA has not been initialized yet.
    #[error("PJSUA not initialized")]
    NotInitialized,

    /// Configuration error.
    #[error("Config error: {0}")]
    Config(String),
}

/// Create a `PjError::Pjsip` directly from a non-zero status code.
///
/// Use this instead of `check_status(status).unwrap_err()` when you already
/// know the status is an error.
pub fn make_pj_error(status: i32) -> PjError {
    PjError::Pjsip {
        status,
        message: status_to_string(status),
    }
}

/// Check a pj_status_t return value.
/// Returns Ok(()) for PJ_SUCCESS (0), or Err(PjError::Pjsip) with the
/// human-readable error string from PJSIP.
pub fn check_status(status: i32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(PjError::Pjsip {
            status,
            message: status_to_string(status),
        })
    }
}

/// Convert a PJSIP status code to its human-readable string representation.
pub fn status_to_string(status: i32) -> String {
    let mut buf = [0u8; 256];
    unsafe {
        ffi::pj_strerror(
            status as ffi::pj_status_t,
            buf.as_mut_ptr() as *mut std::ffi::c_char,
            buf.len(),
        );
    }
    // Find the null terminator
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_status_success() {
        assert!(check_status(0).is_ok());
    }

    #[test]
    fn check_status_failure() {
        let err = check_status(70018).unwrap_err();
        match &err {
            PjError::Pjsip { status, message } => {
                assert_eq!(*status, 70018);
                assert!(!message.is_empty(), "Error message should not be empty");
            }
            other => panic!("Expected PjError::Pjsip, got: {:?}", other),
        }
        // The Display impl should produce a non-empty string
        let display = format!("{}", err);
        assert!(!display.is_empty());
    }

    #[test]
    fn make_pj_error_produces_pjsip_variant() {
        let err = make_pj_error(70018);
        match &err {
            PjError::Pjsip { status, message } => {
                assert_eq!(*status, 70018);
                assert!(!message.is_empty());
            }
            other => panic!("Expected Pjsip, got: {:?}", other),
        }
    }

    #[test]
    fn error_display_already_in_use() {
        let err = PjError::AlreadyInUse("conference port".into());
        assert_eq!(format!("{err}"), "Already in use: conference port");
    }

    #[test]
    fn error_display() {
        let err = PjError::InvalidArg("bad value".into());
        assert_eq!(format!("{err}"), "Invalid argument: bad value");

        let err = PjError::NotFound("account 99".into());
        assert_eq!(format!("{err}"), "Not found: account 99");

        let err = PjError::AlreadyInitialized;
        assert_eq!(format!("{err}"), "PJSUA already initialized");

        let err = PjError::NotInitialized;
        assert_eq!(format!("{err}"), "PJSUA not initialized");

        let err = PjError::Config("missing field".into());
        assert_eq!(format!("{err}"), "Config error: missing field");
    }
}
