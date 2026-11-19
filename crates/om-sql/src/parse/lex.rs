//! The FQL lexer (execute-sql.md 3.1): the statement's code points to
//! tokens, as FileMaker's ANTLR lexer `FQLSQLLexer` splits them.
//!
//! ANTLR lexers take the longest match at each position, the first rule on
//! a tie (keywords before `IDENTIFIER`), and fall back to the longest token
//! that did match: an unterminated `'abc` is the token `'` and then `abc`,
//! `5.` is `5` and `.`, a `//` that no LF ends is two `/`. A character that
//! starts no token is a lexer error; ANTLR reports it and skips that one
//! character, so the error is remembered with the index of the token being
//! produced when it happened (the parser decides which error came first,
//! [`super::interp`]).

/// A token type: [`EOF`], a token class, a punctuation token or a keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenType(pub u16);

pub const EOF: TokenType = TokenType(0);
pub const IDENTIFIER: TokenType = TokenType(1);
/// `"ROWID"` in any case: the keyword in quotes, not a name.
pub const QUOTED_ROWID: TokenType = TokenType(2);
pub const STRING_CONST: TokenType = TokenType(3);
pub const UNSIGNED_CONST: TokenType = TokenType(4);
pub const FLOAT_CONST: TokenType = TokenType(5);
pub const BIT_CONST: TokenType = TokenType(6);
pub const HEX_CONST: TokenType = TokenType(7);
/// `<>` or `!=`.
pub const NEQ: TokenType = TokenType(8);
/// `^` or `**`.
pub const RAISE: TokenType = TokenType(9);

/// Punctuation by its text, from type 10.
pub const PUNCTUATION: &[&str] = &[
    "(", ")", "{", "}", "[", "]", ".", "+", "-", "/", "*", ",", "'", "\"", "=", "<", "<=", ">",
    ">=", "?", "||", ":",
];
const PUNCT_BASE: u16 = 10;
/// Keywords from type 40, in the order of [`KEYWORDS`].
const KEYWORD_BASE: u16 = 40;

