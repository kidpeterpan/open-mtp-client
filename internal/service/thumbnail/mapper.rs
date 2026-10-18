use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder};

use crate::dto::{THUMBNAIL_SIZE, Thumbnail};

/// Pictures this app can decode itself.
const DECODABLE: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];
/// Media a phone may have its own thumbnail for (its media index covers these).
const PHONE_MEDIA: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "heif", "dng", "mp4", "mov", "3gp", "mkv",
    "webm",
];

fn extension(name: &str) -> Option<String> {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => Some(ext.to_lowercase()),
        _ => None,
    }
}

pub fn is_decodable(name: &str) -> bool {
    extension(name).is_some_and(|ext| DECODABLE.contains(&ext.as_str()))
}

/// What macOS's Quick Look can preview that the decoder above can't.
const QUICK_LOOK: &[&str] = &["heic", "heif", "mov", "mp4", "m4v", "dng", "pdf"];

pub fn is_quick_look(name: &str) -> bool {
    extension(name).is_some_and(|ext| QUICK_LOOK.contains(&ext.as_str()))
}

pub fn is_phone_media(name: &str) -> bool {
    extension(name).is_some_and(|ext| PHONE_MEDIA.contains(&ext.as_str()))
}

/// Widest or tallest image decoded; anything bigger is refused before its
/// pixels are allocated.
pub const MAX_SIDE: u32 = 16_384;
/// Most memory one decode may allocate.
const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;

/// Decodes an image and shrinks it to fit `THUMBNAIL_SIZE`, keeping its
/// shape. `None` when the bytes aren't an image this app can read, or claim
/// a size that would take too much memory: a few bytes can declare a
/// 100,000 × 100,000 picture, and the bytes may come from a phone.
/// The picture is turned the way its own EXIF orientation says, as Finder
/// shows it.
pub fn to_thumbnail(bytes: &[u8]) -> Option<Thumbnail> {
    let (image, _) = decode_upright(bytes)?;
    Some(shrink(&image))
}

/// A phone's own thumbnail of a photo whose start is `head`: Android sends
/// the photo's EXIF thumbnail, stored the way the camera's sensor was held
/// and carrying no orientation of its own, so it is turned the way the
/// photo says. One whose shape already matches the photo turned (a phone
/// that turned it itself) is left as it is.
pub fn to_phone_thumbnail(bytes: &[u8], head: Option<&[u8]>) -> Option<Thumbnail> {
    let (mut image, turned) = decode_upright(bytes)?;
    if !turned
        && let Some(head) = head
        && let Some(orientation) = jpeg_orientation(head)
        && still_to_turn(&image, orientation, jpeg_stored_size(head))
    {
        image.apply_orientation(orientation);
    }
    Some(shrink(&image))
}

/// Whether a thumbnail still needs `orientation`: a quarter turn swaps width
/// and height, so a thumbnail already the shape of the photo turned upright
/// (the other shape from the stored pixels) has been turned; a square one,
/// or a flip, can't tell and is turned.
fn still_to_turn(
    image: &DynamicImage,
    orientation: Orientation,
    stored: Option<(u32, u32)>,
) -> bool {
    let quarter = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    match stored {
        Some((width, height)) if quarter && width != height && image.width() != image.height() => {
            (width > height) == (image.width() > image.height())
        }
        _ => orientation != Orientation::NoTransforms,
    }
}

/// Decodes with the limits below, turned upright by the picture's own
/// orientation; says whether it carried one that turned it.
fn decode_upright(bytes: &[u8]) -> Option<(DynamicImage, bool)> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some((image, orientation != Orientation::NoTransforms))
}

/// How far into a photo its orientation and size are looked for: the EXIF
/// block and the frame header come before the picture data.
pub const HEAD_BYTES: u32 = 64 * 1024;

/// The segments of a JPEG's start, up to its picture data: (marker, payload).
/// Bounds are checked at every step: the bytes come from the phone.
fn jpeg_segments(head: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = if head.starts_with(&[0xFF, 0xD8]) {
        2
    } else {
        head.len()
    };
    std::iter::from_fn(move || {
        let marker = *head
            .get(at..at + 2)
            .filter(|bytes| bytes[0] == 0xFF)?
            .get(1)?;
        // Start of scan or end of image: the picture data follows.
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes(
            head.get(at + 2..at + 4)?.try_into().ok()?,
        ));
        let payload = head.get(at + 4..at + 2 + length.max(2))?;
        at += 2 + length.max(2);
        Some((marker, payload))
    })
}

