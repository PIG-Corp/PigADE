//! Literal argument-list parsing for custom CLI profiles; this never invokes a shell.
//!
//! Outside quotes, Unicode whitespace separates arguments and backslash quotes
//! the following character. Single quotes group literal text (backslash remains
//! literal); double-quoted segments use JSON string escaping. Adjacent quoted
//! and unquoted segments form one argument. Shell metacharacters are ordinary
//! text and are never expanded.

const MAX_SOURCE_BYTES: usize = 128 * 1024;
const MAX_ARGUMENTS: usize = 64;
const MAX_DECODED_BYTES: usize = 16 * 1024;

pub(crate) fn parse_arguments(source: &str) -> Result<Vec<String>, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("Список аргументов превышает 128 КиБ".into());
    }

    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut active = false;
    let mut single_quoted = false;
    let mut escaped = false;
    let mut decoded_bytes = 0usize;
    let mut index = 0usize;

    while index < source.len() {
        let ch = source[index..]
            .chars()
            .next()
            .expect("index is a char boundary");
        if escaped {
            if ch == '\0' {
                return Err("Аргументы не могут содержать NUL".into());
            }
            current.push(ch);
            decoded_bytes += ch.len_utf8();
            if decoded_bytes > MAX_DECODED_BYTES {
                return Err("Общий размер аргументов превышает 16 КиБ".into());
            }
            escaped = false;
            active = true;
            index += ch.len_utf8();
            continue;
        }
        if ch == '\0' {
            return Err("Аргументы не могут содержать NUL".into());
        }
        if single_quoted {
            if ch == '\'' {
                single_quoted = false;
            } else {
                current.push(ch);
                decoded_bytes += ch.len_utf8();
            }
            active = true;
        } else {
            match ch {
                '\'' => {
                    single_quoted = true;
                    active = true;
                }
                '"' => {
                    let (value, end) = parse_json_string(source, index)?;
                    if value.contains('\0') {
                        return Err("Аргументы не могут содержать NUL".into());
                    }
                    decoded_bytes = decoded_bytes.saturating_add(value.len());
                    if decoded_bytes > MAX_DECODED_BYTES {
                        return Err("Общий размер аргументов превышает 16 КиБ".into());
                    }
                    current.push_str(&value);
                    active = true;
                    index = end;
                    continue;
                }
                '\\' => {
                    escaped = true;
                    active = true;
                }
                ch if ch.is_whitespace() => {
                    if active {
                        arguments.push(std::mem::take(&mut current));
                        active = false;
                        if arguments.len() > MAX_ARGUMENTS {
                            return Err("Допускается не более 64 аргументов".into());
                        }
                    }
                }
                _ => {
                    current.push(ch);
                    decoded_bytes += ch.len_utf8();
                    active = true;
                }
            }
        }
        if decoded_bytes > MAX_DECODED_BYTES {
            return Err("Общий размер аргументов превышает 16 КиБ".into());
        }
        index += ch.len_utf8();
    }
    if escaped {
        return Err("Последний обратный слеш не экранирует символ".into());
    }
    if single_quoted {
        return Err("Не закрыта одинарная кавычка в списке аргументов".into());
    }
    if active {
        arguments.push(current);
    }
    if arguments.len() > MAX_ARGUMENTS {
        return Err("Допускается не более 64 аргументов".into());
    }
    if decoded_bytes > MAX_DECODED_BYTES {
        return Err("Общий размер аргументов превышает 16 КиБ".into());
    }
    Ok(arguments)
}

fn parse_json_string(source: &str, start: usize) -> Result<(String, usize), String> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    let mut escaped = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if !escaped => {
                let end = index + 1;
                let value = serde_json::from_str::<String>(&source[start..end])
                    .map_err(|_| "Неверная JSON-экранировка в двойных кавычках".to_owned())?;
                return Ok((value, end));
            }
            b'\\' if !escaped => escaped = true,
            _ => escaped = false,
        }
        index += 1;
    }
    Err("Не закрыта двойная кавычка в списке аргументов".into())
}

/// Produce a display/edit representation that round-trips through
/// [`parse_arguments`] without expansion or shell interpretation.
pub(crate) fn format_arguments(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| serde_json::to_string(argument).expect("strings always serialize"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_empty_words_unicode_and_shell_metacharacters_are_literal() {
        let parsed =
            parse_arguments("plain 'two words' \"\" '雪🌱' $(touch x) a;b|c&d*`x`").unwrap();
        assert_eq!(
            parsed,
            [
                "plain",
                "two words",
                "",
                "雪🌱",
                "$(touch",
                "x)",
                "a;b|c&d*`x`"
            ]
        );
    }

    #[test]
    fn formatter_round_trips_quotes_newlines_and_empty_arguments() {
        let input = vec![
            String::new(),
            "it's quoted".into(),
            "line one\nline two".into(),
            "λ 雪".into(),
            "$(echo `x`);|&*".into(),
        ];
        assert_eq!(parse_arguments(&format_arguments(&input)).unwrap(), input);
    }

    #[test]
    fn formatter_round_trips_every_non_nul_ascii_character_and_unicode() {
        let mut value: String = (1u8..=0x7f).map(char::from).collect();
        value.push_str("λ雪🌱\n\t");
        let input = vec![value];
        assert_eq!(parse_arguments(&format_arguments(&input)).unwrap(), input);
    }

    #[test]
    fn formatted_controls_survive_the_real_edit_buffer_crlf_normalization() {
        let mut value: String = (1u8..=0x7f).map(char::from).collect();
        value.push_str("λ雪🌱\r\n");
        let input = vec![value];
        let encoded = format_arguments(&input);
        let editor = crate::text_edit::EditBuffer::new(encoded);
        assert_eq!(parse_arguments(editor.text()).unwrap(), input);
    }

    #[test]
    fn formatter_round_trips_maximum_decoded_arguments_with_control_characters() {
        let arguments = vec!["\u{1}".repeat(256); 64];
        assert_eq!(
            arguments.iter().map(String::len).sum::<usize>(),
            MAX_DECODED_BYTES
        );
        let formatted = format_arguments(&arguments);
        assert!(formatted.len() > 64 * 1024);
        assert_eq!(parse_arguments(&formatted).unwrap(), arguments);
    }

    #[test]
    fn malformed_quotes_and_escape_are_rejected() {
        assert!(parse_arguments("'unfinished").is_err());
        assert!(parse_arguments("\"unfinished").is_err());
        assert!(parse_arguments("trailing\\").is_err());
        assert!(parse_arguments(r#""bad\q""#).is_err());
        assert!(parse_arguments(r#""\u0000""#).is_err());
    }

    #[test]
    fn source_argument_count_and_decoded_size_are_bounded() {
        assert!(parse_arguments(&"x ".repeat(65_537)).is_err());
        assert!(parse_arguments(&vec!["x"; 65].join(" ")).is_err());
        assert!(parse_arguments(&format!("'{}'", "x".repeat(16 * 1024 + 1))).is_err());
    }
}
