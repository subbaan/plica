//! Text encodings: detection, lossless decoding and encoding back.
//!
//! The buffer is always UTF-8. A file is decoded from the encoding it was
//! written in and saved back in that same encoding:
//!
//! 1. A byte-order mark decides (UTF-8, UTF-16 LE/BE). UTF-32 is refused.
//! 2. BOM-less UTF-16 is recognised by its pattern of zero bytes.
//! 3. Valid UTF-8 is UTF-8 (the fast path: nothing more is checked).
//! 4. Any other file with NUL bytes is binary and refused.
//! 5. Mostly-valid UTF-8 with a few stray bytes stays UTF-8.
//! 6. Anything else is a legacy encoding: the configured fallback, else
//!    chardetng's guess (Firefox's detector).
//!
//! Bytes the encoding can't decode become placeholder chars (U+10FC00 plus
//! the byte, drawn as `\xNN`) that are written back as the same byte. Every
//! decode outside the UTF-8 fast path is verified by encoding the text again
//! and comparing with the file; if the bytes differ, the document is marked
//! inexact and saving over the original asks first.

use encoding_rs::{DecoderResult, EncoderResult, Encoding as Enc, GB18030, GBK, UTF_8, UTF_16BE, UTF_16LE};
use std::fmt;

/// Placeholder chars for undecodable bytes: U+10FC00 + byte (private use).
const RAW_BASE: u32 = 0x10_FC00;
/// chardetng reads at most this much of a file.
const GUESS_LIMIT: usize = 4 << 20;

pub fn raw_byte(c: char) -> Option<u8> {
    let u = c as u32;
    (RAW_BASE..=RAW_BASE + 0xFF).contains(&u).then(|| (u - RAW_BASE) as u8)
}

fn raw_char(b: u8) -> char {
    char::from_u32(RAW_BASE + u32::from(b)).expect("valid private-use scalar")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding {
    pub enc: &'static Enc,
    /// Write a byte-order mark (UTF-8 / UTF-16 only).
    pub bom: bool,
    /// The text may hold raw-byte placeholders to write back as bytes.
    pub raw: bool,
}

impl Default for Encoding {
    fn default() -> Self {
        Self::UTF8
    }
}

/// A char the target encoding has no bytes for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmappable {
    pub ch: char,
    /// Byte offset of `ch` in the text.
    pub offset: usize,
    pub encoding: String,
}

impl fmt::Display for Unmappable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\u{2018}{}\u{2019} (U+{:04X}) can't be stored in {}", self.ch, self.ch as u32, self.encoding)
    }
}

impl std::error::Error for Unmappable {}

impl Encoding {
    pub const UTF8: Encoding = Encoding { enc: UTF_8, bom: false, raw: false };

    pub fn new(enc: &'static Enc) -> Self {
        Self { enc, bom: false, raw: false }
    }

    pub fn is_utf8(self) -> bool {
        self.enc == UTF_8
    }

    pub fn is_utf16(self) -> bool {
        self.enc == UTF_16LE || self.enc == UTF_16BE
    }

    /// Status-bar name, e.g. "UTF-8", "UTF-8 BOM", "UTF-16 LE", "windows-1252".
    pub fn label(self) -> String {
        let base = match self.enc {
            e if e == UTF_16LE => "UTF-16 LE",
            e if e == UTF_16BE => "UTF-16 BE",
            e => e.name(),
        };
        if self.bom { format!("{base} BOM") } else { base.to_string() }
    }

    /// Stable name for config and recovery files, e.g. "utf-8-bom",
    /// "windows-1252", "utf-8-raw". Read back by `parse`.
    pub fn config_name(self) -> String {
        let mut s = self.enc.name().to_ascii_lowercase();
        if self.bom {
            s.push_str("-bom");
        }
        if self.raw {
            s.push_str("-raw");
        }
        s
    }

