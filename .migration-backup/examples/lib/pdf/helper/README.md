# kora-pdf-helper

The `pdf_render` package's helper: PDF bytes in, one PNG per page out.

It is a separate program on purpose. Rendering a page needs a real renderer,
which means PDFium — half a million lines of C++ parsing a file the program
did not write. In its own process a bug in it is an error Kora reports; linked
into the interpreter it would share an address space with every label,
capability, and journal entry the run holds.

It has its own `Cargo.toml` with an empty `[workspace]`, so the Kora
workspace never builds it and `cargo build` at the repository root never
touches PDFium.

## Building it

```bash
cd examples/lib/pdf/helper
cargo build --release
```

Then put PDFium beside the binary. Prebuilt libraries come from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries);
take the archive for your platform (`pdfium-mac-arm64`, `pdfium-linux-x64`,
`pdfium-win-x64`, …) and copy the library out of its `lib/` into
`target/release/`:

```bash
# macOS arm64, as an example
curl -L -o pdfium.tgz \
  https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-mac-arm64.tgz
mkdir -p pdfium && tar xzf pdfium.tgz -C pdfium
cp pdfium/lib/libpdfium.dylib target/release/
```

The helper looks for the library beside itself first, then wherever the system
keeps one. `examples/lib/pdf/kora.toml` points at `target/release/` while the
helper is being developed.

## Releasing it

A published package names an archive and its hash instead of a path:

```toml
[package.helper.aarch64-apple-darwin]
url = "https://.../kora-pdf-helper-0.1.0-aarch64-apple-darwin.tar.gz"
sha256 = "..."
binary = "kora-pdf-helper"
```

The archive holds the binary and the PDFium library together. `kora install`
downloads only the entry for the machine it is running on, refuses anything
whose bytes hash to something else, and records what arrived in `kora.sums`.
Nothing is executed to install it.

## The protocol

`stdio/v1`, defined in `crates/kora-helper`. A big-endian length, a JSON
header, then the payloads the header declares.

| call | arguments | answers |
|---|---|---|
| `render` | `dpi`, `max_pages`, and the document | one `{"image": {...}}` per page |
| `page_count` | the document | how many pages |