/// The orientation a JPEG's EXIF block gives, from the start of the file.
pub fn jpeg_orientation(head: &[u8]) -> Option<Orientation> {
    jpeg_segments(head)
        .find_map(|(marker, payload)| {
            (marker == 0xE1)
                .then_some(payload)?
                .strip_prefix(b"Exif\0\0")
        })
        .and_then(Orientation::from_exif_chunk)
}

/// The width and height a JPEG's pixels are stored at (before turning).
fn jpeg_stored_size(head: &[u8]) -> Option<(u32, u32)> {
    jpeg_segments(head).find_map(|(marker, payload)| {
        let frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        let height = u16::from_be_bytes(payload.get(1..3)?.try_into().ok()?);
        let width = u16::from_be_bytes(payload.get(3..5)?.try_into().ok()?);
        frame.then_some((u32::from(width), u32::from(height)))
    })
}

fn shrink(image: &DynamicImage) -> Thumbnail {
    // `thumbnail` would also enlarge small images; those stay as they are.
    let small = if image.width() <= THUMBNAIL_SIZE && image.height() <= THUMBNAIL_SIZE {
        image.to_rgba8()
    } else {
        image.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE).to_rgba8()
    };
    Thumbnail {
        width: small.width(),
        height: small.height(),
        rgba: small.into_raw(),
    }
}

/// A JPEG `width` × `height` with an EXIF block saying `orientation` (1–8),
/// as a phone's camera writes one.
#[cfg(test)]
pub fn jpeg_with_orientation(width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([30, 120, 200]));
    jpeg_of(&image, orientation)
}

/// `image` as a JPEG with an EXIF block saying `orientation` (1–8).
#[cfg(test)]
pub fn jpeg_of(image: &image::RgbImage, orientation: u16) -> Vec<u8> {
    let mut plain = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut plain, image::ImageFormat::Jpeg)
        .unwrap();
    let plain = plain.into_inner();
    // A big-endian TIFF header and one IFD entry: Orientation (0x0112), SHORT.
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
    exif.extend_from_slice(&orientation.to_be_bytes());
    exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
    bytes.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
    bytes.extend_from_slice(&exif);
    bytes.extend_from_slice(&plain[2..]);
    bytes
}

