//! Standard base64, because an image reaches a provider as text.
//!
//! Hand-rolled rather than pulled in as a dependency: the encoder is twenty
//! lines and fully specified, and a language runtime that ships a compiler,
//! a debug adapter, and a language server does not need a crate for it.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 base64 with padding.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn known_vectors() {
        // RFC 4648 section 10.
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn binary_bytes_use_the_full_alphabet() {
        // 0xfb 0xff exercises the `+` and `/` end of the table, which a
        // text-only test never reaches.
        assert_eq!(encode(&[0xfb, 0xff, 0xfe]), "+//+");
        assert_eq!(encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn length_is_always_a_multiple_of_four() {
        for len in 0..64 {
            let bytes = vec![0x41u8; len];
            assert_eq!(encode(&bytes).len() % 4, 0, "len {len}");
        }
    }
}
