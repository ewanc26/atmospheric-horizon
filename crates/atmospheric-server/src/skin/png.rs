use atmospheric::records::MAX_SKIN_BYTES;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// True when the bytes start with the PNG signature.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_MAGIC.len() && bytes[..PNG_MAGIC.len()] == PNG_MAGIC
}

/// Validate one skin or cape upload against the lexicon's blob cap and PNG
/// signature. The browser accepts only `image/png`; a renamed file still has
/// to pass this check.
pub fn validate_skin(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.is_empty() {
        return Err("Choose a PNG file first.");
    }
    if bytes.len() > MAX_SKIN_BYTES {
        return Err("The PNG must be at most 1 MiB.");
    }
    if !is_png(bytes) {
        return Err("Only PNG files are accepted.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_and_size_are_enforced() {
        let mut png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        assert!(is_png(&png));
        assert!(validate_skin(&png).is_ok());
        png.truncate(8);
        assert!(is_png(&png));
        assert!(validate_skin(b"not a png").is_err());
        assert!(validate_skin(&vec![0u8; MAX_SKIN_BYTES + 1]).is_err());
        assert!(validate_skin(&[]).is_err());
    }
}
