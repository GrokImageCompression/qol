//! Voice command parsing.
//!
//! Each completed Aavaaz segment is fed through [`parse`] before going to
//! the polish/inject pipeline. Whole-segment matches become a structured
//! [`Command`]; anything else falls through as plain text.
//!
//! We deliberately only match the entire (trimmed, lower-cased, punctuation-
//! stripped) utterance — partial matches mid-sentence are ambiguous ("new
//! line" might be literal prose). VAD-driven segment boundaries from Aavaaz
//! make whole-utterance commands feel natural in practice.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// User-spoken text; polish + inject as usual.
    Text(String),
    /// Insert a single newline.
    Newline,
    /// Insert a blank line (two newlines).
    Paragraph,
    /// Erase the previous injection.
    ScratchLast,
    /// Select-all in the focused app.
    SelectAll,
}

pub fn parse(raw: &str) -> Command {
    let normalized = normalize(raw);
    match normalized.as_str() {
        "scratch that" | "delete that" | "strike that" => Command::ScratchLast,
        "new line" | "newline" => Command::Newline,
        "new paragraph" | "new para" => Command::Paragraph,
        "select all" => Command::SelectAll,
        _ => Command::Text(raw.trim().to_string()),
    }
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_end_matches(['.', '!', '?', ','])
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_that_variants() {
        for s in [
            "scratch that",
            "Scratch that.",
            "  SCRATCH THAT  ",
            "delete that",
            "Strike that!",
        ] {
            assert_eq!(parse(s), Command::ScratchLast, "input {s:?}");
        }
    }

    #[test]
    fn newline_variants() {
        for s in ["new line", "New line.", "newline"] {
            assert_eq!(parse(s), Command::Newline, "input {s:?}");
        }
    }

    #[test]
    fn paragraph_variants() {
        for s in ["new paragraph", "New paragraph.", "new para"] {
            assert_eq!(parse(s), Command::Paragraph, "input {s:?}");
        }
    }

    #[test]
    fn select_all() {
        assert_eq!(parse("select all"), Command::SelectAll);
        assert_eq!(parse("Select all."), Command::SelectAll);
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(
            parse("hello world"),
            Command::Text("hello world".to_string())
        );
        assert_eq!(
            parse("  remind me to buy milk  "),
            Command::Text("remind me to buy milk".to_string())
        );
    }

    #[test]
    fn similar_but_not_a_command() {
        // Partial matches mid-sentence stay as text.
        assert_eq!(
            parse("please scratch that itch"),
            Command::Text("please scratch that itch".to_string())
        );
        assert_eq!(
            parse("draw a new line on the chart"),
            Command::Text("draw a new line on the chart".to_string())
        );
    }
}
