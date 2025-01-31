use {
    crate::{Position, Span},
    std::{iter::Peekable, str::Chars},
    unicode_width::UnicodeWidthChar,
    unicode_xid::UnicodeXID,
};

/// The tokens that can emerge from the lexer
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Token {
    Error,
    Whitespace,
    NewLine,
    NewLineIndented,
    CommentSingle,
    CommentMulti,
    Number,
    Id,

    SingleQuote,
    DoubleQuote,
    StringLiteral,

    // Symbols
    At,
    Colon,
    Comma,
    Dollar,
    Dot,
    Ellipsis,
    Function,
    RoundOpen,
    RoundClose,
    SquareOpen,
    SquareClose,
    CurlyOpen,
    CurlyClose,
    Wildcard,
    Range,
    RangeInclusive,

    // operators
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,

    Assign,
    AssignAdd,
    AssignSubtract,
    AssignMultiply,
    AssignDivide,
    AssignModulo,

    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,

    Pipe,

    // Keywords
    And,
    Break,
    Catch,
    Continue,
    Debug,
    Else,
    ElseIf,
    Export,
    False,
    Finally,
    For,
    From,
    If,
    Import,
    In,
    Loop,
    Match,
    Not,
    Num2,
    Num4,
    Or,
    Return,
    Switch,
    Then,
    Throw,
    True,
    Try,
    Until,
    While,
    Yield,
}

impl Token {
    /// Returns true if the token should be counted as whitespace
    pub fn is_whitespace(&self) -> bool {
        use Token::*;
        matches!(self, Whitespace | CommentMulti | CommentSingle)
    }

    /// Returns true if the token represents a new line
    pub fn is_newline(&self) -> bool {
        use Token::*;
        matches!(self, NewLine | NewLineIndented)
    }
}

// Used to keep track of different lexing modes while working through a string
#[derive(Clone)]
enum StringMode {
    // Inside a string literal, expecting an end quote
    Literal(char),
    // Just after a $ symbol, either an id or a '{' will follow
    TemplateStart,
    // Inside a string template, e.g. '${...}'
    TemplateExpression,
    // Inside an inline map in a template expression, e.g. '${foo({bar: 42})}'
    // A closing '}' will end the map rather than the template expression.
    TemplateExpressionInlineMap,
}

// Separates the input source into Tokens
//
// TokenLexer is the internal implementation, KotoLexer provides the external interface.
#[derive(Clone)]
struct TokenLexer<'a> {
    // The input source
    source: &'a str,
    // The current position in the source
    current_byte: usize,
    // Used to provide the token's slice
    previous_byte: usize,
    // A cache of the previous token that was emitted
    previous_token: Option<Token>,
    // The span represented by the current token
    span: Span,
    // The indentation of the current line
    indent: usize,
    // A stack of string modes, allowing for nested mode changes while parsing strings
    string_mode_stack: Vec<StringMode>,
}