    /// Any WHATWG label ("latin1", "cp1252", "sjis", "utf8", …), optionally
    /// followed by "-bom" (and "-raw", as written by `config_name`).
    pub fn parse(name: &str) -> Option<Self> {
        let mut s = name.trim().to_ascii_lowercase();
        let raw = s.ends_with("-raw") && {
            s.truncate(s.len() - 4);
            true
        };
        let bom = (s.ends_with("-bom") || s.ends_with(" bom")) && {
            s.truncate(s.len() - 4);
            true
        };
        let enc = Enc::for_label_no_replacement(s.as_bytes())?;
        // x-user-defined is a browser artefact, not a text encoding.
        if enc == encoding_rs::X_USER_DEFINED {
            return None;
        }
        let bom = bom && (enc == UTF_8 || enc == UTF_16LE || enc == UTF_16BE);
        Some(Self { enc, bom, raw })
    }

    fn bom_bytes(self) -> &'static [u8] {
        match self.enc {
            _ if !self.bom => b"",
            e if e == UTF_8 => b"\xEF\xBB\xBF",
            e if e == UTF_16LE => b"\xFF\xFE",
            e if e == UTF_16BE => b"\xFE\xFF",
            _ => b"",
        }
    }

    /// The file's bytes for `text`, BOM included.
    pub fn encode(self, text: &str) -> Result<Vec<u8>, Unmappable> {
        let mut out = Vec::with_capacity(text.len() + 4);
        out.extend_from_slice(self.bom_bytes());
        if self.is_utf8() && !self.raw {
            out.extend_from_slice(text.as_bytes());
            return Ok(out);
        }
        if self.is_utf16() {
            let be = self.enc == UTF_16BE;
            out.reserve(text.len() * 2);
            let mut units = [0u16; 2];
            for c in text.chars() {
                if self.raw
                    && let Some(b) = raw_byte(c)
                {
                    out.push(b);
                    continue;
                }
                for u in c.encode_utf16(&mut units) {
                    out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
                }
            }
            return Ok(out);
        }
        // UTF-8 with placeholders, or a legacy encoding: encode the runs
        // between placeholders, writing each placeholder as its byte.
        let mut encoder = (!self.is_utf8()).then(|| self.enc.new_encoder());
        let mut run_start = 0;
        let runs = text.char_indices().filter_map(|(i, c)| self.raw.then(|| raw_byte(c)).flatten().map(|b| (i, c.len_utf8(), Some(b))));
        for (at, len, byte) in runs.chain(std::iter::once((text.len(), 0, None))) {
            let run = &text[run_start..at];
            match &mut encoder {
                None => out.extend_from_slice(run.as_bytes()),
                Some(e) => self.encode_run(e, run, run_start, byte.is_none(), &mut out)?,
            }
            if let Some(b) = byte {
                out.push(b);
            }
            run_start = at + len;
        }
        Ok(out)
    }

    fn encode_run(self, e: &mut encoding_rs::Encoder, mut src: &str, mut offset: usize, last: bool, out: &mut Vec<u8>) -> Result<(), Unmappable> {
        loop {
            let need = e.max_buffer_length_from_utf8_without_replacement(src.len()).unwrap_or(src.len() * 4 + 16);
            out.reserve(need.max(16));
            let (res, read) = e.encode_from_utf8_to_vec_without_replacement(src, out, last);
            match res {
                EncoderResult::InputEmpty => return Ok(()),
                EncoderResult::OutputFull => {}
                EncoderResult::Unmappable(ch) => {
                    return Err(Unmappable { ch, offset: offset + read - ch.len_utf8(), encoding: self.label() });
                }
            }
            src = &src[read..];
            offset += read;
        }
    }

    /// Decode `body` (no BOM), turning undecodable bytes into placeholders.
    /// Returns the text and the number of placeholders.
    fn decode_raw(self, body: &[u8]) -> (String, usize) {
        let mut d = self.enc.new_decoder_without_bom_handling();
        let cap = d.max_utf8_buffer_length_without_replacement(body.len()).unwrap_or(body.len() * 3);
        let mut out = String::with_capacity(cap.min(body.len() * 4 + 16));
        let (mut pos, mut raw) = (0, 0);
        loop {
            let (res, read) = d.decode_to_string_without_replacement(&body[pos..], &mut out, true);
            pos += read;
            match res {
                DecoderResult::InputEmpty => return (out, raw),
                DecoderResult::OutputFull => {
                    let more = d.max_utf8_buffer_length_without_replacement(body.len() - pos).unwrap_or(64);
                    out.reserve(more.max(64));
                }
                DecoderResult::Malformed(bad, extra) => {
                    let end = pos.saturating_sub(usize::from(extra));
                    for &b in &body[end.saturating_sub(usize::from(bad))..end] {
                        out.push(raw_char(b));
                        raw += 1;
                    }
                }
            }
        }
    }
}

