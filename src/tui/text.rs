use crate::logging::LogHub;

const MAX_VISIBLE_CHARS: usize = 256;

// Larger source fields cannot be safely truncated before removing complete credential values.
pub(super) fn for_terminal(hub: &LogHub, source: &str) -> String {
    if source.len() > 4096 {
        return "[value omitted]".into();
    }
    visible_text(&hub.redact_output(source))
}

fn visible_text(input: &str) -> String {
    let mut output = String::new();
    let mut count = 0;
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
        if count == MAX_VISIBLE_CHARS {
            output.pop();
            output.push('…');
            break;
        }
        let hidden = ch.is_control()
            || matches!(ch,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}');
        output.push(if hidden { ' ' } else { ch });
        count += 1;
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
        assert_eq!(visible_text(&"猫".repeat(257)), "猫".repeat(255) + "…");
    }

    #[test]
    fn redaction_precedes_terminal_display() {
        let hub = LogHub::default();
        let line = for_terminal(&hub, "rtsp://operator:canary-secret@host/live");
        assert!(!line.contains("canary-secret") && !line.contains("operator"));
        assert_eq!(for_terminal(&hub, &"x".repeat(4097)), "[value omitted]");
    }
}