#[cfg(test)]
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_recognised_by_extension_ignoring_case() {
        assert!(is_decodable("IMG_001.JPG"));
        assert!(is_decodable("a.webp"));
        assert!(!is_decodable("clip.mp4"));
        assert!(!is_decodable(".png"));
        assert!(!is_decodable("README"));

        assert!(is_quick_look("IMG_0001.HEIC"));
        assert!(is_quick_look("clip.mov"));
        assert!(!is_quick_look("photo.jpg"));

        assert!(is_phone_media("clip.MP4"));
        assert!(is_phone_media("IMG_002.heic"));
        assert!(!is_phone_media("notes.txt"));
    }

    #[test]
    fn a_large_image_shrinks_to_fit_keeping_its_shape() {
        let thumbnail = to_thumbnail(&png(800, 400)).unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (192, 96));
        assert_eq!(thumbnail.rgba.len(), 192 * 96 * 4);
    }

    #[test]
    fn a_small_image_is_not_enlarged() {
        let thumbnail = to_thumbnail(&png(40, 20)).unwrap();

        assert_eq!((thumbnail.width, thumbnail.height), (40, 20));
    }

    #[test]
    fn an_image_wider_than_the_limit_is_refused() {
        assert_eq!(to_thumbnail(&png(MAX_SIDE + 1, 1)), None);
        assert!(to_thumbnail(&png(MAX_SIDE, 1)).is_some());
    }

    #[test]
    fn bytes_that_are_not_an_image_have_no_thumbnail() {
        assert_eq!(to_thumbnail(b"not an image"), None);
    }

    #[test]
    fn a_picture_is_turned_the_way_its_own_exif_says() {
        let turned = to_thumbnail(&jpeg_with_orientation(120, 80, 6)).unwrap();
        assert_eq!((turned.width, turned.height), (80, 120), "a quarter turn");
        let upright = to_thumbnail(&jpeg_with_orientation(120, 80, 1)).unwrap();
        assert_eq!((upright.width, upright.height), (120, 80));
        let half = to_thumbnail(&jpeg_with_orientation(120, 80, 3)).unwrap();
        assert_eq!(
            (half.width, half.height),
            (120, 80),
            "a half turn keeps the shape"
        );
    }

    #[test]
    fn a_jpegs_start_says_its_orientation_and_stored_size() {
        let photo = jpeg_with_orientation(400, 300, 8);
        assert_eq!(jpeg_orientation(&photo), Some(Orientation::Rotate270));
        assert_eq!(jpeg_stored_size(&photo), Some((400, 300)));
        assert_eq!(jpeg_orientation(&png(10, 10)), None, "not a JPEG");
        assert_eq!(jpeg_orientation(&photo[..10]), None, "cut short");
        assert_eq!(
            jpeg_orientation(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF]),
            None
        );
        assert_eq!(
            jpeg_stored_size(b"\xFF\xD8\xFF\xC0\x00\x03"),
            None,
            "a frame too short"
        );
        assert_eq!(jpeg_orientation(&[]), None);
    }

    #[test]
    fn a_phones_thumbnail_is_turned_as_the_photo_says_unless_it_already_was() {
        // The phone's EXIF thumbnail: stored as the photo is, no orientation.
        let photo = jpeg_with_orientation(4000, 3000, 6);
        let thumb = jpeg_with_orientation(160, 120, 1);
        let turned = to_phone_thumbnail(&thumb, Some(&photo[..])).unwrap();
        assert_eq!((turned.width, turned.height), (120, 160));

        // A phone that turned it itself: already the photo's upright shape.
        let already = jpeg_with_orientation(120, 160, 1);
        let left = to_phone_thumbnail(&already, Some(&photo[..])).unwrap();
        assert_eq!((left.width, left.height), (120, 160), "not turned twice");

        // No start of the photo (the phone can't read part of it): as sent.
        let plain = to_phone_thumbnail(&thumb, None).unwrap();
        assert_eq!((plain.width, plain.height), (160, 120));
        // A thumbnail with an orientation of its own follows that, once.
        let own =
            to_phone_thumbnail(&jpeg_with_orientation(160, 120, 6), Some(&photo[..])).unwrap();
        assert_eq!((own.width, own.height), (120, 160));
        // Square: can't tell, turned as the photo says.
        let square = to_phone_thumbnail(
            &jpeg_with_orientation(100, 100, 1),
            Some(&jpeg_with_orientation(400, 300, 3)[..]),
        );
        assert!(square.is_some());
    }

    /// A square picture, red on the left and blue on the right.
    fn halves() -> image::RgbImage {
        image::RgbImage::from_fn(100, 100, |x, _| {
            if x < 50 {
                image::Rgb([220, 20, 20])
            } else {
                image::Rgb([20, 20, 220])
            }
        })
    }

    fn is_red(thumbnail: &Thumbnail, x: u32, y: u32) -> bool {
        let at = ((y * thumbnail.width + x) * 4) as usize;
        thumbnail.rgba[at] > 150 && thumbnail.rgba[at + 2] < 100
    }

    #[test]
    fn a_square_thumbnail_is_turned_once_whether_by_its_own_exif_or_the_photos() {
        let photo = jpeg_with_orientation(400, 300, 3);
        // Turned by the photo's half turn: red ends up on the right.
        let by_photo = to_phone_thumbnail(&jpeg_of(&halves(), 1), Some(&photo[..])).unwrap();
        assert!(is_red(&by_photo, 90, 50) && !is_red(&by_photo, 10, 50));
        // Turned by its own half turn, and not again by the photo's.
        let by_itself = to_phone_thumbnail(&jpeg_of(&halves(), 3), Some(&photo[..])).unwrap();
        assert!(is_red(&by_itself, 90, 50), "turned once, not back again");
    }

    #[test]
    fn the_start_is_read_only_up_to_the_picture_data_and_past_huffman_tables() {
        // A Huffman table before the frame header: not taken for the size.
        let mut bytes = vec![
            0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x08, 0x10, 0x00, 0x05, 0x00, 0x07, 0xAA,
        ];
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x08, 0x08, 0x01, 0x2C, 0x01, 0x90, 0x03]);
        assert_eq!(jpeg_stored_size(&bytes), Some((400, 300)));
        // An EXIF block after the start of the picture data is not this
        // photo's: what follows there is pixels.
        let mut plain = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(40, 30)
            .write_to(&mut plain, image::ImageFormat::Jpeg)
            .unwrap();
        let photo = plain.into_inner();
        let start = photo
            .windows(2)
            .position(|pair| pair == [0xFF, 0xDA])
            .unwrap();
        let mut late = photo[..start].to_vec();
        late.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        late.extend_from_slice(&jpeg_with_orientation(4, 4, 6)[2..]);
        assert_eq!(jpeg_orientation(&late), None, "no EXIF before the pixels");
    }
}