/// How the encoding was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Bom,
    Utf8,
    /// BOM-less UTF-16 recognised by its zero bytes.
    Utf16Pattern,
    /// The configured fallback for files that aren't UTF-8.
    Fallback,
    /// chardetng's guess.
    Guess,
    /// Chosen by the user (`--encoding`, Reopen with).
    Chosen,
    /// The document's previous encoding, kept on reload.
    Kept,
}

#[derive(Debug, Clone)]
pub struct Detected {
    pub encoding: Encoding,
    pub text: String,
    pub how: How,
    /// Undecodable bytes kept as placeholders.
    pub raw: usize,
    /// Encoding the text again gives back the file's exact bytes.
    pub exact: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Decode with this encoding, whatever the bytes look like.
    pub force: Option<Encoding>,
    /// Keep this encoding when the bytes don't decide otherwise (reload).
    pub prefer: Option<Encoding>,
    /// For files that aren't UTF-8 or UTF-16 (None = guess).
    pub fallback: Option<&'static Enc>,
}

/// Detect and decode a file's bytes. Valid UTF-8 (with or without a BOM)
/// becomes the text without being copied.
pub fn detect(bytes: Vec<u8>, o: &Options) -> Result<Detected, String> {
    let utf8_bom = bytes.starts_with(b"\xEF\xBB\xBF");
    let plain = !utf8_bom && sniff_bom(&bytes)?.0.is_none() && utf16_pattern(&bytes).is_none();
    if o.force.is_some() || !(utf8_bom || plain) {
        return detect_slow(&bytes, o);
    }
    let mut bytes = bytes;
    if utf8_bom {
        bytes.drain(..3);
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            let mut b = e.into_bytes();
            if utf8_bom {
                b.splice(0..0, *b"\xEF\xBB\xBF");
            }
            return detect_slow(&b, o);
        }
    };
    // A legacy encoding the user picked survives a pure-ASCII reload, so
    // newly typed non-ASCII text is still saved the way they chose.
    if let Some(p) = o.prefer
        && plain
        && !p.is_utf8()
        && p.enc.is_ascii_compatible()
        && text.is_ascii()
    {
        return Ok(Detected { encoding: Encoding::new(p.enc), text, how: How::Kept, raw: 0, exact: true });
    }
    let (encoding, how) = if utf8_bom { (Encoding { bom: true, ..Encoding::UTF8 }, How::Bom) } else { (Encoding::UTF8, How::Utf8) };
    Ok(Detected { encoding, text, how, raw: 0, exact: true })
}

