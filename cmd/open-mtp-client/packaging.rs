//! Checks on what `make app` bundles: the icon and the Info.plist template.

#[cfg(test)]
mod tests {
    const ICON: &[u8] = include_bytes!("../../assets/icon-1024.png");
    const PLIST: &str = include_str!("../../packaging/Info.plist");

    #[test]
    fn the_icon_is_a_1024_pixel_png_with_transparency() {
        assert_eq!(&ICON[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&ICON[12..16], b"IHDR");
        let width = u32::from_be_bytes(ICON[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(ICON[20..24].try_into().unwrap());
        assert_eq!((width, height), (1024, 1024), "iconutil needs 512@2x");
        let (bit_depth, color_type) = (ICON[24], ICON[25]);
        assert_eq!((bit_depth, color_type), (8, 6), "8-bit RGBA");
    }

    fn value_of<'a>(key: &str) -> &'a str {
        let after = PLIST
            .split_once(&format!("<key>{key}</key>"))
            .unwrap_or_else(|| panic!("{key} missing"))
            .1;
        let start = after.find("<string>").unwrap() + "<string>".len();
        let end = after.find("</string>").unwrap();
        after[start..end].trim()
    }

    #[test]
    fn the_bundle_runs_the_open_mtp_client_binary_with_its_icon() {
        assert_eq!(value_of("CFBundleExecutable"), "open-mtp-client");
        assert_eq!(value_of("CFBundleIconFile"), "open-mtp-client");
        assert_eq!(value_of("CFBundlePackageType"), "APPL");
        assert_eq!(
            value_of("CFBundleIdentifier"),
            "com.github.kidpeterpan.open-mtp-client"
        );
    }

    #[test]
    fn the_version_is_filled_in_at_build_time() {
        assert_eq!(value_of("CFBundleShortVersionString"), "@VERSION@");
        assert_eq!(value_of("CFBundleVersion"), "@VERSION@");
    }
}
