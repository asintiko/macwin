//! NSPasteboard access. Reading content (including `inspect`, which sizes
//! every type) is a programmatic pasteboard read: on recent macOS it shows
//! the "Paste from Other Apps" alert unless the app is allowed in
//! System Settings > Privacy & Security. `changeCount` and the list of
//! types are metadata and never trigger it, so [`MacClipboard::snapshot`]
//! reads content only when it is going to be shared.
//!
//! Files: Finder puts one `public.file-url` per item (plus their names as
//! text, which is ignored then). Writing NSURLs makes a Finder paste copy
//! the files. Images: `public.png` is preferred; `public.tiff` (screenshots
//! of older apps) is converted to PNG with NSBitmapImageRep. Images are
//! written as PNG and TIFF.
//!
//! The clipboard thread lives for a whole session and has no autorelease
//! pool of its own, so every call drains one ([`pooled`]).

use std::path::PathBuf;

use macwin_platform::image::png_size;
use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardContent, ClipboardFormat, ClipboardInfo,
    ClipboardSnapshot, MAC_SENSITIVE_TYPES, PlatformError, SnapshotLimits,
};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeString,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};
use tracing::debug;

const FILE_URL_TYPE: &str = "public.file-url";
const PNG_TYPE: &str = "public.png";
const TIFF_TYPE: &str = "public.tiff";

/// The general pasteboard.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacClipboard;

/// Runs `f` in its own autorelease pool, so the objects AppKit autoreleases
/// (pasteboard data, image representations) are freed when it returns.
fn pooled<T>(f: impl FnOnce() -> T) -> T {
    autoreleasepool(|_| f())
}

fn string_type() -> &'static NSString {
    // SAFETY: an AppKit constant valid for the process lifetime.
    unsafe { NSPasteboardTypeString }
}

/// The pasteboard's type names (metadata only).
fn type_names(pb: &NSPasteboard) -> Vec<String> {
    let Some(types) = pb.types() else {
        return Vec::new();
    };
    (0..types.count())
        .map(|i| types.objectAtIndex(i).to_string())
        .collect()
}

/// The paths of the copied files, one per pasteboard item.
fn file_paths(pb: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pb.pasteboardItems() else {
        return Vec::new();
    };
    let file_url = NSString::from_str(FILE_URL_TYPE);
    let mut paths = Vec::new();
    for i in 0..items.count() {
        let item = items.objectAtIndex(i);
        let Some(s) = item.stringForType(&file_url) else {
            continue;
        };
        let Some(url) = NSURL::URLWithString(&s) else {
            continue;
        };
        // File reference URLs (file:///.file/id=...) resolve to a path URL.
        let url = url.filePathURL().unwrap_or(url);
        if let Some(path) = url.path() {
            paths.push(PathBuf::from(path.to_string()));
        }
    }
    paths
}

/// TIFF -> PNG, or PNG -> TIFF, through NSBitmapImageRep.
fn convert(data: &NSData, to: NSBitmapImageFileType) -> Option<Retained<NSData>> {
    let rep = NSBitmapImageRep::imageRepWithData(data)?;
    if to == NSBitmapImageFileType::TIFF {
        return rep.TIFFRepresentation();
    }
    let props = NSDictionary::new();
    // SAFETY: an empty dictionary is a valid properties argument.
    unsafe { rep.representationUsingType_properties(to, &props) }
}

fn image_content(pb: &NSPasteboard, max_bytes: usize) -> ClipboardContent {
    let sized = |data: &NSData| {
        let len = data.len();
        if len > max_bytes {
            ClipboardContent::TooLarge { bytes: len as u64 }
        } else {
            ClipboardContent::Image(data.to_vec())
        }
    };
    if let Some(png) = pb.dataForType(&NSString::from_str(PNG_TYPE)) {
        return sized(&png);
    }
    let Some(tiff) = pb.dataForType(&NSString::from_str(TIFF_TYPE)) else {
        return ClipboardContent::Unsupported;
    };
    match convert(&tiff, NSBitmapImageFileType::PNG) {
        Some(png) => sized(&png),
        None => ClipboardContent::Unsupported,
    }
}

/// Sizes the string before converting it, so a huge text is never copied.
fn text_content(s: &NSString, max_text_bytes: usize) -> ClipboardContent {
    let utf16_len = s.len();
    if utf16_len > max_text_bytes {
        return ClipboardContent::TooLarge {
            bytes: utf16_len as u64,
        };
    }
    let text = s.to_string();
    if text.len() > max_text_bytes {
        ClipboardContent::TooLarge {
            bytes: text.len() as u64,
        }
    } else {
        ClipboardContent::Text(text)
    }
}

