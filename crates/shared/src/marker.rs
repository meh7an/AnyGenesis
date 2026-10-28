//! The marker every file the tools derive from a ROM image carries.

/// The text every file derived from a ROM image holds on its first line (C sources and
/// headers, listings, reports). The legal guard refuses any file holding it, so no such file
/// can be committed. It is spelled in two halves so that no source file holds it whole.
pub const ROM_MARKER: &str = concat!(
    "AnyGenesis: generated from a ROM image.",
    " Never distribute or commit this file."
);

/// The marker as the first line of a C source or header, with its newline.
pub fn c_marker_line() -> String {
    format!("/* {ROM_MARKER} */\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c_marker_line_is_one_comment_line() {
        let line = c_marker_line();
        assert!(line.starts_with("/* ") && line.ends_with(" */\n"));
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains(ROM_MARKER));
    }
}
