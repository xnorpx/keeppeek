use crate::logging::LogHub;

const MAX_VISIBLE_CHARS: usize = 256;

// Larger source fields cannot be safely truncated before removing complete credential values.
pub(super) fn for_terminal(hub: &LogHub, source: &str) -> String {
    if source.len() > 4096 {
        return "[value omitted]".into();
    }
    let redacted = hub.redact_output(source);
    let normalized = visible_text(&redacted);
    // Removing a control sequence may join previously separated portions of a private value.
    let mut safe = if normalized == redacted {
        redacted
    } else {
        hub.redact_output(&normalized)
    };
    if let Some((end, _)) = safe.char_indices().nth(MAX_VISIBLE_CHARS) {
        safe.truncate(end);
        safe.pop();
        safe.push('\u{2026}');
    }
    safe
}

fn visible_text(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => {
                match chars.next() {
                    Some('[') => discard_csi(&mut chars),
                    Some(']' | 'P' | 'X' | '^' | '_') => discard_string(&mut chars),
                    Some(' '..='/') => {
                        chars.find(|ch| ('0'..='~').contains(ch));
                    }
                    _ => {}
                }
                continue;
            }
            '\u{9b}' => {
                discard_csi(&mut chars);
                continue;
            }
            '\u{90}' | '\u{98}' | '\u{9d}'..='\u{9f}' => {
                discard_string(&mut chars);
                continue;
            }
            _ => {}
        }
        let hidden = ch.is_control()
            || matches!(ch,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}');
        output.push(if hidden { ' ' } else { ch });
    }
    output
}

fn discard_csi(chars: &mut std::str::Chars<'_>) {
    chars.find(|ch| ('@'..='~').contains(ch));
}

fn discard_string(chars: &mut std::str::Chars<'_>) {
    let mut escape = false;
    for ch in chars {
        if ch == '\x07' || ch == '\u{9c}' || (escape && ch == '\\') {
            break;
        }
        escape = ch == '\x1b';
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_controls_are_removed_and_unicode_is_bounded() {
        assert_eq!(
            visible_text("門\x1b[2J口\x1b]0;hidden\x07\n\u{202e}"),
            "門口  "
        );
        assert_eq!(
            visible_text("a\u{9d}title\u{9c}b\x1bPdiscard\x1b\\c"),
            "abc"
        );
        assert_eq!(
            for_terminal(&LogHub::default(), &"猫".repeat(257)),
            "猫".repeat(255) + "…"
        );
    }

    #[test]
    fn normalized_private_values_are_redacted_before_clipping() {
        let hub = LogHub::default();
        hub.set_sensitive_values(["sample-private-value".to_owned()]);
        let formatted = "sample-\x1b[31mprivate-value";
        assert_eq!(
            for_terminal(&hub, formatted),
            for_terminal(&hub, "sample-private-value")
        );
        let prefix = "x".repeat(MAX_VISIBLE_CHARS - 10);
        let line = for_terminal(&hub, &format!("{prefix}{formatted}"));
        assert_eq!(
            line,
            for_terminal(&hub, &format!("{prefix}sample-private-value"))
        );
        assert!(!line.contains("sample-"));
    }

    #[test]
    fn redaction_precedes_terminal_display() {
        let hub = LogHub::default();
        let line = for_terminal(&hub, "rtsp://operator:canary-secret@host/live");
        assert!(!line.contains("canary-secret") && !line.contains("operator"));
        assert_eq!(for_terminal(&hub, &"x".repeat(4097)), "[value omitted]");
    }
}
