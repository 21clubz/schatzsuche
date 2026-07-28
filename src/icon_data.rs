//! The application icon, carried inside the binary.
//!
//! Embedded rather than loaded from the bundle, so the icon does not depend on
//! macOS resolving the bundle resource — which it failed to do once already,
//! after the bundle had briefly shipped a shell script as its executable and
//! the icon cache remembered a placeholder.
//!
//! Stored as a PNG and decoded at startup rather than as a raw RGBA array in
//! the source. The array worked at 128 pixels and was already 2700 lines; the
//! intro screen draws the mark at 420 physical pixels on a Retina display, so
//! 128 arrived visibly soft. The same picture at 512 would be a megabyte of
//! Rust source. Decoding costs well under a millisecond, once.
//!
//! The artwork comes from `assets/chest-source.png` via
//! `scripts/make-icon.py`.

/// Decoded icon: RGBA, eight bits per channel, `width * height * 4` bytes.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

static ICON_PNG: &[u8] = include_bytes!("../assets/icon-512.png");

/// Decodes the embedded icon.
///
/// Returns `None` rather than panicking: a window without an icon is a small
/// blemish, and a program that refuses to start over one would be a
/// considerably larger one.
pub fn icon() -> Option<Icon> {
    let decoder = png::Decoder::new(ICON_PNG);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;

    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(Icon {
        width: info.width,
        height: info.height,
        rgba: buf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon has to decode, be square, and be big enough for the intro
    /// screen, which draws it at 420 physical pixels on a Retina display.
    #[test]
    fn icon_decodes_at_a_useful_size() {
        let icon = icon().expect("embedded icon does not decode");
        assert_eq!(icon.width, icon.height, "icon is not square");
        assert!(
            icon.width >= 256,
            "icon is {}px; the intro screen would show it soft",
            icon.width
        );
        assert_eq!(
            icon.rgba.len() as u32,
            icon.width * icon.height * 4,
            "pixel buffer does not match the stated size"
        );
        assert!(
            icon.rgba.chunks_exact(4).any(|p| p[3] > 0),
            "icon is fully transparent"
        );
    }
}
