//! The application icon.

use std::sync::Arc;

use eframe::egui::{self, ColorImage, IconData, TextureHandle, TextureOptions};

const ICON_PNG: &[u8] = include_bytes!("../../../assets/app-256.png");

fn decode() -> Option<IconData> {
    eframe::icon_data::from_png_bytes(ICON_PNG).ok()
}

/// The window and Dock icon.
pub fn window_icon() -> Option<Arc<IconData>> {
    decode().map(Arc::new)
}

/// The icon as a texture for the header, smoothly minified.
pub fn logo_texture(ctx: &egui::Context) -> Option<TextureHandle> {
    let icon = decode()?;
    let image =
        ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    let options = TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    Some(ctx.load_texture("macwin-logo", image, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_icon_decodes_to_256_square() {
        let icon = decode().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
