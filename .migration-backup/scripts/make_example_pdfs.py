#!/usr/bin/env python3
"""Build the PDFs `examples/21_pdf.ko` reads.

The example needs two documents: one with a real text layer, and one whose
pages carry no text at all -- what a scan looks like to any extractor. Both
are generated rather than committed as opaque binaries, so what is in them is
readable here instead of only in a hex dump.

    python3 scripts/make_example_pdfs.py

PDF is a text format with a byte-offset table at the end, so this writes the
objects first and records where each one started. No dependencies.
"""

import pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / "examples" / "documents"

HANDBOOK = [
    [
        "Northwind Supply - Delivery Policy",
        "",
        "1. Orders placed before 16:00 ship the same working day.",
        "2. Standard delivery is 3 working days. Express is next day.",
        "3. A delivery attempt is made twice before the parcel returns.",
    ],
    [
        "Northwind Supply - Returns",
        "",
        "4. Unopened goods may be returned within 30 days.",
        "5. Refunds are issued to the original payment method.",
        "6. Return postage is paid by the customer unless the item is faulty.",
    ],
]


def text_operations(lines):
    """The content stream for one page: Helvetica, one line every 22 points."""
    if not lines:
        # A page with no text operations at all. An extractor finds nothing
        # here, exactly as it would on a scanned page.
        return b""
    out = ["BT", "/F1 13 Tf", "1 0 0 1 60 760 Tm", "22 TL"]
    for line in lines:
        escaped = line.replace("\\", r"\\").replace("(", r"\(").replace(")", r"\)")
        out.append(f"({escaped}) Tj")
        out.append("T*")
    out.append("ET")
    return "\n".join(out).encode("latin-1")


def build(pages):
    """Serialize a one-font PDF whose pages hold `pages[i]` lines of text."""
    objects = []  # index 0 is object 1

    font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"
    # 1 catalog, 2 pages tree, 3 font, then per page: a page and a stream.
    page_ids = [4 + 2 * i for i in range(len(pages))]
    kids = " ".join(f"{n} 0 R" for n in page_ids)

    objects.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    objects.append(
        f"<< /Type /Pages /Kids [{kids}] /Count {len(pages)} >>".encode("latin-1")
    )
    objects.append(font)

    for index, lines in enumerate(pages):
        stream = text_operations(lines)
        page_id = page_ids[index]
        objects.append(
            (
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] "
                f"/Resources << /Font << /F1 3 0 R >> >> "
                f"/Contents {page_id + 1} 0 R >>"
            ).encode("latin-1")
        )
        objects.append(
            b"<< /Length "
            + str(len(stream)).encode("latin-1")
            + b" >>\nstream\n"
            + stream
            + b"\nendstream"
        )

    out = bytearray(b"%PDF-1.5\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n".encode("latin-1") + body + b"\nendobj\n"

    start_xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode("latin-1")
    out += b"0000000000 65535 f \n"
    for offset in offsets:
        out += f"{offset:010d} 00000 n \n".encode("latin-1")
    out += (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{start_xref}\n%%EOF\n"
    ).encode("latin-1")
    return bytes(out)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "handbook.pdf").write_bytes(build(HANDBOOK))
    # Two pages, no text operations on either: the shape of a scan.
    (OUT / "scanned.pdf").write_bytes(build([[], []]))
    for name in ("handbook.pdf", "scanned.pdf"):
        print(f"wrote examples/documents/{name}")


if __name__ == "__main__":
    main()