impl<'a> TokenLexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            previous_byte: 0,
            current_byte: 0,
            indent: 0,
            previous_token: None,
            span: Span::default(),
            string_mode_stack: vec![],
        }
    }

    // The slice associated with the token that was just emitted
    fn slice(&self) -> &'a str {
        &self.source[self.previous_byte..self.current_byte]
    }

    fn current_position(&self) -> Position {
        self.span.end
    }

    fn advance_line(&mut self, char_bytes: usize) {
        self.advance_line_utf8(char_bytes, char_bytes);
    }

    fn advance_line_utf8(&mut self, char_bytes: usize, char_count: usize) {
        self.previous_byte = self.current_byte;
        self.current_byte += char_bytes;

        let previous_end = self.span.end;
        self.span = Span {
            start: previous_end,
            end: Position {
                line: previous_end.line,
                column: previous_end.column + char_count as u32,
            },
        };
    }

    fn advance_to_position(&mut self, char_bytes: usize, position: Position) {
        self.previous_byte = self.current_byte;
        self.current_byte += char_bytes;

        self.span = Span {
            start: self.span.end,
            end: position,
        };
    }

    fn consume_newline(&mut self, mut chars: Peekable<Chars>) -> Token {
        use Token::*;

        let mut consumed_bytes = 1;
        let mut newline_bytes = 1;

        if chars.peek() == Some(&'\r') {
            consumed_bytes += 1;
            newline_bytes += 1;
            chars.next();
        }

        match chars.next() {
            Some('\n') => {}
            _ => return Error,
        }

        consumed_bytes += consume_and_count(&mut chars, is_whitespace);

        self.indent = consumed_bytes - newline_bytes;
        self.advance_to_position(
            consumed_bytes,
            Position {
                line: self.current_position().line + 1,
                column: (consumed_bytes - newline_bytes + 1) as u32, // indexing from 1 for column
            },
        );

        if self.indent == 0 {
            NewLine
        } else {
            NewLineIndented
        }
    }

    fn consume_comment(&mut self, mut chars: Peekable<Chars>) -> Token {
        use Token::*;

        // The # symbol has already been matched
        chars.next();

        if chars.peek() == Some(&'-') {
            // multi-line comment
            let mut char_bytes = 1;
            let mut nest_count = 1;
            let mut position = self.current_position();
            while let Some(c) = chars.next() {
                char_bytes += c.len_utf8();
                position.column += c.width().unwrap_or(0) as u32;
                match c {
                    '#' => {
                        if chars.peek() == Some(&'-') {
                            chars.next();
                            char_bytes += 1;
                            position.column += 1;
                            nest_count += 1;
                        }
                    }
                    '-' => {
                        if chars.peek() == Some(&'#') {
                            chars.next();
                            char_bytes += 1;
                            position.column += 1;
                            nest_count -= 1;
                            if nest_count == 0 {
                                break;
                            }
                        }
                    }
                    '\r' => {
                        if chars.next() != Some('\n') {
                            return Error;
                        }
                        char_bytes += 1;
                        position.line += 1;
                        position.column = 1;
                    }
                    '\n' => {
                        position.line += 1;
                        position.column = 1;
                    }
                    _ => {}
                }
            }

            self.advance_to_position(char_bytes, position);

            if nest_count == 0 {
                CommentMulti
            } else {
                Error
            }
        } else {
            // single-line comment
            let (comment_bytes, comment_width) =
                consume_and_count_utf8(&mut chars, |c| !matches!(c, '\r' | '\n'));
            self.advance_line_utf8(comment_bytes + 1, comment_width + 1);
            CommentSingle
        }
    }

    fn consume_string_literal(&mut self, mut chars: Peekable<Chars>) -> Token {
        use Token::*;

        let string_quote = match self.string_mode_stack.last() {
            Some(StringMode::Literal(quote)) => *quote,
            _ => return Error,
        };

        let mut string_bytes = 0;
        let mut position = self.current_position();

        while let Some(c) = chars.peek().cloned() {
            match c {
                _ if c == string_quote => {
                    self.advance_to_position(string_bytes, position);
                    return StringLiteral;
                }
                '$' => {
                    self.advance_to_position(string_bytes, position);
                    return StringLiteral;
                }
                '\\' => {
                    chars.next();
                    string_bytes += 1;
                    position.column += 1;

                    let skip_next_char = match chars.peek() {
                        Some('$') => true,
                        Some('\\') => true,
                        Some(c) if *c == string_quote => true,
                        _ => false,
                    };

                    if skip_next_char {
                        chars.next();
                        string_bytes += 1;
                        position.column += 1;
                    }
                }
                '\r' => {
                    chars.next();
                    if chars.next() != Some('\n') {
                        return Error;
                    }
                    string_bytes += 2;
                    position.line += 1;
                    position.column = 1;
                }
                '\n' => {
                    chars.next();
                    string_bytes += 1;
                    position.line += 1;
                    position.column = 1;
                }
                _ => {
                    chars.next();
                    string_bytes += c.len_utf8();
                    position.column += c.width().unwrap_or(0) as u32;
                }
            }
        }

        Error
    }

    fn consume_number(&mut self, mut chars: Peekable<Chars>) -> Token {
        use Token::*;

        let has_leading_zero = chars.peek() == Some(&'0');
        let mut char_bytes = consume_and_count(&mut chars, is_digit);
        let mut allow_exponent = true;

        match chars.peek() {
            Some(&'b') if has_leading_zero && char_bytes == 1 => {
                chars.next();
                char_bytes += 1 + consume_and_count(&mut chars, is_binary_digit);
                allow_exponent = false;
            }
            Some(&'o') if has_leading_zero && char_bytes == 1 => {
                chars.next();
                char_bytes += 1 + consume_and_count(&mut chars, is_octal_digit);
                allow_exponent = false;
            }
            Some(&'x') if has_leading_zero && char_bytes == 1 => {
                chars.next();
                char_bytes += 1 + consume_and_count(&mut chars, is_hex_digit);
                allow_exponent = false;
            }
            Some(&'.') => {
                chars.next();

                match chars.peek() {
                    Some(c) if is_digit(*c) => {}
                    Some(&'e') => {
                        // lookahead to check that this isn't a function call starting with 'e'
                        // e.g. 1.exp()
                        let mut lookahead = chars.clone();
                        lookahead.next();
                        match lookahead.peek() {
                            Some(c) if is_digit(*c) => {}
                            Some(&'+') | Some(&'-') => {}
                            _ => {
                                self.advance_line(char_bytes);
                                return Number;
                            }
                        }
                    }
                    _ => {
                        self.advance_line(char_bytes);
                        return Number;
                    }
                }

                char_bytes += 1 + consume_and_count(&mut chars, is_digit);
            }
            _ => {}
        }

        if chars.peek() == Some(&'e') && allow_exponent {
            chars.next();
            char_bytes += 1;

            if matches!(chars.peek(), Some(&'+') | Some(&'-')) {
                chars.next();
                char_bytes += 1;
            }

            char_bytes += consume_and_count(&mut chars, is_digit);
        }

        self.advance_line(char_bytes);
        Number
    }

    fn consume_id_or_keyword(&mut self, mut chars: Peekable<Chars>) -> Token {
        use Token::*;

        // The first character has already been matched
        let c = chars.next().unwrap();

        let (char_bytes, char_count) = consume_and_count_utf8(&mut chars, is_id_continue);
        let char_bytes = c.len_utf8() + char_bytes;
        let char_count = 1 + char_count;

        let id = &self.source[self.current_byte..self.current_byte + char_bytes];

        if id == "else" {
            if self
                .source
                .get(self.current_byte..self.current_byte + char_bytes + 3)
                == Some("else if")
            {
                self.advance_line(7);
                return ElseIf;
            } else {
                self.advance_line(4);
                return Else;
            }
        }

        macro_rules! check_keyword {
            ($keyword:expr, $token:ident) => {
                if id == $keyword {
                    self.advance_line($keyword.len());
                    return $token;
                }
            };
        }

        if !matches!(self.previous_token, Some(Token::Dot)) {
            check_keyword!("and", And);
            check_keyword!("break", Break);
            check_keyword!("catch", Catch);
            check_keyword!("continue", Continue);
            check_keyword!("debug", Debug);
            check_keyword!("export", Export);
            check_keyword!("false", False);
            check_keyword!("finally", Finally);
            check_keyword!("for", For);
            check_keyword!("from", From);
            check_keyword!("if", If);
            check_keyword!("import", Import);
            check_keyword!("in", In);
            check_keyword!("loop", Loop);
            check_keyword!("match", Match);
            check_keyword!("not", Not);
            check_keyword!("num2", Num2);
            check_keyword!("num4", Num4);
            check_keyword!("or", Or);
            check_keyword!("return", Return);
            check_keyword!("switch", Switch);
            check_keyword!("then", Then);
            check_keyword!("throw", Throw);
            check_keyword!("true", True);
            check_keyword!("try", Try);
            check_keyword!("until", Until);
            check_keyword!("while", While);
            check_keyword!("yield", Yield);
        }

        // If no keyword matched, then consume as an Id
        self.advance_line_utf8(char_bytes, char_count);
        Token::Id
    }

    fn consume_symbol(&mut self, remaining: &str) -> Option<Token> {
        use Token::*;

        macro_rules! check_symbol {
            ($token_str:expr, $token:ident) => {
                if remaining.starts_with($token_str) {
                    self.advance_line($token_str.len());
                    return Some($token);
                }
            };
        }

        check_symbol!("...", Ellipsis);

        check_symbol!("..=", RangeInclusive);
        check_symbol!("..", Range);

        check_symbol!(">>", Pipe);

        check_symbol!("==", Equal);
        check_symbol!("!=", NotEqual);
        check_symbol!(">=", GreaterOrEqual);
        check_symbol!("<=", LessOrEqual);
        check_symbol!(">", Greater);
        check_symbol!("<", Less);

        check_symbol!("+=", AssignAdd);
        check_symbol!("-=", AssignSubtract);
        check_symbol!("*=", AssignMultiply);
        check_symbol!("/=", AssignDivide);
        check_symbol!("%=", AssignModulo);
        check_symbol!("=", Assign);

        check_symbol!("+", Add);
        check_symbol!("-", Subtract);
        check_symbol!("*", Multiply);
        check_symbol!("/", Divide);
        check_symbol!("%", Modulo);

        check_symbol!("@", At);
        check_symbol!(":", Colon);
        check_symbol!(",", Comma);
        check_symbol!(".", Dot);
        check_symbol!("(", RoundOpen);
        check_symbol!(")", RoundClose);
        check_symbol!("|", Function);
        check_symbol!("[", SquareOpen);
        check_symbol!("]", SquareClose);
        check_symbol!("{", CurlyOpen);
        check_symbol!("}", CurlyClose);
        check_symbol!("_", Wildcard);

        None
    }

    fn get_next_token(&mut self) -> Option<Token> {
        use Token::*;

        let result = match self.source.get(self.current_byte..) {
            Some(remaining) if !remaining.is_empty() => {
                let mut chars = remaining.chars().peekable();
                let next_char = *chars.peek().unwrap(); // At least one char is remaining

                let string_mode = self.string_mode_stack.last().cloned();

                match string_mode {
                    Some(StringMode::Literal(quote)) => match next_char {
                        '"' if quote == '"' => {
                            self.advance_line(1);
                            self.string_mode_stack.pop();
                            Some(DoubleQuote)
                        }
                        '\'' if quote == '\'' => {
                            self.advance_line(1);
                            self.string_mode_stack.pop();
                            Some(SingleQuote)
                        }
                        '$' => {
                            self.advance_line(1);
                            self.string_mode_stack.push(StringMode::TemplateStart);
                            Some(Dollar)
                        }
                        _ => Some(self.consume_string_literal(chars)),
                    },
                    Some(StringMode::TemplateStart) => match next_char {
                        _ if is_id_start(next_char) => match self.consume_id_or_keyword(chars) {
                            Id => {
                                self.string_mode_stack.pop();
                                Some(Id)
                            }
                            _ => Some(Error),
                        },
                        '{' => {
                            self.advance_line(1);
                            self.string_mode_stack.pop();
                            self.string_mode_stack.push(StringMode::TemplateExpression);
                            Some(CurlyOpen)
                        }
                        _ => Some(Error),
                    },
                    _ => match next_char {
                        c if is_whitespace(c) => {
                            let count = consume_and_count(&mut chars, is_whitespace);
                            self.advance_line(count);
                            Some(Whitespace)
                        }
                        '\r' | '\n' => Some(self.consume_newline(chars)),
                        '#' => Some(self.consume_comment(chars)),
                        '"' => {
                            self.advance_line(1);
                            self.string_mode_stack.push(StringMode::Literal('"'));
                            Some(DoubleQuote)
                        }
                        '\'' => {
                            self.advance_line(1);
                            self.string_mode_stack.push(StringMode::Literal('\''));
                            Some(SingleQuote)
                        }
                        '0'..='9' => Some(self.consume_number(chars)),
                        c if is_id_start(c) => Some(self.consume_id_or_keyword(chars)),
                        _ => {
                            let result = self.consume_symbol(remaining).unwrap_or(Error);

                            use StringMode::*;
                            match result {
                                CurlyOpen => {
                                    if matches!(string_mode, Some(TemplateExpression)) {
                                        self.string_mode_stack.push(TemplateExpressionInlineMap);
                                    }
                                }
                                CurlyClose => {
                                    if matches!(
                                        string_mode,
                                        Some(TemplateExpression | TemplateExpressionInlineMap)
                                    ) {
                                        self.string_mode_stack.pop();
                                    }
                                }
                                _ => {}
                            }

                            Some(result)
                        }
                    },
                }
            }
            _ => None,
        };

        self.previous_token = result;
        result
    }
}

