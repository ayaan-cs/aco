//! Reading Python source files: encoding, BOM and line endings.
//!
//! aco works on UTF-8 text with `\n` line endings. A UTF-8 byte-order mark is
//! dropped and CRLF endings are converted on the way in, and both are put
//! back when a result is written, so the output file matches the input's
//! conventions. Other encodings are refused with a conversion hint rather than
//! guessed.

/// A decoded source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The text, without BOM and with `\n` line endings.
    pub text: String,
    pub had_bom: bool,
    pub crlf: bool,
}

impl Source {
    /// Re-encode `text` (with `\n` endings) the way the original was stored.
    pub fn restore(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() + 3);
        if self.had_bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        if self.crlf {
            out.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
        } else {
            out.extend_from_slice(text.as_bytes());
        }
        out
    }
}

/// The PEP 263 encoding declared in the first two lines, if any.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    for line in bytes.split(|b| *b == b'\n').take(2) {
        let line = String::from_utf8_lossy(line);
        let t = line.trim_start_matches([' ', '\t', '\x0c']);
        if !t.starts_with('#') {
            continue;
        }
        if let Some(i) = t.find("coding") {
            let rest = &t[i + 6..];
            if let Some(rest) = rest.strip_prefix([':', '=']) {
                let enc: String = rest
                    .trim_start_matches([' ', '\t'])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .collect();
                if !enc.is_empty() {
                    return Some(enc);
                }
            }
        }
    }
    None
}

fn is_utf8_name(enc: &str) -> bool {
    let e = enc.to_ascii_lowercase().replace('_', "-");
    e == "utf-8" || e == "utf8" || e.starts_with("utf-8-") || e == "ascii" || e == "us-ascii"
}

/// Decode `bytes` as Python source. Errors are complete sentences naming the
/// problem and the fix.
pub fn decode(bytes: &[u8]) -> Result<Source, String> {
    let (body, had_bom) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(b) => (b, true),
        None => (bytes, false),
    };
    if let Some(enc) = declared_encoding(body) {
        if !is_utf8_name(&enc) {
            return Err(format!(
                "it declares `coding: {enc}`; aco only reads UTF-8 source. Convert it with \
                 `iconv -f {enc} -t utf-8 FILE > FILE.utf8` and drop the coding line"
            ));
        }
    }
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            let at = e.valid_up_to();
            let line = body[..at].iter().filter(|b| **b == b'\n').count() + 1;
            let byte = body.get(at).copied().unwrap_or(0);
            if body[..body.len().min(8000)].contains(&0) {
                return Err("it is not text (it contains NUL bytes)".into());
            }
            return Err(format!(
                "it is not UTF-8 text (byte 0x{byte:02X} on line {line}). If it is Latin-1 or \
                 cp1252, convert it with `iconv -f cp1252 -t utf-8 FILE > FILE.utf8`"
            ));
        }
    };
    if text.contains('\0') {
        return Err("it is not text (it contains NUL bytes)".into());
    }
    let crlf = text.contains("\r\n");
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    };
    Ok(Source {
        text,
        had_bom,
        crlf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_and_crlf_round_trip() {
        let raw = b"\xEF\xBB\xBFx = 1\r\nprint(x)\r\n";
        let s = decode(raw).unwrap();
        assert_eq!(s.text, "x = 1\nprint(x)\n");
        assert!(s.had_bom && s.crlf);
        assert_eq!(s.restore(&s.text), raw.to_vec());
    }

    #[test]
    fn non_utf8_is_refused_with_a_hint() {
        let e = decode(b"x = 1\ns = '\xe9'\n").unwrap_err();
        assert!(
            e.contains("byte 0xE9 on line 2") && e.contains("iconv"),
            "{e}"
        );
        let e = decode(b"# -*- coding: latin-1 -*-\nx = 1\n").unwrap_err();
        assert!(e.contains("coding: latin-1"), "{e}");
        assert!(decode(b"# coding: utf-8\nx = 1\n").is_ok());
        assert!(decode(b"\x7fELF\0\0\0").unwrap_err().contains("NUL"));
    }
}
