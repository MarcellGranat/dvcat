//! Copying text to the system clipboard.
//!
//! The platform's own tool when there is one (`pbcopy`, `wl-copy`, `xclip`, `xsel`, `clip`);
//! otherwise the terminal is asked through the OSC 52 escape sequence, which many terminals
//! (kitty, iTerm2, WezTerm, tmux with `set-clipboard on`, ...) honour even over SSH.

use std::{
    io::Write,
    process::{Command, Stdio},
};

const TOOLS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("clip", &[]),
];

/// Copy `text`. Returns how it was done (for the notice).
pub fn copy(text: &str) -> Result<&'static str, String> {
    for (program, args) in TOOLS {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take()
            && stdin.write_all(text.as_bytes()).is_err()
        {
            continue;
        }
        if child.wait().is_ok_and(|status| status.success()) {
            return Ok(program);
        }
    }
    let mut out = std::io::stdout();
    out.write_all(osc52(text).as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| format!("cannot copy: {error}"))?;
    Ok("the terminal (OSC 52)")
}

/// The escape sequence that asks the terminal to put `text` on the clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(ALPHABET[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_the_encoded_text() {
        assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    }
}