/// Tags the first item as macwin's write. A pasteboard that refuses the
/// marker still works: the change-count check catches our own writes.
fn mark(pb: &NSPasteboard) {
    let marker = NSString::from_str(CLIPBOARD_ORIGIN_FORMAT);
    if !pb.setData_forType(Some(&NSData::with_bytes(b"1")), &marker) {
        debug!("the pasteboard refused the origin marker");
    }
}

impl ClipboardBackend for MacClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        pooled(|| Ok(NSPasteboard::generalPasteboard().changeCount() as u64))
    }

    fn inspect(&self) -> Result<ClipboardInfo, PlatformError> {
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            let change_count = pb.changeCount() as u64;
            let mut formats = Vec::new();
            if let Some(types) = pb.types() {
                for i in 0..types.count() {
                    let t = types.objectAtIndex(i);
                    formats.push(ClipboardFormat {
                        name: t.to_string(),
                        size: pb.dataForType(&t).map(|d| d.len()),
                    });
                }
            }
            let ours = formats.iter().any(|f| f.name == CLIPBOARD_ORIGIN_FORMAT);
            Ok(ClipboardInfo {
                change_count,
                formats,
                ours,
            })
        })
    }

    fn read_text(&self) -> Result<Option<String>, PlatformError> {
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            Ok(pb.stringForType(string_type()).map(|s| s.to_string()))
        })
    }

    fn snapshot(&self, limits: &SnapshotLimits) -> Result<ClipboardSnapshot, PlatformError> {
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            let change_count = pb.changeCount() as u64;
            let types = type_names(&pb);
            let has = |name: &str| types.iter().any(|t| t == name);
            let content = if has(CLIPBOARD_ORIGIN_FORMAT) {
                ClipboardContent::Ours
            } else if MAC_SENSITIVE_TYPES.iter().any(|t| has(t)) {
                ClipboardContent::Sensitive
            } else if has(FILE_URL_TYPE) {
                match limits.files.then(|| file_paths(&pb)) {
                    Some(paths) if !paths.is_empty() => ClipboardContent::Files(paths),
                    _ => ClipboardContent::Unsupported,
                }
            } else if has(&string_type().to_string()) {
                match pb.stringForType(string_type()) {
                    Some(s) => text_content(&s, limits.max_text_bytes),
                    None => ClipboardContent::Unsupported,
                }
            } else if has(PNG_TYPE) || has(TIFF_TYPE) {
                if limits.images {
                    image_content(&pb, limits.max_image_bytes)
                } else {
                    ClipboardContent::Unsupported
                }
            } else {
                ClipboardContent::Unsupported
            };
            Ok(ClipboardSnapshot {
                change_count,
                content,
            })
        })
    }

    fn write_text(&self, text: &str) -> Result<u64, PlatformError> {
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            if !pb.setString_forType(&NSString::from_str(text), string_type()) {
                return Err(PlatformError::Backend(
                    "the pasteboard refused the text".into(),
                ));
            }
            mark(&pb);
            Ok(pb.changeCount() as u64)
        })
    }

    fn write_image(&self, png: &[u8]) -> Result<u64, PlatformError> {
        // ImageIO would expand an oversized picture before anything else.
        png_size(png).map_err(|e| PlatformError::Backend(e.to_string()))?;
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            let data = NSData::with_bytes(png);
            pb.clearContents();
            if !pb.setData_forType(Some(&data), &NSString::from_str(PNG_TYPE)) {
                return Err(PlatformError::Backend(
                    "the pasteboard refused the image".into(),
                ));
            }
            // Older apps only read TIFF.
            if let Some(tiff) = convert(&data, NSBitmapImageFileType::TIFF) {
                pb.setData_forType(Some(&tiff), &NSString::from_str(TIFF_TYPE));
            }
            mark(&pb);
            Ok(pb.changeCount() as u64)
        })
    }

    fn write_files(&self, paths: &[PathBuf]) -> Result<u64, PlatformError> {
        pooled(|| {
            let pb = NSPasteboard::generalPasteboard();
            let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
                .iter()
                .map(|p| {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
                    ProtocolObject::from_retained(url)
                })
                .collect();
            pb.clearContents();
            if !pb.writeObjects(&NSArray::from_retained_slice(&urls)) {
                return Err(PlatformError::Backend(
                    "the pasteboard refused the files".into(),
                ));
            }
            mark(&pb);
            Ok(pb.changeCount() as u64)
        })
    }
}