/// The 265 reserved words (case-insensitive ASCII letters; `END-EXEC` has a
/// hyphen).
pub const KEYWORDS: &[&str] = &[
    "ABSOLUTE",
    "ACTION",
    "ADD",
    "ALL",
    "ALLOCATE",
    "ALTER",
    "AND",
    "ANY",
    "ARE",
    "AS",
    "ASC",
    "ASSERTION",
    "AT",
    "AUTHORIZATION",
    "AVG",
    "BEGIN",
    "BETWEEN",
    "BINARY",
    "BIT",
    "BIT_LENGTH",
    "BLOB",
    "BOOLEAN",
    "BOTH",
    "BY",
    "CASCADE",
    "CASCADED",
    "CASE",
    "CAST",
    "CATALOG",
    "CHAR",
    "CHARACTER",
    "CHAR_LENGTH",
    "CHARACTER_LENGTH",
    "CHR",
    "CHECK",
    "CLOSE",
    "COALESCE",
    "COLLATE",
    "COLLATION",
    "COLUMN",
    "COMMIT",
    "CONNECT",
    "CONNECTION",
    "CONSTRAINT",
    "CONSTRAINTS",
    "CONTINUE",
    "CONVERT",
    "CORRESPONDING",
    "COUNT",
    "CREATE",
    "CROSS",
    "CURDATE",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "CURSOR",
    "CURTIME",
    "CURTIMESTAMP",
    "DATE",
    "DATEVAL",
    "DAY",
    "DAYNAME",
    "DAYOFWEEK",
    "DEALLOCATE",
    "DEC",
    "DECIMAL",
    "DECLARE",
    "DEFAULT",
    "DEFERRABLE",
    "DEFERRED",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DESCRIPTOR",
    "DIAGNOSTICS",
    "DISCONNECT",
    "DISTINCT",
    "DOMAIN",
    "DOUBLE",
    "DROP",
    "ELSE",
    "END",
    "END-EXEC",
    "ESCAPE",
    "EVERY",
    "EXCEPT",
    "EXCEPTION",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "EXTERNAL",
    "EXTRACT",
    "FALSE",
    "FETCH",
    "FIRST",
    "FLOAT",
    "FOR",
    "FOREIGN",
    "FOUND",
    "FROM",
    "FULL",
    "GET",
    "GETAS",
    "GLOBAL",
    "GO",
    "GOTO",
    "GRANT",
    "GROUP",
    "HAVING",
    "HOUR",
    "IDENTITY",
    "IMMEDIATE",
    "IN",
    "INDEX",
    "INDICATOR",
    "INITIALLY",
    "INNER",
    "INPUT",
    "INSENSITIVE",
    "INSERT",
    "INT",
    "INTEGER",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "ISOLATION",
    "JOIN",
    "KEY",
    "LANGUAGE",
    "LAST",
    "LEADING",
    "LEFT",
    "LENGTH",
    "FEWER_FOLDERS",
    "LEVEL",
    "LIKE",
    "LOCAL",
    "LONGVARBINARY",
    "LOWER",
    "LTRIM",
    "MATCH",
    "MAX",
    "MIN",
    "MINUTE",
    "MODULE",
    "MONTH",
    "MONTHNAME",
    "NAMES",
    "NATURAL",
    "NCHAR",
    "NEXT",
    "NO",
    "NOT",
    "NULL",
    "NULLIF",
    "NUMERIC",
    "NUMVAL",
    "OCTET_LENGTH",
    "OF",
    "OFFSET",
    "ON",
    "ONLY",
    "OPEN",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "OUTPUT",
    "OVERLAPS",
    "PAD",
    "PART",
    "PARTIAL",
    "PERCENT",
    "POSITION",
    "PRECISION",
    "PREPARE",
    "PRESERVE",
    "PRIMARY",
    "PRIOR",
    "PRIVILEGES",
    "PROCEDURE",
    "PUBLIC",
    "PUTAS",
    "RANDOM",
    "READ",
    "REAL",
    "REFERENCES",
    "RELATIVE",
    "RENAME",
    "RESTRICT",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROUND",
    "ROW",
    "ROWS",
    "ROWID",
    "RTRIM",
    "SCHEMA",
    "SCROLL",
    "SECOND",
    "SECTION",
    "SECURE",
    "SELECT",
    "SESSION",
    "SESSION_USER",
    "SET",
    "SIZE",
    "SMALLINT",
    "SOME",
    "SPACE",
    "SQL",
    "SQLCODE",
    "SQLERROR",
    "SQLSTATE",
    "STRVAL",
    "SUBSTR",
    "SUBSTRING",
    "SUM",
    "SYSTEM_USER",
    "TABLE",
    "TEMPORARY",
    "THEN",
    "TIES",
    "TIME",
    "TIMESTAMP",
    "TIMESTAMPVAL",
    "TIMEVAL",
    "TIMEZONE_HOUR",
    "TIMEZONE_MINUTE",
    "TO",
    "TODAY",
    "TRAILING",
    "TRANSACTION",
    "TRANSLATE",
    "TRANSLATION",
    "TRIM",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UNKNOWN",
    "UPDATE",
    "UPPER",
    "USAGE",
    "USER",
    "USERNAME",
    "USING",
    "VALUE",
    "VALUES",
    "VARBINARY",
    "VARCHAR",
    "VARYING",
    "VIEW",
    "WHEN",
    "WHENEVER",
    "WHERE",
    "WITH",
    "WORK",
    "WRITE",
    "YEAR",
    "ZONE",
];

/// The number of token types.
pub fn token_type_count() -> usize {
    usize::from(KEYWORD_BASE) + KEYWORDS.len()
}

/// The keyword spelled `word` (any case).
pub fn keyword(word: &str) -> Option<TokenType> {
    KEYWORDS
        .iter()
        .position(|k| k.eq_ignore_ascii_case(word))
        .map(|i| TokenType(KEYWORD_BASE + i as u16))
}

/// The punctuation token spelled `text`.
pub fn punctuation(text: &str) -> Option<TokenType> {
    PUNCTUATION
        .iter()
        .position(|p| *p == text)
        .map(|i| TokenType(PUNCT_BASE + i as u16))
}

/// A keyword's type by its upper-case spelling; panics for a word that is
/// not one (grammar and AST code use it with literals).
pub fn kw(word: &str) -> TokenType {
    keyword(word).unwrap_or_else(|| panic!("not an FQL keyword: {word}"))
}

/// A punctuation token's type; panics for anything else.
pub fn punct(text: &str) -> TokenType {
    punctuation(text).unwrap_or_else(|| panic!("not FQL punctuation: {text}"))
}

