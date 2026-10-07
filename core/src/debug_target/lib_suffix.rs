//! The `{$LIBSUFFIX}` a package's main source declares, read the way the
//! compiler reads it: comments and string literals are skipped, and the
//! conditional directives around the declaration are evaluated, so that the
//! multi-version pattern
//!
//! ```pascal
//! {$IFDEF VER350}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}
//! ```
//!
//! yields the suffix of the compiler that actually builds the package. Only
//! what a suffix declaration needs is understood: `IFDEF`/`IFNDEF`, `IF`/
//! `ELSEIF` over `Defined(…)`, `CompilerVersion` and `RTLVersion`, `ELSE`,
//! `ENDIF`/`IFEND`. A condition outside of that (`IFOPT`, a constant of the
//! program) is *unknown*: declarations under it are kept as possibilities,
//! and reported as ambiguous when they disagree, rather than guessed at.

/// What the evaluation needs to know about the build.
#[derive(Debug, Clone, Default)]
pub struct BuildSymbols {
    /// Conditional symbols in effect (`VER360`, `WIN64`, `DEBUG`, …), any casing.
    pub defined: Vec<String>,
    /// The value of `CompilerVersion` and `RTLVersion` (`36.0` for Delphi 12).
    pub compiler_version: f64,
}

impl BuildSymbols {
    fn is_defined(&self, symbol: &str) -> bool {
        self.defined.iter().any(|known| known.eq_ignore_ascii_case(symbol))
    }
}

/// The suffix a main source declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredSuffix {
    /// No `{$LIBSUFFIX}` applies.
    None,
    /// `{$LIBSUFFIX '290'}`.
    Literal(String),
    /// `{$LIBSUFFIX AUTO}`: the compiler's package version.
    Auto,
    /// Several declarations may apply and they differ; the conditions
    /// deciding between them could not be evaluated.
    Ambiguous(Vec<String>),
}

/// The truth of a conditional block: known, or depending on something this
/// reader does not evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truth {
    True,
    False,
    Unknown,
}

impl Truth {
    fn of(value: bool) -> Self {
        if value { Truth::True } else { Truth::False }
    }

    fn not(self) -> Self {
        match self {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown => Truth::Unknown,
        }
    }

    fn and(self, other: Truth) -> Self {
        match (self, other) {
            (Truth::False, _) | (_, Truth::False) => Truth::False,
            (Truth::True, Truth::True) => Truth::True,
            _ => Truth::Unknown,
        }
    }

    fn or(self, other: Truth) -> Self {
        match (self, other) {
            (Truth::True, _) | (_, Truth::True) => Truth::True,
            (Truth::False, Truth::False) => Truth::False,
            _ => Truth::Unknown,
        }
    }
}

/// One `IF…` block being read: whether its current branch is taken, and
/// whether an earlier branch already was (which rules out the later ones).
struct Block {
    branch: Truth,
    taken_before: Truth,
}

