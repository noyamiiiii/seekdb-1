//! Tokenizer for the SQL subset.
//!
//! Statements arrive from [`crate::proto`] with comments already removed, so the
//! lexer only deals with whitespace, words, numbers, string literals and the few
//! punctuation characters the grammar uses.

use crate::error::{DbError, DbResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Bare word: keyword or identifier; the parser decides which.
    Word,
    /// Integer or floating point literal, kept as written.
    Number,
    /// Single-quoted string with escapes already resolved.
    Str,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Semi,
    Star,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: Kind,
    /// Raw word/number text, or the decoded string for [`Kind::Str`].
    pub text: String,
    /// Byte offset of the token in the statement, for error messages.
    pub pos: usize,
}

impl Token {
    pub fn is_word(&self, keyword: &str) -> bool {
        self.kind == Kind::Word && self.text.eq_ignore_ascii_case(keyword)
    }
}

pub fn tokenize(input: &str) -> DbResult<Vec<Token>> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let pos = i;
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => push(&mut tokens, Kind::LParen, pos, &mut i),
            b')' => push(&mut tokens, Kind::RParen, pos, &mut i),
            b'[' => push(&mut tokens, Kind::LBracket, pos, &mut i),
            b']' => push(&mut tokens, Kind::RBracket, pos, &mut i),
            b',' => push(&mut tokens, Kind::Comma, pos, &mut i),
            b';' => push(&mut tokens, Kind::Semi, pos, &mut i),
            b'*' => push(&mut tokens, Kind::Star, pos, &mut i),
            b'=' => push(&mut tokens, Kind::Eq, pos, &mut i),
            b'<' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token {
                        kind: Kind::Le,
                        text: "<=".into(),
                        pos,
                    });
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'>') {
                    return Err(syntax_at(pos, "unexpected '<>' operator"));
                } else {
                    push(&mut tokens, Kind::Lt, pos, &mut i);
                }
            }
            b'>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token {
                        kind: Kind::Ge,
                        text: ">=".into(),
                        pos,
                    });
                    i += 2;
                } else {
                    push(&mut tokens, Kind::Gt, pos, &mut i);
                }
            }
            b'\'' => {
                let (text, next) = read_string(input, i)?;
                tokens.push(Token {
                    kind: Kind::Str,
                    text,
                    pos,
                });
                i = next;
            }
            b'0'..=b'9' => {
                let (text, next) = read_number(input, i)?;
                tokens.push(Token {
                    kind: Kind::Number,
                    text,
                    pos,
                });
                i = next;
            }
            b'-' if bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) => {
                let (text, next) = read_number(input, i)?;
                tokens.push(Token {
                    kind: Kind::Number,
                    text,
                    pos,
                });
                i = next;
            }
            b'_' | b'A'..=b'Z' | b'a'..=b'z' => {
                let start = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                tokens.push(Token {
                    kind: Kind::Word,
                    text: input[start..i].to_string(),
                    pos,
                });
            }
            _ => {
                let ch = input[i..].chars().next().unwrap_or('?');
                return Err(syntax_at(pos, format!("unexpected character '{ch}'")));
            }
        }
    }
    tokens.push(Token {
        kind: Kind::End,
        text: String::new(),
        pos: bytes.len(),
    });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, kind: Kind, pos: usize, i: &mut usize) {
    tokens.push(Token {
        kind,
        text: String::new(),
        pos,
    });
    *i += 1;
}

fn read_number(input: &str, start: usize) -> DbResult<(String, usize)> {
    let bytes = input.as_bytes();
    let mut i = start;
    if bytes[i] == b'-' {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    // Fraction / exponent: only used by vector elements, but accepting them
    // uniformly keeps the lexer free of context.
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(|c| c.is_ascii_digit()) {
            i = j;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    Ok((input[start..i].to_string(), i))
}

fn read_string(input: &str, start: usize) -> DbResult<(String, usize)> {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut i = start + 1;
    loop {
        let Some(&b) = bytes.get(i) else {
            return Err(syntax_at(start, "unterminated string literal"));
        };
        match b {
            b'\'' => {
                // `''` is an escaped quote and continues the literal.
                if bytes.get(i + 1) == Some(&b'\'') {
                    out.push('\'');
                    i += 2;
                } else {
                    return Ok((out, i + 1));
                }
            }
            b'\\' => {
                let Some(&esc) = bytes.get(i + 1) else {
                    return Err(syntax_at(start, "unterminated string literal"));
                };
                out.push(match esc {
                    b'\'' => '\'',
                    b'"' => '"',
                    b'\\' => '\\',
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    other => {
                        return Err(syntax_at(
                            i,
                            format!("unsupported escape '\\{}'", other as char),
                        ))
                    }
                });
                i += 2;
            }
            _ => {
                // Copy the whole UTF-8 scalar, not the byte.
                let ch = input[i..].chars().next().unwrap_or('?');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
}

fn syntax_at(pos: usize, message: impl Into<String>) -> DbError {
    DbError::syntax(format!("{} (at character {})", message.into(), pos + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<Kind> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn words_numbers_and_punctuation() {
        assert_eq!(
            kinds("SELECT id FROM t WHERE id >= 10 LIMIT 5"),
            vec![
                Kind::Word,
                Kind::Word,
                Kind::Word,
                Kind::Word,
                Kind::Word,
                Kind::Word,
                Kind::Ge,
                Kind::Number,
                Kind::Word,
                Kind::Number,
                Kind::End
            ]
        );
    }

    #[test]
    fn keywords_are_matched_case_insensitively() {
        let t = tokenize("select Select SELECT").unwrap();
        assert!(t[0].is_word("SELECT"));
        assert!(t[1].is_word("select"));
    }

    #[test]
    fn strings_keep_quotes_and_escapes_inside() {
        let t = tokenize("'a''b' 'c\\'d' 'x;y'").unwrap();
        assert_eq!(t[0].text, "a'b");
        assert_eq!(t[1].text, "c'd");
        assert_eq!(t[2].text, "x;y");
    }

    #[test]
    fn numbers_accept_sign_fraction_and_exponent() {
        let t = tokenize("-12 3.5 1e-3").unwrap();
        assert_eq!(t[0].text, "-12");
        assert_eq!(t[1].text, "3.5");
        assert_eq!(t[2].text, "1e-3");
    }

    #[test]
    fn brackets_are_tokenized() {
        assert_eq!(
            kinds("[1,2.5,-3]"),
            vec![
                Kind::LBracket,
                Kind::Number,
                Kind::Comma,
                Kind::Number,
                Kind::Comma,
                Kind::Number,
                Kind::RBracket,
                Kind::End
            ]
        );
    }

    #[test]
    fn unterminated_string_is_a_syntax_error() {
        let err = tokenize("'abc").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Syntax);
    }

    #[test]
    fn unexpected_character_is_a_syntax_error() {
        assert!(tokenize("SELECT @x").is_err());
    }
}