impl TokenType {
    /// A display name: the keyword or punctuation text, or the class name.
    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "EOF",
            1 => "IDENTIFIER",
            2 => "QUOTED_ROWID",
            3 => "STRING_CONST",
            4 => "UNSIGNED_CONST",
            5 => "FLOAT_CONST",
            6 => "BIT_CONST",
            7 => "HEX_CONST",
            8 => "NEQ",
            9 => "RAISE",
            n if n < KEYWORD_BASE => PUNCTUATION
                .get(usize::from(n - PUNCT_BASE))
                .copied()
                .unwrap_or("?"),
            n => KEYWORDS
                .get(usize::from(n - KEYWORD_BASE))
                .copied()
                .unwrap_or("?"),
        }
    }

    pub fn is_keyword(self) -> bool {
        self.0 >= KEYWORD_BASE
    }
}

/// A position in the statement: line from 1 (advanced by LF only) and
/// column in code points since the last LF, from 0 (ANTLR's
/// `charPositionInLine`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: u32,
    pub column: u32,
}

/// A token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub ty: TokenType,
    /// The source text of the token.
    pub text: String,
    /// The value: identifiers without quotes and with `""` as `"`, strings
    /// without quotes and with `''` as `'`; otherwise the text.
    pub value: String,
    pub pos: Pos,
}

/// A character that starts no token, at `pos`, reported while the lexer
/// produced the token with index `token_index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexError {
    pub pos: Pos,
    pub token_index: usize,
}

/// The tokens of a statement (ending with [`EOF`]) and its lexer errors in
/// text order.
#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
}

fn is_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Splits `sql` into tokens.
pub fn lex(sql: &str) -> Lexed {
    let chars: Vec<char> = sql.chars().collect();
    let mut lexer = Lexer {
        chars: &chars,
        i: 0,
        pos: Pos { line: 1, column: 0 },
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        errors: lexer.errors,
    }
}

struct Lexer<'a> {
    chars: &'a [char],
    i: usize,
    pos: Pos,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
}