fn detect_slow(bytes: &[u8], o: &Options) -> Result<Detected, String> {
    let (bom_enc, body) = sniff_bom(bytes)?;
    if let Some(f) = o.force {
        let (bom, body) = if bom_enc == Some(f.enc) { (true, body) } else { (false, bytes) };
        return Ok(decode_checked(Encoding { enc: f.enc, bom, raw: false }, body, How::Chosen));
    }
    if let Some(enc) = bom_enc {
        return Ok(decode_checked(Encoding { enc, bom: true, raw: false }, body, How::Bom));
    }
    // Before the UTF-8 check: ASCII text in UTF-16 is valid UTF-8 too
    // (full of NULs). A cheap look at the first 8 KiB.
    if let Some(enc) = utf16_pattern(bytes) {
        let d = decode_checked(Encoding::new(enc), bytes, How::Utf16Pattern);
        if d.exact && d.raw == 0 {
            return Ok(d);
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        // A legacy encoding the user picked survives a pure-ASCII reload,
        // so newly typed non-ASCII text is still saved the way they chose.
        if let Some(p) = o.prefer
            && !p.is_utf8()
            && p.enc.is_ascii_compatible()
            && bytes.is_ascii()
        {
            return Ok(Detected { encoding: Encoding::new(p.enc), text: s.to_owned(), how: How::Kept, raw: 0, exact: true });
        }
        return Ok(Detected { encoding: Encoding::UTF8, text: s.to_owned(), how: How::Utf8, raw: 0, exact: true });
    }
    if bytes.contains(&0) {
        return Err("looks like a binary file (it contains NUL bytes)".into());
    }
    if let Some(p) = o.prefer.filter(|p| !p.is_utf16()) {
        let d = decode_checked(Encoding::new(p.enc), bytes, How::Kept);
        if d.exact && (d.raw == 0 || p.is_utf8()) {
            return Ok(d);
        }
    }
    if mostly_utf8(bytes) {
        let d = decode_checked(Encoding::UTF8, bytes, How::Utf8);
        if d.exact {
            return Ok(d);
        }
    }
    let (guess, how) = match o.fallback {
        Some(e) => (e, How::Fallback),
        None => (chardet(bytes), How::Guess),
    };
    let d = decode_checked(Encoding::new(guess), bytes, how);
    if d.exact && d.raw == 0 {
        return Ok(d);
    }
    if guess == GBK {
        // GBK's decoder reads GB18030's four-byte forms, its encoder can't.
        let g = decode_checked(Encoding::new(GB18030), bytes, how);
        if g.exact {
            return Ok(g);
        }
    }
    Ok(d)
}

/// Decode and verify; an inexact decode falls back to replacement chars.
fn decode_checked(enc: Encoding, body: &[u8], how: How) -> Detected {
    let (text, raw) = enc.decode_raw(body);
    let enc = Encoding { raw: raw > 0, ..enc };
    let skip = enc.bom_bytes().len();
    let exact = enc.encode(&text).is_ok_and(|b| b[skip..] == *body);
    if exact {
        return Detected { encoding: enc, text, how, raw, exact };
    }
    let (text, _) = enc.enc.decode_without_bom_handling(body);
    Detected { encoding: Encoding { raw: false, ..enc }, text: text.into_owned(), how, raw: 0, exact: false }
}

fn sniff_bom(bytes: &[u8]) -> Result<(Option<&'static Enc>, &[u8]), String> {
    if bytes.starts_with(b"\x00\x00\xFE\xFF") || (bytes.starts_with(b"\xFF\xFE\x00\x00") && bytes.len().is_multiple_of(4)) {
        return Err("is UTF-32, which Plica doesn't support (convert it with: iconv -f UTF-32 -t UTF-8)".into());
    }
    Ok(match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => (Some(UTF_8), rest),
        [0xFF, 0xFE, rest @ ..] => (Some(UTF_16LE), rest),
        [0xFE, 0xFF, rest @ ..] => (Some(UTF_16BE), rest),
        _ => (None, bytes),
    })
}

/// BOM-less UTF-16: Latin-script text has a zero in every other byte.
fn utf16_pattern(bytes: &[u8]) -> Option<&'static Enc> {
    let n = bytes.len().min(8192) & !1;
    if n < 4 || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let pairs = n / 2;
    let (mut even, mut odd) = (0, 0);
    for p in bytes[..n].as_chunks::<2>().0 {
        even += usize::from(p[0] == 0);
        odd += usize::from(p[1] == 0);
    }
    if odd * 3 >= pairs && even * 20 <= pairs {
        Some(UTF_16LE)
    } else if even * 3 >= pairs && odd * 20 <= pairs {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// UTF-8 with a few stray bytes: at least as many valid multi-byte
/// characters as invalid bytes. (Latin-1 text almost never forms valid
/// UTF-8 sequences, so it fails this.)
fn mostly_utf8(bytes: &[u8]) -> bool {
    let (mut good, mut bad) = (0usize, 0usize);
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                good += s.bytes().filter(|&b| b >= 0xC0).count();
                break;
            }
            Err(e) => {
                let (ok, after) = rest.split_at(e.valid_up_to());
                good += ok.iter().filter(|&&b| b >= 0xC0).count();
                let skip = e.error_len().unwrap_or(after.len());
                bad += skip;
                rest = &after[skip..];
            }
        }
    }
    good > 0 && bad <= good
}

