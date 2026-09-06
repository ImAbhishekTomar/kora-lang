//! Images as a runtime value.
//!
//! An agent-first language that can only read text cannot look at a receipt,
//! a screenshot, or a scanned form — which is most of the work people
//! actually hand to a model. So an image is an ordinary Kora value: it comes
//! out of `fs.image(path)` like text comes out of `fs.read(path)`, and it
//! goes into `analyze()` like any other data.
//!
//! Two defects fixed on the way in.
//!
//! **The extension is a claim; the bytes are the fact.** Python's
//! `mimetypes.guess_type` reads the filename, so a `.png` holding a JPEG is
//! sent to the provider mislabelled and comes back as an opaque 400. Here the
//! type is read from the magic bytes and the extension is never consulted.
//!
//! **A too-large image fails at the provider, not at the call site.** The
//! limit is checked when the file is read, so the error names the file and
//! its size instead of surfacing as an HTTP error several frames later.

/// An image loaded into the program.
///
/// Bytes are held decoded-as-read (no re-encoding): whatever the provider
/// needs on the wire is the provider's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The MIME type read from the file's magic bytes, e.g. `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
    /// Where it came from, for display and error messages.
    pub source: String,
}

/// Beyond this, every provider refuses the request anyway; failing here names
/// the file instead of returning an opaque HTTP error.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;

impl Image {
    /// Identify `bytes` as an image, or say what was found instead.
    ///
    /// `source` only labels the value; it is never parsed for a type.
    pub fn detect(bytes: Vec<u8>, source: &str) -> Result<Image, String> {
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "{source} is {} and the limit is {}",
                human_size(bytes.len()),
                human_size(MAX_BYTES)
            ));
        }
        let Some(mime) = sniff(&bytes) else {
            return Err(format!(
                "{source} is not a PNG, JPEG, GIF, or WebP image (its first bytes are {})",
                first_bytes(&bytes)
            ));
        };
        Ok(Image {
            mime: mime.to_string(),
            bytes,
            source: source.to_string(),
        })
    }

    /// One-line summary for `print`, the debugger, and error messages. Never
    /// the bytes: a terminal full of binary helps nobody.
    pub fn summary(&self) -> String {
        format!(
            "<image {} {} {}>",
            self.source,
            self.mime,
            human_size(self.bytes.len())
        )
    }
}

/// The MIME type of `bytes`, read from the format's own header.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // RIFF....WEBP — the four size bytes in between are not part of the tag.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// A short hex preview, so "not an image" says what it actually got.
fn first_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "empty".to_string();
    }
    let preview: Vec<String> = bytes.iter().take(4).map(|b| format!("{b:02x}")).collect();
    preview.join(" ")
}

fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = bytes as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    #[test]
    fn each_supported_format_is_recognized() {
        assert_eq!(sniff(&png()), Some("image/png"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a....."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
    }

    /// The point of sniffing: a filename that lies must not decide the type.
    #[test]
    fn the_bytes_decide_not_the_extension() {
        let jpeg_bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        let image = Image::detect(jpeg_bytes, "receipt.png").unwrap();
        assert_eq!(image.mime, "image/jpeg");
    }

    #[test]
    fn a_non_image_says_what_it_found() {
        let err = Image::detect(b"%PDF-1.7".to_vec(), "invoice.png").unwrap_err();
        assert!(err.contains("invoice.png"), "{err}");
        assert!(err.contains("25 50 44 46"), "{err}");
    }

    #[test]
    fn an_empty_file_is_reported_as_empty() {
        let err = Image::detect(Vec::new(), "blank.png").unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn oversize_names_the_file_and_both_sizes() {
        let err = Image::detect(vec![0u8; MAX_BYTES + 1], "huge.png").unwrap_err();
        assert!(err.contains("huge.png"), "{err}");
        assert!(err.contains("20.0 MB"), "{err}");
    }

    /// A near-miss on the WebP tag must not be accepted: `RIFF` alone is also
    /// a WAV file.
    #[test]
    fn riff_without_webp_is_not_an_image() {
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff(b"RIFF"), None);
    }

    #[test]
    fn summary_shows_source_type_and_size_never_bytes() {
        let image = Image::detect(png(), "dataset/0.png").unwrap();
        let summary = image.summary();
        assert_eq!(summary, "<image dataset/0.png image/png 40 bytes>");
    }
}
