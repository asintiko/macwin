//! The pairing code as the user types it.

/// The number of digits in a pairing code.
pub const CODE_DIGITS: usize = 6;

/// The ASCII digits of `input`, at most [`CODE_DIGITS`]: pasted text such as
/// `"123 456"` or `"123-456\n"` works too.
pub fn digits(input: &str) -> String {
    input
        .chars()
        .filter(char::is_ascii_digit)
        .take(CODE_DIGITS)
        .collect()
}

/// `input` as it is shown: digits with a dash after the third, `"123-456"`.
pub fn format_code(input: &str) -> String {
    let d = digits(input);
    if d.len() <= 3 {
        d
    } else {
        format!("{}-{}", &d[..3], &d[3..])
    }
}

/// True when `input` holds a whole code.
pub fn is_complete(input: &str) -> bool {
    digits(input).len() == CODE_DIGITS
}

/// The text for `UiCommand::EnterCode`: the code, with the address in front
/// when one is given (`"HOST[:PORT] 123-456"`). `None` while the code is
/// incomplete.
pub fn command_text(code_input: &str, address: &str) -> Option<String> {
    if !is_complete(code_input) {
        return None;
    }
    let code = format_code(code_input);
    let address = address.trim();
    Some(if address.is_empty() {
        code
    } else {
        format!("{address} {code}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_while_typing() {
        assert_eq!(format_code(""), "");
        assert_eq!(format_code("1"), "1");
        assert_eq!(format_code("123"), "123");
        assert_eq!(format_code("1234"), "123-4");
        assert_eq!(format_code("123456"), "123-456");
    }

    #[test]
    fn backspacing_over_the_dash_keeps_the_digits() {
        // "123-4" minus the last digit, then minus the dash.
        assert_eq!(format_code("123-"), "123");
        assert_eq!(format_code("123"), "123");
    }

    #[test]
    fn accepts_pasted_forms_and_drops_the_rest() {
        assert_eq!(format_code("123456"), "123-456");
        assert_eq!(format_code("  123 456 \n"), "123-456");
        assert_eq!(format_code("482-913"), "482-913");
        assert_eq!(format_code("ab1c2d3e4f5g6h7"), "123-456");
        assert_eq!(format_code("١٢٣"), "");
    }

    #[test]
    fn completeness_needs_six_digits() {
        assert!(!is_complete("12345"));
        assert!(is_complete("123-456"));
        assert!(is_complete("123456"));
    }

    #[test]
    fn command_text_adds_the_address() {
        assert_eq!(command_text("482913", ""), Some("482-913".into()));
        assert_eq!(command_text("482-913", "  "), Some("482-913".into()));
        assert_eq!(
            command_text("482913", " 192.168.1.20:47800 "),
            Some("192.168.1.20:47800 482-913".into())
        );
        assert_eq!(command_text("4829", "host"), None);
    }
}
