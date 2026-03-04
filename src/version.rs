/// Minimum supported PJSIP version (2.13.0)
pub const MIN_VERSION: u32 = 0x020D_0000;

/// Maximum tested PJSIP version (2.15.0)
pub const MAX_TESTED_VERSION: u32 = 0x020F_0000;

/// Parse a PJSIP version number into (major, minor, patch) components.
///
/// PJSIP encodes versions as 0xMMmmpp00 where:
/// - MM = major version
/// - mm = minor version
/// - pp = patch version
pub fn parse_version(version_num: u32) -> (u8, u8, u8) {
    let major = ((version_num >> 24) & 0xFF) as u8;
    let minor = ((version_num >> 16) & 0xFF) as u8;
    let patch = ((version_num >> 8) & 0xFF) as u8;
    (major, minor, patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_2_15() {
        let (major, minor, patch) = parse_version(0x020F_0000);
        assert_eq!((major, minor, patch), (2, 15, 0));
    }

    #[test]
    fn parse_version_2_13() {
        let (major, minor, patch) = parse_version(0x020D_0000);
        assert_eq!((major, minor, patch), (2, 13, 0));
    }

    #[test]
    fn min_max_ordering() {
        assert!(MIN_VERSION <= MAX_TESTED_VERSION);
        let (min_major, min_minor, _) = parse_version(MIN_VERSION);
        let (max_major, max_minor, _) = parse_version(MAX_TESTED_VERSION);
        assert_eq!(min_major, max_major);
        assert!(min_minor < max_minor);
    }
}
