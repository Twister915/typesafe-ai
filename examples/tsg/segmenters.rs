//! Deliberately shallow boundaries: code uses complete lines, never a language parser.
//! Strings, comments, nested blocks, and minified code may be split at the byte cap.
use unicode_segmentation::UnicodeSegmentation;

/// Group complete JS/TS lines using shallow bracket depth and trailing continuations.
/// Delimiters inside strings/comments count too; this is intentionally not lexical analysis.
pub(crate) fn javascript(text: &str, eof: bool) -> Option<usize> {
    let (mut end, mut depth) = (0, 0usize);
    for line in text.split_inclusive('\n') {
        end += line.len();
        if !line.ends_with('\n') && !eof {
            break;
        }
        let line = line.trim();
        for character in line.chars() {
            depth = match character {
                '(' | '[' => depth + 1,
                ')' | ']' => depth.saturating_sub(1),
                _ => depth,
            };
        }
        let continued = line.ends_with(|c: char| "{([.,=:+-*/%&|?!<>\\".contains(c));
        if depth == 0 && !continued {
            return Some(end);
        }
    }
    eof.then_some(text.len()).filter(|end| *end > 0)
}

/// End a Rust unit after a blank line, item terminator, or closing-brace line.
pub(crate) fn rust(text: &str, eof: bool) -> Option<usize> {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        end += line.len();
        if !line.ends_with('\n') && !eof {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line == "}" || line.ends_with("};") || line.ends_with(';') {
            return Some(end);
        }
    }
    eof.then_some(text.len()).filter(|end| *end > 0)
}

/// End a CSS unit after a closing-rule line or a standalone semicolon at-rule.
pub(crate) fn css(text: &str, eof: bool) -> Option<usize> {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        end += line.len();
        if !line.ends_with('\n') && !eof {
            break;
        }
        let line = line.trim();
        if line.ends_with('}') || (line.starts_with('@') && line.ends_with(';')) {
            return Some(end);
        }
    }
    eof.then_some(text.len()).filter(|end| *end > 0)
}

/// Use Unicode UAX #29 sentences, retaining the final unfinished sentence until EOF.
pub(crate) fn prose(text: &str, eof: bool) -> Option<usize> {
    let first = text.split_sentence_bounds().next()?;
    (eof || first.len() < text.len()).then_some(first.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmenters_are_at_most_thirty_physical_lines() {
        let source = include_str!("segmenters.rs");
        for name in ["javascript", "rust", "css", "prose"] {
            let start = source.find(&format!("pub(crate) fn {name}(")).unwrap();
            let function = &source[start..];
            let end = function.find("\n}").unwrap();
            assert!(function[..end + 2].lines().count() <= 30, "{name}");
        }
    }

    #[test]
    fn complete_code_lines_are_simple_boundaries() {
        assert_eq!(javascript("const x = 1;\r\nnext", false), Some(14));
        assert_eq!(rust("fn f() {\n  work();\n}\n", false), Some(19));
        assert_eq!(css("a {\n color: red;\n}\nnext", false), Some(19));
        assert_eq!(javascript("const x = 1;", false), None);
        assert_eq!(rust("unfinished", true), Some(10));
    }

    #[test]
    fn semicolonless_javascript_keeps_calls_and_trailing_continuations_together() {
        for first in [
            "record()\n",
            "record(\n  first,\n  second\n)\n",
            "const sum = first +\n  second\n",
            "const items = [\n  first,\n  second\n]\n",
            "source.\n  map(transform)\n",
        ] {
            let source = format!("{first}record()\n");
            assert_eq!(javascript(&source, false), Some(first.len()), "{source}");
        }
        assert_eq!(javascript("record(\n  value\n", false), None);
        assert_eq!(javascript("const sum = first +\n", false), None);
    }

    #[test]
    fn prose_uses_unicode_decimal_and_crlf_sentence_boundaries() {
        let text = "Value 3.14 is\r\nuseful. Next。 最後！";
        let boundary = prose(text, false).unwrap();
        assert_eq!(&text[..boundary], "Value 3.14 is\r\n");
        assert_eq!(prose("Value 3.14 is useful. ", false), None);
        assert_eq!(prose("你好。 下一个。", false), Some("你好。 ".len()));
    }
}
