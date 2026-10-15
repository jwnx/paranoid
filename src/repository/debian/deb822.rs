//! deb822 stanzas, the `Key: Value` format of `Packages` and `Release`.

/// Parse an RFC 822-style stanza into key-value pairs.
///
/// Handles:
/// - `Key: Value` format
/// - `Key:` with empty value
/// - Continuation lines (starting with space or tab)
pub(super) fn parse_stanza(stanza: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut current_key = String::new();
    let mut current_value = String::new();

    for line in stanza.lines() {
        if line.is_empty() {
            continue;
        }

        // Check for continuation line (starts with space or tab)
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation of previous field
            if !current_key.is_empty() {
                current_value.push('\n');
                current_value.push_str(line.trim_start());
            }
            continue;
        }

        // Save previous field if any
        if !current_key.is_empty() {
            fields.push((current_key, current_value));
            current_key = String::new();
            current_value = String::new();
        }

        // Parse "Key: Value" or "Key:"
        if let Some(colon_pos) = line.find(':') {
            current_key = line[..colon_pos].to_string();
            current_value = line[colon_pos + 1..].trim_start().to_string();
        }
    }

    // Don't forget the last field
    if !current_key.is_empty() {
        fields.push((current_key, current_value));
    }

    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stanza_simple() {
        let stanza = "Package: foo\nVersion: 1.0\nArchitecture: amd64";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], ("Package".to_string(), "foo".to_string()));
        assert_eq!(fields[1], ("Version".to_string(), "1.0".to_string()));
        assert_eq!(fields[2], ("Architecture".to_string(), "amd64".to_string()));
    }

    #[test]
    fn test_parse_stanza_continuation_lines() {
        let stanza = "Package: foo\nDescription: Short desc\n Long description\n continues here";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 2);
        assert_eq!(
            fields[1],
            (
                "Description".to_string(),
                "Short desc\nLong description\ncontinues here".to_string()
            )
        );
    }

    #[test]
    fn test_parse_stanza_empty_value() {
        let stanza = "Package: foo\nDepends:\nVersion: 1.0";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], ("Depends".to_string(), "".to_string()));
    }
}