fn chardet(bytes: &[u8]) -> &'static Enc {
    let mut d = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    let n = bytes.len().min(GUESS_LIMIT);
    d.feed(&bytes[..n], n == bytes.len());
    d.guess(None, chardetng::Utf8Detection::Deny)
}

/// Legacy encodings offered in menus, with a plain description.
pub const LEGACY: &[(&str, &str)] = &[
    ("windows-1252", "Western European (also Latin-1, ISO-8859-1)"),
    ("ISO-8859-15", "Western European with €"),
    ("macintosh", "Western European (Mac OS Roman)"),
    ("windows-1250", "Central European"),
    ("ISO-8859-2", "Central European"),
    ("windows-1251", "Cyrillic"),
    ("KOI8-R", "Russian"),
    ("KOI8-U", "Ukrainian"),
    ("IBM866", "Cyrillic (DOS)"),
    ("ISO-8859-5", "Cyrillic"),
    ("x-mac-cyrillic", "Cyrillic (Mac)"),
    ("windows-1253", "Greek"),
    ("ISO-8859-7", "Greek"),
    ("windows-1254", "Turkish (also ISO-8859-9)"),
    ("windows-1255", "Hebrew"),
    ("ISO-8859-8", "Hebrew (visual)"),
    ("ISO-8859-8-I", "Hebrew (logical)"),
    ("windows-1256", "Arabic"),
    ("ISO-8859-6", "Arabic"),
    ("windows-1257", "Baltic"),
    ("ISO-8859-4", "Baltic"),
    ("ISO-8859-13", "Baltic"),
    ("windows-1258", "Vietnamese"),
    ("windows-874", "Thai"),
    ("ISO-8859-3", "South European"),
    ("ISO-8859-10", "Nordic"),
    ("ISO-8859-14", "Celtic"),
    ("ISO-8859-16", "South-Eastern European"),
    ("Shift_JIS", "Japanese"),
    ("EUC-JP", "Japanese"),
    ("ISO-2022-JP", "Japanese (7-bit)"),
    ("GBK", "Chinese, Simplified"),
    ("gb18030", "Chinese, Simplified (full Unicode)"),
    ("Big5", "Chinese, Traditional"),
    ("EUC-KR", "Korean"),
];

/// Choices for the fallback encoding setting: auto (None), then LEGACY.
pub fn fallback_choices() -> Vec<Option<&'static Enc>> {
    std::iter::once(None).chain(LEGACY.iter().filter_map(|(n, _)| Enc::for_label(n.as_bytes())).map(Some)).collect()
}