impl<'a> Iterator for TokenLexer<'a> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        self.get_next_token()
    }
}

fn is_digit(c: char) -> bool {
    matches!(c, '0'..='9')
}

fn is_binary_digit(c: char) -> bool {
    matches!(c, '0' | '1')
}

fn is_octal_digit(c: char) -> bool {
    matches!(c, '0'..='7')
}

fn is_hex_digit(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='f' | 'A'..='F')
}

fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

/// Returns true if the character matches the XID_Start Unicode property
pub fn is_id_start(c: char) -> bool {
    UnicodeXID::is_xid_start(c)
}

/// Returns true if the character matches the XID_Continue Unicode property
pub fn is_id_continue(c: char) -> bool {
    UnicodeXID::is_xid_continue(c)
}

fn consume_and_count(chars: &mut Peekable<Chars>, predicate: impl Fn(char) -> bool) -> usize {
    let mut char_bytes = 0;

    while let Some(c) = chars.peek() {
        if !predicate(*c) {
            break;
        }
        char_bytes += 1;
        chars.next();
    }

    char_bytes
}

fn consume_and_count_utf8(
    chars: &mut Peekable<Chars>,
    predicate: impl Fn(char) -> bool,
) -> (usize, usize) {
    let mut char_bytes = 0;
    let mut char_count = 0;

    while let Some(c) = chars.peek() {
        if !predicate(*c) {
            break;
        }
        char_bytes += c.len_utf8();
        char_count += c.width().unwrap_or(0);
        chars.next();
    }

    (char_bytes, char_count)
}

