//! Compound text encoder and decoder used by the Linux XIM client.

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "std")]
use std::io::{self, Write};

const UTF8_START: &[u8] = &[0x1B, 0x25, 0x47];
const UTF8_END: &[u8] = &[0x1B, 0x25, 0x40];

/// A UTF-8 string that can be written as COMPOUND_TEXT.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct CText<'s> {
    utf8: &'s str,
}

impl<'s> fmt::Debug for CText<'s> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.utf8)
    }
}

impl<'s> fmt::Display for CText<'s> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.utf8)
    }
}

impl<'s> CText<'s> {
    pub const fn new(utf8: &'s str) -> Self {
        Self { utf8 }
    }

    pub const fn len(self) -> usize {
        self.utf8.len() + UTF8_START.len() + UTF8_END.len()
    }

    pub const fn is_empty(self) -> bool {
        self.utf8.is_empty()
    }

    #[cfg(feature = "std")]
    pub fn write(self, mut output: impl Write) -> io::Result<usize> {
        let mut written = 0;
        written += output.write(UTF8_START)?;
        written += output.write(self.utf8.as_bytes())?;
        written += output.write(UTF8_END)?;
        Ok(written)
    }
}

/// Encodes UTF-8 as COMPOUND_TEXT using the UTF-8 escape sequence.
pub fn utf8_to_compound_text(text: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(text.len() + UTF8_START.len() + UTF8_END.len());
    result.extend_from_slice(UTF8_START);
    result.extend_from_slice(text.as_bytes());
    result.extend_from_slice(UTF8_END);
    result
}

#[derive(Debug, Clone)]
pub enum DecodeError {
    InvalidEncoding,
    UnsupportedEncoding,
    Utf8Error(alloc::string::FromUtf8Error),
}

impl From<alloc::string::FromUtf8Error> for DecodeError {
    fn from(error: alloc::string::FromUtf8Error) -> Self {
        Self::Utf8Error(error)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => write!(formatter, "Invalid compound text"),
            Self::UnsupportedEncoding => write!(formatter, "This encoding is not supported yet"),
            Self::Utf8Error(error) => write!(formatter, "Not a valid utf8 {error}"),
        }
    }
}

macro_rules! decode {
    ($decoder:expr, $output:expr, $bytes:expr, $last:expr) => {
        loop {
            let (result, _, _) = $decoder.decode_to_string($bytes, $output, $last);
            match result {
                encoding_rs::CoderResult::InputEmpty => break,
                encoding_rs::CoderResult::OutputFull => {
                    $output.reserve(
                        $decoder
                            .max_utf8_buffer_length($bytes.len())
                            .unwrap_or_default(),
                    );
                }
            }
        }
    };
}

pub fn compound_text_to_utf8(bytes: &[u8]) -> Result<String, DecodeError> {
    let Some(first) = bytes.first() else {
        return Ok(String::new());
    };

    if *first != 0x1B {
        // COMPOUND_TEXT starts in ISO-8859-1. Many input methods send ASCII
        // and UTF-8 here as an extension, so retain valid UTF-8 and decode an
        // invalid payload with the protocol's single-byte default repertoire.
        return match String::from_utf8(bytes.to_vec()) {
            Ok(text) => Ok(text),
            Err(_) => Ok(bytes.iter().map(|byte| char::from(*byte)).collect()),
        };
    }

    let Some(escape) = bytes.get(1..3) else {
        return Err(DecodeError::InvalidEncoding);
    };
    match escape {
        // UTF-8
        [0x25, 0x47] => {
            let payload = bytes
                .get(3..)
                .and_then(|payload| payload.strip_suffix(UTF8_END))
                .ok_or(DecodeError::InvalidEncoding)?;
            Ok(String::from_utf8(payload.to_vec())?)
        }
        // 94N
        [0x24, 0x28] => match bytes.get(3) {
            // JP
            Some(0x42) => {
                let payload = bytes.get(4..).ok_or(DecodeError::InvalidEncoding)?;
                let mut decoder = encoding_rs::ISO_2022_JP.new_decoder_without_bom_handling();
                let mut output = String::new();

                decode!(decoder, &mut output, &[0x1B, 0x24, 0x42], false);
                decode!(decoder, &mut output, payload, true);
                Ok(output)
            }
            // CN and KR
            Some(0x41 | 0x43) => Err(DecodeError::UnsupportedEncoding),
            _ => Err(DecodeError::InvalidEncoding),
        },
        _ => Err(DecodeError::InvalidEncoding),
    }
}

#[cfg(test)]
mod tests {
    use super::{compound_text_to_utf8, utf8_to_compound_text, DecodeError};

    #[test]
    fn decodes_utf8_escape() {
        const UTF8: &str = "가나다";
        const COMPOUND: &[u8] = &[
            27, 37, 71, 234, 176, 128, 235, 130, 152, 235, 139, 164, 27, 37, 64,
        ];
        assert_eq!(utf8_to_compound_text(UTF8), COMPOUND);
        assert_eq!(compound_text_to_utf8(COMPOUND).unwrap(), UTF8);
    }

    #[test]
    fn decodes_iso_2022_jp() {
        const COMPOUND: &[u8] = &[27, 36, 40, 66, 69, 108, 53, 126];
        assert_eq!(compound_text_to_utf8(COMPOUND).unwrap(), "東京");
    }

    #[test]
    fn decodes_unescaped_latin_1_from_xim() {
        assert_eq!(compound_text_to_utf8(&[0xE9]).unwrap(), "é");
    }

    #[test]
    fn preserves_unescaped_utf8_from_xim() {
        assert_eq!(compound_text_to_utf8("é".as_bytes()).unwrap(), "é");
    }

    #[test]
    fn rejects_truncated_utf8_escape_without_panicking() {
        assert!(matches!(
            compound_text_to_utf8(&[0x1B, 0x25, 0x47]),
            Err(DecodeError::InvalidEncoding)
        ));
    }
}