/// Choices for "Reopen with" (`saving` = false) or "Save with" menus.
pub fn menu(saving: bool) -> Vec<(Encoding, String)> {
    let mut v = vec![(Encoding::UTF8, "Unicode (recommended)".to_string())];
    if saving {
        v.push((Encoding { bom: true, ..Encoding::UTF8 }, "Unicode, with byte-order mark".into()));
    }
    for (enc, what) in [(UTF_16LE, "Unicode, little-endian (Windows)"), (UTF_16BE, "Unicode, big-endian")] {
        v.push((Encoding { enc, bom: saving, raw: false }, what.into()));
    }
    for (name, what) in LEGACY {
        if let Some(enc) = Enc::for_label(name.as_bytes()) {
            v.push((Encoding::new(enc), (*what).into()));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(bytes: &[u8], o: &Options) -> Result<Detected, String> {
        detect(bytes.to_vec(), o)
    }

    fn auto(bytes: &[u8]) -> Detected {
        det(bytes, &Options::default()).unwrap()
    }

    fn round_trip(d: &Detected) -> Vec<u8> {
        d.encoding.encode(&d.text).unwrap()
    }

    #[test]
    fn utf8_and_boms() {
        let d = auto("héllo".as_bytes());
        assert_eq!((d.encoding, d.how, d.text.as_str()), (Encoding::UTF8, How::Utf8, "héllo"));
        let d = auto(b"\xEF\xBB\xBFhi");
        assert_eq!((d.encoding.label(), d.text.as_str()), ("UTF-8 BOM".into(), "hi"));
        assert_eq!(round_trip(&d), b"\xEF\xBB\xBFhi");
        for (bytes, label) in [(&b"\xFF\xFEh\x00\xe9\x00"[..], "UTF-16 LE BOM"), (b"\xFE\xFF\x00h\x00\xe9", "UTF-16 BE BOM")] {
            let d = auto(bytes);
            assert_eq!((d.encoding.label(), d.text.as_str(), d.how), (label.to_string(), "hé", How::Bom));
            assert_eq!(round_trip(&d), bytes);
        }
        // Emoji: a surrogate pair.
        let d = auto(b"\xFF\xFE\x3D\xD8\x00\xDE");
        assert_eq!(d.text, "😀");
        assert!(det(b"\x00\x00\xFE\xFFabcd", &Options::default()).unwrap_err().contains("UTF-32"));
    }

    #[test]
    fn bomless_utf16_and_binary() {
        let le: Vec<u8> = "Hello, world\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let d = auto(&le);
        assert_eq!((d.encoding.label(), d.how, d.text.as_str()), ("UTF-16 LE".into(), How::Utf16Pattern, "Hello, world\r\n"));
        assert_eq!(round_trip(&d), le);
        let be: Vec<u8> = "Hello".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(auto(&be).encoding.label(), "UTF-16 BE");
        assert!(det(b"\x7fELF\x02\x01\x00\x00\xff\xfe", &Options::default()).unwrap_err().contains("binary"));
        // Valid UTF-8 with NULs is still text (as before).
        assert_eq!(auto(b"a\x00b").text, "a\0b");
    }

    #[test]
    fn legacy_guess_round_trips() {
        let latin1 = b"Le caf\xe9 \xe9tait d\xe9j\xe0 ferm\xe9, na\xefve fa\xe7ade.\n";
        let d = auto(latin1);
        assert_eq!((d.encoding.enc.name(), d.how, d.exact), ("windows-1252", How::Guess, true));
        assert!(d.text.contains("café était déjà"), "{}", d.text);
        assert_eq!(round_trip(&d), latin1);

        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のテキストファイルです。これは文字コードの判定のテストです。");
        let d = auto(&sjis);
        assert_eq!(d.encoding.enc.name(), "Shift_JIS");
        assert_eq!(round_trip(&d), &sjis[..]);

        let (cyr, _, _) = encoding_rs::WINDOWS_1251.encode("Привет, мир! Это проверка кодировки текста.");
        assert_eq!(auto(&cyr).encoding.enc.name(), "windows-1251");
    }

    #[test]
    fn fallback_force_and_prefer() {
        let o = Options { fallback: Some(encoding_rs::ISO_8859_15), ..Options::default() };
        let d = det(b"\xa4 5", &o).unwrap();
        assert_eq!((d.text.as_str(), d.how), ("€ 5", How::Fallback));
        // UTF-8 files ignore the fallback.
        assert_eq!(det("é".as_bytes(), &o).unwrap().encoding, Encoding::UTF8);

        let o = Options { force: Some(Encoding::new(encoding_rs::WINDOWS_1252)), ..Options::default() };
        let d = det("é".as_bytes(), &o).unwrap();
        assert_eq!((d.text.as_str(), d.how), ("Ã©", How::Chosen));

        // A chosen legacy encoding survives a reload of pure ASCII...
        let o = Options { prefer: Some(Encoding::new(encoding_rs::WINDOWS_1250)), ..Options::default() };
        assert_eq!(det(b"abc", &o).unwrap().encoding.enc.name(), "windows-1250");
        // ...but not a file that became real UTF-8.
        assert!(det("ł".as_bytes(), &o).unwrap().encoding.is_utf8());
        // And a non-UTF-8 reload keeps it rather than guessing again.
        assert_eq!(det(b"\xb3\xf3d\xbc", &o).unwrap().how, How::Kept);
    }

    #[test]
    fn stray_bytes_in_utf8_are_kept_exactly() {
        let bytes = "naïve – café ".as_bytes().iter().copied().chain(*b"\xe9 end\xff").collect::<Vec<u8>>();
        let d = auto(&bytes);
        assert!(d.encoding.is_utf8() && d.encoding.raw && d.exact, "{d:?}");
        assert_eq!(d.raw, 2);
        assert!(d.text.starts_with("naïve – café "));
        assert_eq!(raw_byte(d.text.chars().nth(13).unwrap()), Some(0xe9));
        assert_eq!(round_trip(&d), bytes);
        // Edited text keeps the stray bytes.
        let edited = d.text.replace("end", "fin");
        assert_eq!(d.encoding.encode(&edited).unwrap(), bytes.replace_bytes(b"end", b"fin"));
    }

    trait ReplaceBytes {
        fn replace_bytes(&self, a: &[u8], b: &[u8]) -> Vec<u8>;
    }
    impl ReplaceBytes for Vec<u8> {
        fn replace_bytes(&self, a: &[u8], b: &[u8]) -> Vec<u8> {
            let i = self.windows(a.len()).position(|w| w == a).unwrap();
            [&self[..i], b, &self[i + a.len()..]].concat()
        }
    }

    #[test]
    fn unmappable_reports_char_and_offset() {
        let e = Encoding::new(encoding_rs::WINDOWS_1252);
        assert_eq!(e.encode("café").unwrap(), b"caf\xe9");
        let err = e.encode("ab\n€ ok ✓").unwrap_err();
        assert_eq!((err.ch, err.offset), ('✓', "ab\n€ ok ".len()));
        assert!(err.to_string().contains("windows-1252"));
        // Placeholders are only bytes when the document was decoded with them.
        let p = raw_char(0xe9).to_string();
        assert!(Encoding::new(encoding_rs::WINDOWS_1252).encode(&p).is_err());
        assert_eq!(Encoding { raw: true, ..Encoding::UTF8 }.encode(&p).unwrap(), b"\xe9");
        assert_eq!(Encoding::UTF8.encode(&p).unwrap(), p.as_bytes());
    }

    #[test]
    fn names_parse_back() {
        for e in menu(true).into_iter().map(|(e, _)| e).chain([Encoding { raw: true, ..Encoding::UTF8 }]) {
            assert_eq!(Encoding::parse(&e.config_name()), Some(e), "{}", e.config_name());
        }
        assert_eq!(Encoding::parse("latin1").unwrap().enc.name(), "windows-1252");
        assert_eq!(Encoding::parse("UTF-8 BOM").unwrap().label(), "UTF-8 BOM");
        assert_eq!(Encoding::parse("sjis").unwrap().enc.name(), "Shift_JIS");
        assert!(Encoding::parse("klingon").is_none());
        assert!(Encoding::parse("x-user-defined").is_none());
        assert_eq!(menu(false).len() + 1, menu(true).len());
    }

    #[test]
    fn iso_2022_jp_and_gb18030() {
        let text = "これはテスト";
        let (bytes, _, _) = encoding_rs::ISO_2022_JP.encode(text);
        let o = Options { force: Some(Encoding::new(encoding_rs::ISO_2022_JP)), ..Options::default() };
        let d = det(&bytes, &o).unwrap();
        assert_eq!((d.text.as_str(), d.exact), (text, true));
        assert_eq!(round_trip(&d), &bytes[..]);
        let (g, _, _) = GB18030.encode("中文文本，包含一个罕见字符：𠀀，以及更多的中文内容用于检测。");
        let d = auto(&g);
        assert!(d.exact, "{:?}", d.encoding);
        assert_eq!(round_trip(&d), &g[..]);
    }
}