impl Lexer<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    /// Moves past `n` characters, keeping the line and column.
    fn advance(&mut self, n: usize) {
        for _ in 0..n {
            if self.chars[self.i] == '\n' {
                self.pos.line += 1;
                self.pos.column = 0;
            } else {
                self.pos.column += 1;
            }
            self.i += 1;
        }
    }

    fn emit(&mut self, ty: TokenType, len: usize, value: Option<String>) {
        let text: String = self.chars[self.i..self.i + len].iter().collect();
        let value = value.unwrap_or_else(|| text.clone());
        self.tokens.push(Token {
            ty,
            text,
            value,
            pos: self.pos,
        });
        self.advance(len);
    }

    fn run(&mut self) {
        while self.i < self.chars.len() {
            let c = self.chars[self.i];
            match c {
                '\t' | '\n' | '\r' | ' ' => self.advance(1),
                _ => {
                    if let Some(len) = self.comment() {
                        self.advance(len);
                    } else if !self.token() {
                        self.errors.push(LexError {
                            pos: self.pos,
                            token_index: self.tokens.len(),
                        });
                        self.advance(1);
                    }
                }
            }
        }
        self.tokens.push(Token {
            ty: EOF,
            text: String::new(),
            value: String::new(),
            pos: self.pos,
        });
    }

    /// The length of a comment starting here: `/* ... */` (shortest) or `//`
    /// up to and including a LF.
    fn comment(&self) -> Option<usize> {
        let i = self.i;
        if self.at(i) != Some('/') {
            return None;
        }
        match self.at(i + 1)? {
            '*' => {
                let mut j = i + 2;
                while j + 1 < self.chars.len() {
                    if self.chars[j] == '*' && self.chars[j + 1] == '/' {
                        return Some(j + 2 - i);
                    }
                    j += 1;
                }
                None
            }
            '/' => {
                let mut j = i + 2;
                while matches!(self.at(j), Some(c) if c != '\r' && c != '\n') {
                    j += 1;
                }
                if self.at(j) == Some('\r') {
                    j += 1;
                }
                (self.at(j) == Some('\n')).then_some(j + 1 - i)
            }
            _ => None,
        }
    }

    /// Emits the token starting here; `false` when none starts here.
    fn token(&mut self) -> bool {
        let i = self.i;
        let c = self.chars[i];
        if is_letter(c) {
            self.word();
            return true;
        }
        if c.is_ascii_digit() || (c == '.' && self.at(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            self.number();
            return true;
        }
        match c {
            '"' => {
                if let Some((len, value)) = self.quoted('"') {
                    let ty = if value.eq_ignore_ascii_case("ROWID") && len == 7 {
                        QUOTED_ROWID
                    } else {
                        IDENTIFIER
                    };
                    self.emit(ty, len, Some(value));
                } else {
                    self.emit(punct("\""), 1, None);
                }
                return true;
            }
            '\'' => {
                if let Some((len, value)) = self.quoted('\'') {
                    self.emit(STRING_CONST, len, Some(value));
                } else {
                    self.emit(punct("'"), 1, None);
                }
                return true;
            }
            _ => {}
        }
        let next = self.at(i + 1);
        let (ty, len) = match (c, next) {
            ('<', Some('>')) | ('!', Some('=')) => (NEQ, 2),
            ('<', Some('=')) => (punct("<="), 2),
            ('>', Some('=')) => (punct(">="), 2),
            ('|', Some('|')) => (punct("||"), 2),
            ('*', Some('*')) => (RAISE, 2),
            ('^', _) => (RAISE, 1),
            _ => match punctuation(&c.to_string()) {
                Some(ty) if c != '\'' && c != '"' => (ty, 1),
                _ => return false,
            },
        };
        self.emit(ty, len, None);
        true
    }

    /// A quoted name or string from here: its length and value, `None`
    /// when no closing quote ends it.
    fn quoted(&self, quote: char) -> Option<(usize, String)> {
        let mut j = self.i + 1;
        let mut value = String::new();
        loop {
            let c = self.at(j)?;
            if c == quote {
                if self.at(j + 1) == Some(quote) {
                    value.push(quote);
                    j += 2;
                    continue;
                }
                return Some((j + 1 - self.i, value));
            }
            value.push(c);
            j += 1;
        }
    }

    fn word(&mut self) {
        let i = self.i;
        let mut j = i + 1;
        while self.at(j).is_some_and(is_word_char) {
            j += 1;
        }
        let word_len = j - i;
        // BIT_CONST [Bb]'[01]+' and HEX_CONST [Xx]'[0-9A-Fa-f]+' are longer
        // than the one-letter name before their quote.
        let c = self.chars[i];
        if word_len == 1 && self.at(i + 1) == Some('\'') {
            let digit: fn(char) -> bool = match c {
                'B' | 'b' => |d| d == '0' || d == '1',
                'X' | 'x' => |d| d.is_ascii_hexdigit(),
                _ => |_| false,
            };
            let mut k = i + 2;
            while self.at(k).is_some_and(digit) {
                k += 1;
            }
            if k > i + 2 && self.at(k) == Some('\'') {
                let ty = if matches!(c, 'B' | 'b') {
                    BIT_CONST
                } else {
                    HEX_CONST
                };
                self.emit(ty, k + 1 - i, None);
                return;
            }
        }
        let word: String = self.chars[i..j].iter().collect();
        // END-EXEC.
        if word.eq_ignore_ascii_case("END") {
            let rest: String = self.chars[j..(j + 5).min(self.chars.len())]
                .iter()
                .collect();
            if rest.eq_ignore_ascii_case("-EXEC") {
                self.emit(kw("END-EXEC"), word_len + 5, None);
                return;
            }
        }
        match keyword(&word) {
            Some(ty) => self.emit(ty, word_len, None),
            None => self.emit(IDENTIFIER, word_len, None),
        }
    }

    fn number(&mut self) {
        let i = self.i;
        let digits = |from: usize| {
            let mut k = from;
            while self.at(k).is_some_and(|d| d.is_ascii_digit()) {
                k += 1;
            }
            k
        };
        // An exponent [eE][+-]?[0-9]+ at `from`: where it ends.
        let exponent = |from: usize| -> Option<usize> {
            if !matches!(self.at(from), Some('e' | 'E')) {
                return None;
            }
            let mut k = from + 1;
            if matches!(self.at(k), Some('+' | '-')) {
                k += 1;
            }
            let end = digits(k);
            (end > k).then_some(end)
        };
        let int_end = digits(i);
        let (ty, end) = if self.at(int_end) == Some('.')
            && self.at(int_end + 1).is_some_and(|d| d.is_ascii_digit())
        {
            let frac_end = digits(int_end + 1);
            (FLOAT_CONST, exponent(frac_end).unwrap_or(frac_end))
        } else if let Some(end) = exponent(int_end).filter(|_| int_end > i) {
            (FLOAT_CONST, end)
        } else {
            (UNSIGNED_CONST, int_end)
        };
        self.emit(ty, end - i, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(sql: &str) -> Vec<&'static str> {
        lex(sql).tokens.iter().map(|t| t.ty.name()).collect()
    }

    #[test]
    fn keywords_names_and_quotes() {
        assert_eq!(
            types("SeLeCt x FROM \"Related Tasks\""),
            ["SELECT", "IDENTIFIER", "FROM", "IDENTIFIER", "EOF"]
        );
        let lexed = lex("\"a \"\"b\"\"\" 'it''s' \"rowid\" \"ROWID\"\"\"");
        let values: Vec<&str> = lexed.tokens.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(values, ["a \"b\"", "it's", "rowid", "ROWID\"", ""]);
        assert_eq!(lexed.tokens[2].ty, QUOTED_ROWID);
        assert_eq!(lexed.tokens[3].ty, IDENTIFIER);
        assert_eq!(types("END-EXEC end"), ["END-EXEC", "END", "EOF"]);
        assert_eq!(
            types("Timestamp ROWMODID"),
            ["TIMESTAMP", "IDENTIFIER", "EOF"]
        );
    }

    #[test]
    fn numbers_and_binary_literals() {
        let lexed = lex("5. .5 1e3 1.5E-2 12 1e x'41' B'0101' B'012'");
        let tokens: Vec<(&str, &str)> = lexed
            .tokens
            .iter()
            .map(|t| (t.ty.name(), t.text.as_str()))
            .collect();
        assert_eq!(
            tokens,
            [
                ("UNSIGNED_CONST", "5"),
                (".", "."),
                ("FLOAT_CONST", ".5"),
                ("FLOAT_CONST", "1e3"),
                ("FLOAT_CONST", "1.5E-2"),
                ("UNSIGNED_CONST", "12"),
                ("UNSIGNED_CONST", "1"),
                ("IDENTIFIER", "e"),
                ("HEX_CONST", "x'41'"),
                ("BIT_CONST", "B'0101'"),
                ("IDENTIFIER", "B"),
                ("STRING_CONST", "'012'"),
                ("EOF", ""),
            ]
        );
    }

    #[test]
    fn comments_end_at_a_line_feed() {
        assert_eq!(
            types("1 /* c */ 2"),
            ["UNSIGNED_CONST", "UNSIGNED_CONST", "EOF"]
        );
        assert_eq!(
            types("1 /* c"),
            ["UNSIGNED_CONST", "/", "*", "IDENTIFIER", "EOF"]
        );
        assert_eq!(
            types("1 // c\n2"),
            ["UNSIGNED_CONST", "UNSIGNED_CONST", "EOF"]
        );
        assert_eq!(
            types("1 // c\r\n2"),
            ["UNSIGNED_CONST", "UNSIGNED_CONST", "EOF"]
        );
        assert_eq!(
            types("1 // c\r"),
            ["UNSIGNED_CONST", "/", "/", "IDENTIFIER", "EOF"]
        );
        assert_eq!(
            types("3 --1"),
            ["UNSIGNED_CONST", "-", "-", "UNSIGNED_CONST", "EOF"]
        );
    }

    #[test]
    fn operators_and_unterminated_quotes() {
        assert_eq!(
            types("<> != <= >= || ** ^ 'abc"),
            [
                "NEQ",
                "NEQ",
                "<=",
                ">=",
                "||",
                "RAISE",
                "RAISE",
                "'",
                "IDENTIFIER",
                "EOF"
            ]
        );
    }

    #[test]
    fn positions_count_code_points_and_line_feeds() {
        let lexed = lex("'😀😀' x\r\ny\rz");
        let pos: Vec<(u32, u32)> = lexed
            .tokens
            .iter()
            .map(|t| (t.pos.line, t.pos.column))
            .collect();
        assert_eq!(pos, [(1, 0), (1, 5), (2, 0), (2, 2), (2, 3)]);
    }

    #[test]
    fn characters_that_start_no_token() {
        let lexed = lex("a _b; c\u{a0}");
        assert_eq!(
            lexed.errors,
            [
                LexError {
                    pos: Pos { line: 1, column: 2 },
                    token_index: 1
                },
                LexError {
                    pos: Pos { line: 1, column: 4 },
                    token_index: 2
                },
                LexError {
                    pos: Pos { line: 1, column: 7 },
                    token_index: 3
                },
            ]
        );
        let names: Vec<&str> = lexed.tokens.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", ""]);
    }
}
