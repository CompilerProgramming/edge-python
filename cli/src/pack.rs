use std::collections::HashMap;

pub use compiler::modules::bundle::{Bundle, Entry, MAGIC};

// Marks an eval body that carries a base64 project bundle rather than a raw snippet.
pub const BUNDLE_TAG: &str = "EDGEPKG:";

/* The files keyed by path, the tree an in-memory resolver serves. */
pub fn into_files(bundle: Bundle) -> HashMap<String, Vec<u8>> {
    bundle.files.into_iter().map(|f| (f.path, f.bytes)).collect()
}

/* Standard base64 with padding, the wire form of a bundle sent to an eval group. */
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_with_and_without_padding() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("RURHRVBLRwE=").unwrap(), MAGIC);
        assert!(base64_decode("not base64!").is_none());
    }
}