#[derive(Clone)]
struct PeekedToken<'a> {
    token: Option<Token>,
    slice: &'a str,
    span: Span,
    indent: usize,
    source_position: usize,
}

/// The lexer used by the Koto parser
///
/// Wraps a TokenLexer with unbounded lookahead, see peek_n().
#[derive(Clone)]
pub struct KotoLexer<'a> {
    lexer: TokenLexer<'a>,
    peeked_tokens: Vec<PeekedToken<'a>>,
    current_peek_index: usize,
}

impl<'a> KotoLexer<'a> {
    /// Initializes a lexer with the given input script
    pub fn new(source: &'a str) -> Self {
        Self {
            lexer: TokenLexer::new(source),
            peeked_tokens: Vec::new(),
            current_peek_index: 0,
        }
    }

    /// Returns the input source
    pub fn source(&self) -> &'a str {
        self.lexer.source
    }

    /// Returns the current position in the input source
    pub fn source_position(&self) -> usize {
        if self.peeked_tokens.is_empty() {
            self.lexer.current_byte
        } else {
            self.peeked_tokens[self.current_peek_index].source_position
        }
    }

    /// Peeks the next token in the output stream
    pub fn peek(&mut self) -> Option<Token> {
        if self.peeked_tokens.is_empty() {
            self.peek_n(0)
        } else {
            self.peeked_tokens[self.current_peek_index].token
        }
    }

    /// Peeks the nth token that will appear in the output stream
    ///
    /// peek_n(0) is equivalent to calling peek().
    /// peek_n(1) returns the token that will appear after that, and so forth.
    pub fn peek_n(&mut self, n: usize) -> Option<Token> {
        while self.peeked_tokens.len() - self.current_peek_index <= n {
            let span = self.lexer.span;
            let slice = self.lexer.slice();
            let indent = self.lexer.indent;
            let source_position = self.lexer.current_byte;
            // getting the token needs to happen after the other properties
            let token = self.lexer.next();
            self.peeked_tokens.push(PeekedToken {
                token,
                slice,
                span,
                indent,
                source_position,
            });
        }
        self.peeked_tokens[self.current_peek_index + n].token
    }

    /// Returns the current span
    pub fn span(&self) -> Span {
        if self.peeked_tokens.is_empty() {
            self.lexer.span
        } else {
            self.peeked_tokens[self.current_peek_index].span
        }
    }

    /// Returns the string slice of the input associated with the current token
    pub fn slice(&self) -> &'a str {
        if self.peeked_tokens.is_empty() {
            self.lexer.slice()
        } else {
            self.peeked_tokens[self.current_peek_index].slice
        }
    }

    /// Returns the indent associated with the current token
    pub fn current_indent(&self) -> usize {
        if self.peeked_tokens.is_empty() {
            self.lexer.indent
        } else {
            self.peeked_tokens[self.current_peek_index].indent
        }
    }

    /// Returns the indent associated with the nth coming token in the output stream
    pub fn peek_indent(&self, peek_index: usize) -> usize {
        self.peeked_tokens[self.current_peek_index + peek_index].indent
    }

    /// Returns the line number associated with the current token
    pub fn line_number(&self) -> u32 {
        self.span().end.line
    }

    /// Returns the line number associated with the nth coming token in the output stream
    pub fn peek_line_number(&self, peek_index: usize) -> u32 {
        self.peeked_tokens[self.current_peek_index + peek_index]
            .span
            .end
            .line
    }
}