/// Reads the `{$LIBSUFFIX}` that applies in `source` for a build with `symbols`.
pub fn declared_suffix(source: &str, symbols: &BuildSymbols) -> DeclaredSuffix {
    let mut blocks: Vec<Block> = Vec::new();
    let mut certain: Option<DeclaredSuffix> = None;
    let mut possible: Vec<DeclaredSuffix> = Vec::new();

    for directive in directives(source) {
        let (name, argument) = split_directive(&directive);
        match name.to_ascii_uppercase().as_str() {
            "IFDEF" => blocks.push(open(Truth::of(symbols.is_defined(argument)))),
            "IFNDEF" => blocks.push(open(Truth::of(!symbols.is_defined(argument)))),
            "IF" => blocks.push(open(evaluate(argument, symbols))),
            "IFOPT" => blocks.push(open(Truth::Unknown)),
            "ELSEIF" => {
                if let Some(block) = blocks.last_mut() {
                    block.taken_before = block.taken_before.or(block.branch);
                    block.branch = block.taken_before.not().and(evaluate(argument, symbols));
                }
            }
            "ELSE" => {
                if let Some(block) = blocks.last_mut() {
                    block.taken_before = block.taken_before.or(block.branch);
                    block.branch = block.taken_before.not();
                }
            }
            "ENDIF" | "IFEND" => {
                blocks.pop();
            }
            "LIBSUFFIX" => {
                let Some(suffix) = suffix_of(argument) else { continue };
                match blocks.iter().fold(Truth::True, |state, block| state.and(block.branch)) {
                    // The compiler keeps the last declaration it meets.
                    Truth::True => {
                        certain = Some(suffix);
                        possible.clear();
                    }
                    Truth::Unknown => possible.push(suffix),
                    Truth::False => {}
                }
            }
            _ => {}
        }
    }

    if possible.is_empty() {
        return certain.unwrap_or(DeclaredSuffix::None);
    }
    // A declaration under an unknown condition may or may not replace the
    // certain one before it: every candidate must agree.
    let mut candidates: Vec<DeclaredSuffix> = certain.into_iter().chain(possible).collect();
    candidates.dedup();
    let first = candidates[0].clone();
    if candidates.iter().all(|candidate| *candidate == first) {
        return first;
    }
    DeclaredSuffix::Ambiguous(candidates.iter().map(describe).collect())
}

fn open(branch: Truth) -> Block {
    Block { branch, taken_before: Truth::False }
}

fn describe(suffix: &DeclaredSuffix) -> String {
    match suffix {
        DeclaredSuffix::Literal(text) => format!("'{text}'"),
        DeclaredSuffix::Auto => "AUTO".to_string(),
        _ => String::new(),
    }
}

/// `'290'`, `290` or `AUTO`.
fn suffix_of(argument: &str) -> Option<DeclaredSuffix> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    if argument.eq_ignore_ascii_case("AUTO") {
        return Some(DeclaredSuffix::Auto);
    }
    let literal = argument
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(argument);
    Some(DeclaredSuffix::Literal(literal.trim().to_string()))
}

fn split_directive(directive: &str) -> (&str, &str) {
    let directive = directive.trim();
    match directive.find(|c: char| c.is_whitespace()) {
        Some(end) => (&directive[..end], directive[end..].trim()),
        _ => (directive, ""),
    }
}

/// The compiler directives of `source` in order, without the `{$` … `}`
/// (or `(*$` … `*)`) around them. Comments and string literals are skipped,
/// so a commented-out directive is not one.
fn directives(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    let starts = |at: usize, text: &str| text.chars().enumerate().all(|(k, c)| chars.get(at + k) == Some(&c));
    while i < chars.len() {
        if starts(i, "//") {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if chars[i] == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != '\'' && chars[i] != '\n' {
                i += 1;
            }
            i += 1;
        } else if chars[i] == '{' || starts(i, "(*") {
            let (open_length, close) = if chars[i] == '{' { (1, "}") } else { (2, "*)") };
            let body_start = i + open_length;
            let mut end = body_start;
            while end < chars.len() && !starts(end, close) {
                end += 1;
            }
            if chars.get(body_start) == Some(&'$') {
                found.push(chars[body_start + 1..end.min(chars.len())].iter().collect());
            }
            i = end + close.len();
        } else {
            i += 1;
        }
    }
    found
}

// ─── `{$IF}` expressions ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Word(String),
    Operator(String),
    Open,
    Close,
}

/// A value inside an expression: a truth, a number, or something unknown.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Truth(Truth),
    Number(f64),
}

impl Value {
    fn truth(self) -> Truth {
        match self {
            Value::Truth(truth) => truth,
            Value::Number(_) => Truth::Unknown,
        }
    }
}

fn evaluate(expression: &str, symbols: &BuildSymbols) -> Truth {
    let mut parser = Parser { tokens: tokenize(expression), position: 0, symbols };
    let value = parser.disjunction();
    if parser.position < parser.tokens.len() {
        return Truth::Unknown;
    }
    value.truth()
}

