//! The `pdf-render` package's helper: PDF pages in, PNG pages out.
//!
//! This program exists so that PDFium does not have to live inside Kora.
//! Rendering a page needs a real renderer — fonts, shading, transparency —
//! and every one of those is a large C++ library with a long history of
//! memory-safety bugs, reading a file the program did not write. Kora's
//! answer is not to trust it more, but to hold it further away: this is a
//! separate process, it receives bytes rather than paths, and if it dies the
//! run that called it gets an error rather than a corrupted heap.
//!
//! It speaks `stdio/v1`, the protocol in `kora-helper`: a big-endian length,
//! a JSON header, then the payloads the header declares.
//!
//! ```text
//! render(dpi, max_pages)   one PDF in, one PNG per page out
//! page_count()             how many pages, without rendering any
//! ```
//!
//! PDFium itself is loaded at run time from beside this executable, so the
//! release artifact is this binary and the library together, and neither is
//! anywhere near the machine of someone who never renders a PDF.

use std::io::{Read, Write};

use pdfium_render::prelude::*;
use serde_json::{json, Value};

/// Beyond this a render is refused rather than attempted. A hundred pages at
/// 300 dpi is gigabytes of pixels, and the honest answer to that request is
/// "say which pages", not an out-of-memory kill.
const MAX_PAGES: usize = 200;
const MAX_DPI: f32 = 600.0;

fn main() {
    let pdfium = match load_pdfium() {
        Ok(pdfium) => pdfium,
        Err(why) => {
            // Nothing this helper does is possible without the library, so
            // say why once, clearly, and stop. Kora reports the exit as a
            // failed call.
            eprintln!("kora-pdf-helper: {why}");
            std::process::exit(1);
        }
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    while let Some((header, blobs)) = read_frame(&mut input) {
        let id = header.get("id").cloned().unwrap_or(Value::Null);
        let call = header.get("call").and_then(Value::as_str).unwrap_or("");
        let args = match header.get("args") {
            Some(Value::Array(args)) => args.clone(),
            _ => Vec::new(),
        };

        let (payload, out) = match handle(&pdfium, call, &args, &blobs) {
            Ok((result, blobs)) => (json!({ "id": id, "ok": true, "result": result }), blobs),
            Err(why) => (json!({ "id": id, "ok": false, "error": why }), Vec::new()),
        };
        if write_frame(&mut output, payload, &out).is_err() {
            return;
        }
    }
}

/// PDFium from beside this executable first, then wherever the system keeps
/// it. Beside is what a release artifact ships, so it is what is tried first.
fn load_pdfium() -> Result<Pdfium, String> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));

    if let Some(dir) = beside {
        let name = Pdfium::pdfium_platform_library_name_at_path(&dir);
        if let Ok(bindings) = Pdfium::bind_to_library(name) {
            return Ok(Pdfium::new(bindings));
        }
    }
    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Pdfium::new(bindings)),
        Err(e) => Err(format!(
            "could not load PDFium ({e}). It ships beside this binary; \
             reinstall the package with `kora install`"
        )),
    }
}

fn handle(
    pdfium: &Pdfium,
    call: &str,
    args: &[Value],
    blobs: &[Vec<u8>],
) -> Result<(Value, Vec<Vec<u8>>), String> {
    let Some(document) = blobs.first() else {
        return Err(format!("{call} needs a document"));
    };
    let document = pdfium
        .load_pdf_from_byte_slice(document, None)
        .map_err(|e| format!("this is not a PDF Kora can read ({e})"))?;

    match call {
        "page_count" => Ok((json!(document.pages().len()), Vec::new())),
        "render" => {
            let dpi = args
                .first()
                .and_then(Value::as_f64)
                .unwrap_or(150.0)
                .clamp(36.0, MAX_DPI as f64) as f32;
            let limit = args
                .get(1)
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .filter(|n| *n > 0)
                .unwrap_or(MAX_PAGES)
                .min(MAX_PAGES);

            let pages = document.pages();
            if pages.len() as usize > limit {
                return Err(format!(
                    "this document has {} pages and the limit is {limit}",
                    pages.len()
                ));
            }

            let config = PdfRenderConfig::new().scale_page_by_factor(dpi / 72.0);
            let mut described = Vec::new();
            let mut payloads: Vec<Vec<u8>> = Vec::new();
            for (index, page) in pages.iter().enumerate() {
                let rendered = page
                    .render_with_config(&config)
                    .map_err(|e| format!("page {} could not be rendered ({e})", index + 1))?
                    .as_image()
                    .map_err(|e| format!("page {} rendered to nothing ({e})", index + 1))?;

                let mut png = std::io::Cursor::new(Vec::new());
                rendered
                    .write_to(&mut png, image::ImageFormat::Png)
                    .map_err(|e| format!("page {} could not be encoded ({e})", index + 1))?;

                described.push(json!({
                    "image": { "blob": payloads.len(), "source": format!("page {}", index + 1) }
                }));
                payloads.push(png.into_inner());
            }
            Ok((Value::Array(described), payloads))
        }
        other => Err(format!("this helper has no `{other}`")),
    }
}

fn read_frame(input: &mut impl Read) -> Option<(Value, Vec<Vec<u8>>)> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length).ok()?;
    let mut header = vec![0u8; u32::from_be_bytes(length) as usize];
    input.read_exact(&mut header).ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;

    let mut blobs = Vec::new();
    if let Some(Value::Array(lengths)) = header.get("blobs") {
        for length in lengths {
            let mut blob = vec![0u8; length.as_u64().unwrap_or_default() as usize];
            input.read_exact(&mut blob).ok()?;
            blobs.push(blob);
        }
    }
    Some((header, blobs))
}

fn write_frame(
    output: &mut impl Write,
    mut payload: Value,
    blobs: &[Vec<u8>],
) -> std::io::Result<()> {
    payload["blobs"] = json!(blobs.iter().map(Vec::len).collect::<Vec<_>>());
    let encoded = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    output.write_all(&(encoded.len() as u32).to_be_bytes())?;
    output.write_all(&encoded)?;
    for blob in blobs {
        output.write_all(blob)?;
    }
    output.flush()
}