impl<'a> Iterator for KotoLexer<'a> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        if self.peeked_tokens.is_empty() {
            self.lexer.next()
        } else {
            let result = self.peeked_tokens[self.current_peek_index].token;
            self.current_peek_index += 1;
            if self.current_peek_index == self.peeked_tokens.len() {
                self.peeked_tokens.clear();
                self.current_peek_index = 0;
            }
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Token::*, *};

    fn check_lexer_output(source: &str, tokens: &[(Token, Option<&str>, u32)]) {
        let mut lex = KotoLexer::new(source);

        for (i, (token, maybe_slice, line_number)) in tokens.iter().enumerate() {
            loop {
                match lex.next().expect("Expected token") {
                    Whitespace => continue,
                    output => {
                        assert_eq!(&output, token, "Token mismatch at position {}", i);
                        if let Some(slice) = maybe_slice {
                            assert_eq!(&lex.slice(), slice, "Slice mismatch at position {}", i);
                        }
                        assert_eq!(
                            lex.line_number() as u32,
                            *line_number,
                            "Line number mismatch at position {}",
                            i
                        );
                        break;
                    }
                }
            }
        }

        assert_eq!(lex.next(), None);
    }

    fn check_lexer_output_indented(source: &str, tokens: &[(Token, Option<&str>, u32, u32)]) {
        let mut lex = KotoLexer::new(source);

        for (i, (token, maybe_slice, line_number, indent)) in tokens.iter().enumerate() {
            loop {
                match lex.next().expect("Expected token") {
                    Whitespace => continue,
                    output => {
                        assert_eq!(&output, token, "Mismatch at token {}", i);
                        if let Some(slice) = maybe_slice {
                            assert_eq!(&lex.slice(), slice, "Mismatch at token {}", i);
                        }
                        assert_eq!(
                            lex.line_number() as u32,
                            *line_number,
                            "Line number - expected: {}, actual: {} - (token {} - {:?})",
                            *line_number,
                            lex.line_number(),
                            i,
                            token
                        );
                        assert_eq!(lex.current_indent() as u32, *indent, "Indent (token {})", i);
                        break;
                    }
                }
            }
        }

        assert_eq!(lex.next(), None);
    }

    #[test]
    fn ids() {
        let input = "id id1 id_2 i_d_3 ïd_ƒôûr if iff _";
        check_lexer_output(
            input,
            &[
                (Id, Some("id"), 1),
                (Id, Some("id1"), 1),
                (Id, Some("id_2"), 1),
                (Id, Some("i_d_3"), 1),
                (Id, Some("ïd_ƒôûr"), 1),
                (If, None, 1),
                (Id, Some("iff"), 1),
                (Wildcard, None, 1),
            ],
        );
    }

    #[test]
    fn indent() {
        let input = "\
if true then
  num4 1

num2 2
x
y";
        check_lexer_output_indented(
            input,
            &[
                (If, None, 1, 0),
                (True, None, 1, 0),
                (Then, None, 1, 0),
                (NewLineIndented, None, 2, 2),
                (Num4, None, 2, 2),
                (Number, Some("1"), 2, 2),
                (NewLine, None, 3, 0),
                (NewLine, None, 4, 0),
                (Num2, None, 4, 0),
                (Number, Some("2"), 4, 0),
                (NewLine, None, 5, 0),
                (Id, Some("x"), 5, 0),
                (NewLine, None, 6, 0),
                (Id, Some("y"), 6, 0),
            ],
        );
    }

    #[test]
    fn comments() {
        let input = "\
# single
true #-
multiline -
false #
-# true
()";
        check_lexer_output(
            input,
            &[
                (CommentSingle, Some("# single"), 1),
                (NewLine, None, 2),
                (True, None, 2),
                (CommentMulti, Some("#-\nmultiline -\nfalse #\n-#"), 5),
                (True, None, 5),
                (NewLine, None, 6),
                (RoundOpen, None, 6),
                (RoundClose, None, 6),
            ],
        );
    }

    #[test]
    fn strings() {
        let input = r#"
"hello, world!"
"escaped \\\"\n\$ string"
"double-\"quoted\" 'string'"
'single-\'quoted\' "string"'
""
"\\"
"#;

        check_lexer_output(
            input,
            &[
                (NewLine, None, 2),
                (DoubleQuote, None, 2),
                (StringLiteral, Some("hello, world!"), 2),
                (DoubleQuote, None, 2),
                (NewLine, None, 3),
                (DoubleQuote, None, 3),
                (StringLiteral, Some(r#"escaped \\\"\n\$ string"#), 3),
                (DoubleQuote, None, 3),
                (NewLine, None, 4),
                (DoubleQuote, None, 4),
                (StringLiteral, Some(r#"double-\"quoted\" 'string'"#), 4),
                (DoubleQuote, None, 4),
                (NewLine, None, 5),
                (SingleQuote, None, 5),
                (StringLiteral, Some(r#"single-\'quoted\' "string""#), 5),
                (SingleQuote, None, 5),
                (NewLine, None, 6),
                (DoubleQuote, None, 6),
                (DoubleQuote, None, 6),
                (NewLine, None, 7),
                (DoubleQuote, None, 7),
                (StringLiteral, Some(r#"\\"#), 7),
                (DoubleQuote, None, 7),
                (NewLine, None, 8),
            ],
        );
    }

    #[test]
    fn interpolated_string_ids() {
        let input = r#"
"hello $name, how are you?"
'$foo$bar'
"#;
        check_lexer_output(
            input,
            &[
                (NewLine, None, 2),
                (DoubleQuote, None, 2),
                (StringLiteral, Some("hello "), 2),
                (Dollar, None, 2),
                (Id, Some("name"), 2),
                (StringLiteral, Some(", how are you?"), 2),
                (DoubleQuote, None, 2),
                (NewLine, None, 3),
                (SingleQuote, None, 3),
                (Dollar, None, 3),
                (Id, Some("foo"), 3),
                (Dollar, None, 3),
                (Id, Some("bar"), 3),
                (SingleQuote, None, 3),
                (NewLine, None, 4),
            ],
        );
    }

    #[test]
    fn interpolated_string_expressions() {
        let input = r#"
"x + y == ${x + y}"
'${'{}'.format foo}'
"#;
        check_lexer_output(
            input,
            &[
                (NewLine, None, 2),
                (DoubleQuote, None, 2),
                (StringLiteral, Some("x + y == "), 2),
                (Dollar, None, 2),
                (CurlyOpen, None, 2),
                (Id, Some("x"), 2),
                (Add, None, 2),
                (Id, Some("y"), 2),
                (CurlyClose, None, 2),
                (DoubleQuote, None, 2),
                (NewLine, None, 3),
                (SingleQuote, None, 3),
                (Dollar, None, 3),
                (CurlyOpen, None, 3),
                (SingleQuote, None, 3),
                (StringLiteral, Some("{}"), 3),
                (SingleQuote, None, 3),
                (Dot, None, 3),
                (Id, Some("format"), 3),
                (Id, Some("foo"), 3),
                (CurlyClose, None, 3),
                (SingleQuote, None, 3),
                (NewLine, None, 4),
            ],
        );
    }

    #[test]
    fn operators() {
        let input = r#"
> >= >> < <=
"#;
        check_lexer_output(
            input,
            &[
                (NewLine, None, 2),
                (Greater, None, 2),
                (GreaterOrEqual, None, 2),
                (Pipe, None, 2),
                (Less, None, 2),
                (LessOrEqual, None, 2),
                (NewLine, None, 3),
            ],
        );
    }

    #[test]
    fn numbers() {
        let input = "\
123
55.5
-1e-3
0.5e+9
-8e8
0xabadcafe
0xABADCAFE
0o707606
0b1010101";
        check_lexer_output(
            input,
            &[
                (Number, Some("123"), 1),
                (NewLine, None, 2),
                (Number, Some("55.5"), 2),
                (NewLine, None, 3),
                (Subtract, None, 3),
                (Number, Some("1e-3"), 3),
                (NewLine, None, 4),
                (Number, Some("0.5e+9"), 4),
                (NewLine, None, 5),
                (Subtract, None, 5),
                (Number, Some("8e8"), 5),
                (NewLine, None, 6),
                (Number, Some("0xabadcafe"), 6),
                (NewLine, None, 7),
                (Number, Some("0xABADCAFE"), 7),
                (NewLine, None, 8),
                (Number, Some("0o707606"), 8),
                (NewLine, None, 9),
                (Number, Some("0b1010101"), 9),
            ],
        );
    }

    #[test]
    fn lookups_on_numbers() {
        let input = "\
1.0.sin()
-1e-3.abs()
1.min x
9.exp()";
        check_lexer_output(
            input,
            &[
                (Number, Some("1.0"), 1),
                (Dot, None, 1),
                (Id, Some("sin"), 1),
                (RoundOpen, None, 1),
                (RoundClose, None, 1),
                (NewLine, None, 2),
                (Subtract, None, 2),
                (Number, Some("1e-3"), 2),
                (Dot, None, 2),
                (Id, Some("abs"), 2),
                (RoundOpen, None, 2),
                (RoundClose, None, 2),
                (NewLine, None, 3),
                (Number, Some("1"), 3),
                (Dot, None, 3),
                (Id, Some("min"), 3),
                (Id, Some("x"), 3),
                (NewLine, None, 4),
                (Number, Some("9"), 4),
                (Dot, None, 4),
                (Id, Some("exp"), 4),
                (RoundOpen, None, 4),
                (RoundClose, None, 4),
            ],
        );
    }

    #[test]
    fn modify_assign() {
        let input = "\
a += 1
b -= 2
c *= 3";
        check_lexer_output(
            input,
            &[
                (Id, Some("a"), 1),
                (AssignAdd, None, 1),
                (Number, Some("1"), 1),
                (NewLine, None, 2),
                (Id, Some("b"), 2),
                (AssignSubtract, None, 2),
                (Number, Some("2"), 2),
                (NewLine, None, 3),
                (Id, Some("c"), 3),
                (AssignMultiply, None, 3),
                (Number, Some("3"), 3),
            ],
        );
    }

    #[test]
    fn ranges() {
        let input = "\
a[..=9]
x = [i for i in 0..5]";
        check_lexer_output(
            input,
            &[
                (Id, Some("a"), 1),
                (SquareOpen, None, 1),
                (RangeInclusive, None, 1),
                (Number, Some("9"), 1),
                (SquareClose, None, 1),
                (NewLine, None, 2),
                (Id, Some("x"), 2),
                (Assign, None, 2),
                (SquareOpen, None, 2),
                (Id, Some("i"), 2),
                (For, None, 2),
                (Id, Some("i"), 2),
                (In, None, 2),
                (Number, Some("0"), 2),
                (Range, None, 2),
                (Number, Some("5"), 2),
                (SquareClose, None, 2),
            ],
        );
    }

    #[test]
    fn function() {
        let input = "\
export f = |a, b...|
  c = a + b.size()
  c
f()";
        check_lexer_output_indented(
            input,
            &[
                (Export, None, 1, 0),
                (Id, Some("f"), 1, 0),
                (Assign, None, 1, 0),
                (Function, None, 1, 0),
                (Id, Some("a"), 1, 0),
                (Comma, None, 1, 0),
                (Id, Some("b"), 1, 0),
                (Ellipsis, None, 1, 0),
                (Function, None, 1, 0),
                (NewLineIndented, None, 2, 2),
                (Id, Some("c"), 2, 2),
                (Assign, None, 2, 2),
                (Id, Some("a"), 2, 2),
                (Add, None, 2, 2),
                (Id, Some("b"), 2, 2),
                (Dot, None, 2, 2),
                (Id, Some("size"), 2, 2),
                (RoundOpen, None, 2, 2),
                (RoundClose, None, 2, 2),
                (NewLineIndented, None, 3, 2),
                (Id, Some("c"), 3, 2),
                (NewLine, None, 4, 0),
                (Id, Some("f"), 4, 0),
                (RoundOpen, None, 4, 0),
                (RoundClose, None, 4, 0),
            ],
        );
    }

    #[test]
    fn if_inline() {
        let input = "1 + if true then 0 else 1";
        check_lexer_output(
            input,
            &[
                (Number, Some("1"), 1),
                (Add, None, 1),
                (If, None, 1),
                (True, None, 1),
                (Then, None, 1),
                (Number, Some("0"), 1),
                (Else, None, 1),
                (Number, Some("1"), 1),
            ],
        );
    }

    #[test]
    fn if_block() {
        let input = "\
if true
  0
else if false
  1
else
  0";
        check_lexer_output_indented(
            input,
            &[
                (If, None, 1, 0),
                (True, None, 1, 0),
                (NewLineIndented, None, 2, 2),
                (Number, Some("0"), 2, 2),
                (NewLine, None, 3, 0),
                (ElseIf, None, 3, 0),
                (False, None, 3, 0),
                (NewLineIndented, None, 4, 2),
                (Number, Some("1"), 4, 2),
                (NewLine, None, 5, 0),
                (Else, None, 5, 0),
                (NewLineIndented, None, 6, 2),
                (Number, Some("0"), 6, 2),
            ],
        );
    }

    #[test]
    fn map_lookup() {
        let input = "m.检验.foo[1].bär()";

        check_lexer_output(
            input,
            &[
                (Id, Some("m"), 1),
                (Dot, None, 1),
                (Id, Some("检验"), 1),
                (Dot, None, 1),
                (Id, Some("foo"), 1),
                (SquareOpen, None, 1),
                (Number, Some("1"), 1),
                (SquareClose, None, 1),
                (Dot, None, 1),
                (Id, Some("bär"), 1),
                (RoundOpen, None, 1),
                (RoundClose, None, 1),
            ],
        );
    }

    #[test]
    fn map_lookup_with_keyword_as_key() {
        let input = "foo.and()";

        check_lexer_output(
            input,
            &[
                (Id, Some("foo"), 1),
                (Dot, None, 1),
                (Id, Some("and"), 1),
                (RoundOpen, None, 1),
                (RoundClose, None, 1),
            ],
        );
    }

    #[test]
    fn windows_line_endings() {
        let input = "123\r\n456\r\n789";

        check_lexer_output(
            input,
            &[
                (Number, Some("123"), 1),
                (NewLine, None, 2),
                (Number, Some("456"), 2),
                (NewLine, None, 3),
                (Number, Some("789"), 3),
            ],
        );
    }
}