fn tokenize(expression: &str) -> Vec<Token> {
    let chars: Vec<char> = expression.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            tokens.push(Token::Open);
            i += 1;
        } else if c == ')' {
            tokens.push(Token::Close);
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(text.parse().map(Token::Number).unwrap_or(Token::Word(text)));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                i += 1;
            }
            tokens.push(Token::Word(chars[start..i].iter().collect()));
        } else {
            let pair: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            if [">=", "<=", "<>"].contains(&pair.as_str()) {
                tokens.push(Token::Operator(pair));
                i += 2;
            } else {
                tokens.push(Token::Operator(c.to_string()));
                i += 1;
            }
        }
    }
    tokens
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    symbols: &'a BuildSymbols,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next_is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(found)) if found.eq_ignore_ascii_case(word))
    }

    fn disjunction(&mut self) -> Value {
        let mut value = self.conjunction();
        while self.next_is_word("or") {
            self.position += 1;
            let right = self.conjunction();
            value = Value::Truth(value.truth().or(right.truth()));
        }
        value
    }

    fn conjunction(&mut self) -> Value {
        let mut value = self.negation();
        while self.next_is_word("and") {
            self.position += 1;
            let right = self.negation();
            value = Value::Truth(value.truth().and(right.truth()));
        }
        value
    }

    fn negation(&mut self) -> Value {
        if self.next_is_word("not") {
            self.position += 1;
            return Value::Truth(self.negation().truth().not());
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Value {
        let left = self.primary();
        let Some(Token::Operator(operator)) = self.peek().cloned() else {
            return left;
        };
        self.position += 1;
        let right = self.primary();
        let (Value::Number(left), Value::Number(right)) = (left, right) else {
            return Value::Truth(Truth::Unknown);
        };
        let holds = match operator.as_str() {
            ">=" => left >= right,
            "<=" => left <= right,
            "<>" => left != right,
            ">" => left > right,
            "<" => left < right,
            "=" => left == right,
            _ => return Value::Truth(Truth::Unknown),
        };
        Value::Truth(Truth::of(holds))
    }

    fn primary(&mut self) -> Value {
        let Some(token) = self.peek().cloned() else {
            return Value::Truth(Truth::Unknown);
        };
        self.position += 1;
        match token {
            Token::Number(number) => Value::Number(number),
            Token::Open => {
                let value = self.disjunction();
                self.expect_close();
                value
            }
            Token::Word(word) if word.eq_ignore_ascii_case("Defined") => self.defined(),
            Token::Word(word) if word.eq_ignore_ascii_case("CompilerVersion") || word.eq_ignore_ascii_case("RTLVersion") => {
                Value::Number(self.symbols.compiler_version)
            }
            Token::Word(word) if word.eq_ignore_ascii_case("True") => Value::Truth(Truth::True),
            Token::Word(word) if word.eq_ignore_ascii_case("False") => Value::Truth(Truth::False),
            // `Declared(…)`, a constant of the program, …: skip its
            // arguments so the rest of the expression still parses.
            Token::Word(_) => {
                if self.peek() == Some(&Token::Open) {
                    self.position += 1;
                    self.skip_to_close();
                }
                Value::Truth(Truth::Unknown)
            }
            _ => Value::Truth(Truth::Unknown),
        }
    }

    fn defined(&mut self) -> Value {
        if self.peek() != Some(&Token::Open) {
            return Value::Truth(Truth::Unknown);
        }
        self.position += 1;
        let Some(Token::Word(symbol)) = self.peek().cloned() else {
            self.skip_to_close();
            return Value::Truth(Truth::Unknown);
        };
        self.position += 1;
        self.expect_close();
        Value::Truth(Truth::of(self.symbols.is_defined(&symbol)))
    }

    fn expect_close(&mut self) {
        if self.peek() == Some(&Token::Close) {
            self.position += 1;
        }
    }

    fn skip_to_close(&mut self) {
        let mut depth = 1;
        while let Some(token) = self.peek().cloned() {
            self.position += 1;
            match token {
                Token::Open => depth += 1,
                Token::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delphi_12() -> BuildSymbols {
        BuildSymbols {
            defined: vec!["VER360".into(), "MSWINDOWS".into(), "WIN64".into(), "DEBUG".into()],
            compiler_version: 36.0,
        }
    }

    fn suffix(source: &str) -> DeclaredSuffix {
        declared_suffix(source, &delphi_12())
    }

    fn literal(text: &str) -> DeclaredSuffix {
        DeclaredSuffix::Literal(text.to_string())
    }

    #[test]
    fn reads_the_quoted_the_bare_and_the_automatic_form() {
        assert_eq!(suffix("package P;\n{$LIBSUFFIX 'D29'}\nend."), literal("D29"));
        assert_eq!(suffix("package P;\n{$LIBSUFFIX 290}\nend."), literal("290"));
        assert_eq!(suffix("package P;\n{$libsuffix auto}\nend."), DeclaredSuffix::Auto);
        assert_eq!(suffix("package P;\n(*$LIBSUFFIX '290'*)\nend."), literal("290"));
        assert_eq!(suffix("package P;\nend."), DeclaredSuffix::None);
    }

    #[test]
    fn takes_the_branch_of_the_compiler_that_builds_the_package() {
        let source = "{$IFDEF VER350}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IFDEF VER360}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IFNDEF VER360}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), DeclaredSuffix::None);
    }

    #[test]
    fn evaluates_if_expressions_over_the_compiler_version_and_defined() {
        let source = "{$IF CompilerVersion >= 36}{$LIBSUFFIX '290'}{$ELSEIF CompilerVersion >= 35}{$LIBSUFFIX '280'}\
                      {$ELSE}{$LIBSUFFIX '270'}{$IFEND}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IF Defined(WIN32) or (not Defined(DEBUG))}{$LIBSUFFIX 'A'}{$ELSE}{$LIBSUFFIX 'B'}{$ENDIF}";
        assert_eq!(suffix(source), literal("B"));
        let source = "{$IF Defined(WIN64) and (RTLVersion < 37.0)}{$LIBSUFFIX 'A'}{$ENDIF}";
        assert_eq!(suffix(source), literal("A"));
    }

    #[test]
    fn nested_blocks_need_every_level_to_hold() {
        let source = "{$IFDEF MSWINDOWS}{$IFDEF VER350}{$LIBSUFFIX '280'}{$ENDIF}{$IFDEF VER360}{$LIBSUFFIX '290'}{$ENDIF}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
    }

    #[test]
    fn a_commented_out_or_quoted_directive_is_not_one() {
        assert_eq!(suffix("// {$LIBSUFFIX '1'}\n{ {$LIBSUFFIX '2' }\n(* {$LIBSUFFIX '3'} *)\n{$LIBSUFFIX '4'}"), literal("4"));
        assert_eq!(suffix("const S = '{$LIBSUFFIX ''1''}';"), DeclaredSuffix::None);
    }

    #[test]
    fn the_last_declaration_that_applies_wins() {
        assert_eq!(suffix("{$LIBSUFFIX '1'}{$LIBSUFFIX '2'}"), literal("2"));
    }

    #[test]
    fn a_condition_it_cannot_evaluate_makes_differing_declarations_ambiguous() {
        let source = "{$IFOPT D+}{$LIBSUFFIX 'D'}{$ELSE}{$LIBSUFFIX 'R'}{$ENDIF}";
        assert_eq!(suffix(source), DeclaredSuffix::Ambiguous(vec!["'D'".into(), "'R'".into()]));
        let source = "{$IF SomeConstant > 3}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$LIBSUFFIX 'A'}{$IF Declared(Foo)}{$LIBSUFFIX 'B'}{$ENDIF}";
        assert_eq!(suffix(source), DeclaredSuffix::Ambiguous(vec!["'A'".into(), "'B'".into()]));
    }
}
