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
//! `ENDIF`/`IFEND`, and the source's own `DEFINE`/`UNDEF`. A condition
//! outside of that (`IFOPT`, a constant of the program, a symbol an include
//! file or the command line may define) is *unknown*: every suffix such a
//! condition could leave in force is kept — "none at all" among them — and
//! reported as ambiguous when they disagree, rather than guessed at.

/// What the evaluation needs to know about the build.
#[derive(Debug, Clone, Default)]
pub struct BuildSymbols {
    /// Conditional symbols in effect (`VER360`, `WIN64`, `DEBUG`, …), any casing.
    pub defined: Vec<String>,
    /// The value of `CompilerVersion` and `RTLVersion` (`36.0` for Delphi 12),
    /// where the build knows it exactly. A compiler configuration records a
    /// whole number, so the one release with a fractional version — Delphi
    /// 2007, at `18.5` — has none to offer and comparisons over it decide
    /// nothing.
    pub compiler_version: Option<f64>,
    /// Whether `defined` carries the symbols of the target platform, which
    /// only the platforms the builder knows do. Without them the platform
    /// family is undecided, not absent.
    pub platform_known: bool,
    /// Whether `defined` carries every `VERxxx` the compiler declares. A
    /// release may declare more than one — Delphi 2007 is both `VER180` and
    /// `VER185`, being a non-breaking release — and the pre-XE compiler
    /// configurations record a single one, so there the family decides
    /// nothing.
    pub version_symbols_known: bool,
}

/// Symbols the compiler settles from its own version, whatever is built.
const VERSION_DECIDED: [&str; 2] = ["CONDITIONALEXPRESSIONS", "UNICODE"];

/// Symbols the compiler settles from the target platform — its OS and CPU.
/// It defines exactly those of them that hold for the target, so for a
/// platform the builder knows, a name from this list that is missing from
/// `defined` is certainly absent. Two kinds are deliberately absent from it:
/// symbols that follow from the project rather than the platform (`CONSOLE`
/// from the application type, `DEBUG`/`RELEASE` from the configuration),
/// which an include file or the command line reaches; and the toolchain
/// markers (`EXTERNALLINKER`, `ALIGN_STACK`, `PC_MAPPED_EXCEPTIONS`,
/// `UNDERSCOREIMPORTNAME`), whose Windows status differs between the
/// classic and the LLVM back end — `win64x` is the latter and shares this
/// list. They decide no suffix worth a wrong answer.
const PLATFORM_DECIDED: [&str; 29] = [
    "MSWINDOWS",
    "WIN32",
    "WIN64",
    "LINUX",
    "LINUX32",
    "LINUX64",
    "POSIX",
    "POSIX32",
    "POSIX64",
    "MACOS",
    "MACOS32",
    "MACOS64",
    "ANDROID",
    "ANDROID32",
    "ANDROID64",
    "IOS",
    "IOS32",
    "IOS64",
    "CPUX86",
    "CPUX64",
    "CPU386",
    "CPUARM",
    "CPUARM32",
    "CPUARM64",
    "CPU32BITS",
    "CPU64BITS",
    "NEXTGEN",
    "AUTOREFCOUNT",
    "ELF",
];

impl BuildSymbols {
    /// A symbol of the build is true; a missing one is false only where its
    /// absence is knowable — the `VERxxx` family where the build records all
    /// of them, and the symbols the compiler itself settles for this target.
    /// Any other name may come from an include file or the command line and
    /// stays unknown.
    fn truth_of(&self, symbol: &str) -> Truth {
        if self.defined.iter().any(|known| known.eq_ignore_ascii_case(symbol)) {
            return Truth::True;
        }
        if is_version_symbol(symbol) {
            return match self.version_symbols_known {
                true => Truth::False,
                false => Truth::Unknown,
            };
        }
        if lists(&VERSION_DECIDED, symbol) {
            return Truth::False;
        }
        if self.platform_known && lists(&PLATFORM_DECIDED, symbol) {
            return Truth::False;
        }
        Truth::Unknown
    }
}

/// The symbol a directive names: the compiler reads one identifier and
/// ignores the rest of the line, as the stock `.dpk` template relies on
/// (`{$IFDEF IMPLICITBUILDING This IFDEF should not be used by users}`).
fn first_word(argument: &str) -> &str {
    argument.split_whitespace().next().unwrap_or(argument)
}

fn lists(symbols: &[&str], symbol: &str) -> bool {
    symbols.iter().any(|known| known.eq_ignore_ascii_case(symbol))
}

fn is_version_symbol(symbol: &str) -> bool {
    let Some(head) = symbol.get(..3) else {
        return false;
    };
    head.eq_ignore_ascii_case("VER") && symbol.len() > 3 && symbol[3..].bytes().all(|byte| byte.is_ascii_digit())
}

/// The symbols while reading one source: those of the build, under those the
/// source itself has `{$DEFINE}`d or `{$UNDEF}`d up to this point.
struct Symbols<'a> {
    build: &'a BuildSymbols,
    local: Vec<(String, Truth)>,
}

impl Symbols<'_> {
    fn truth_of(&self, symbol: &str) -> Truth {
        match self.local.iter().find(|(name, _)| name.eq_ignore_ascii_case(symbol)) {
            Some((_, truth)) => *truth,
            None => self.build.truth_of(symbol),
        }
    }

    /// A `{$DEFINE}`/`{$UNDEF}` applies from here on, and only as far as the
    /// branch around it is taken. Under an unknown condition it can only
    /// add what it sets: a symbol that already has that truth keeps it, any
    /// other becomes unknown.
    fn set(&mut self, argument: &str, defined: Truth, state: Truth) {
        let Some(symbol) = argument.split_whitespace().next() else {
            return;
        };
        if state == Truth::False {
            return;
        }
        let truth = match state {
            Truth::True => defined,
            _ if self.truth_of(symbol) == defined => defined,
            _ => Truth::Unknown,
        };
        match self.local.iter_mut().find(|(name, _)| name.eq_ignore_ascii_case(symbol)) {
            Some(entry) => entry.1 = truth,
            None => self.local.push((symbol.to_string(), truth)),
        }
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
    /// Several outcomes are possible and they differ — declaring no suffix
    /// at all among them; the conditions deciding between them could not be
    /// evaluated.
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

/// One `IF…` block being read. A branch is evaluated as if it were taken;
/// whether it is decides, when the block closes, which of the branches'
/// outcomes survive.
struct Block {
    /// Whether the branch now open is taken: its own condition, and no
    /// earlier branch of the block taken before it.
    branch: Truth,
    /// The disjunction of the branches' conditions as written, which an
    /// `{$ELSE}` completes to `True`. Only the raw conditions compose:
    /// `{$ELSE}` after a true `{$ELSEIF}` is `False`, not unknown.
    conditions: Truth,
    /// The suffixes possible where the block was entered.
    entered_with: Vec<DeclaredSuffix>,
    /// Those its finished branches leave behind.
    from_branches: Vec<DeclaredSuffix>,
}

/// Reads the `{$LIBSUFFIX}` that applies in `source` for a build with `symbols`.
pub fn declared_suffix(source: &str, symbols: &BuildSymbols) -> DeclaredSuffix {
    let mut known = Symbols { build: symbols, local: Vec::new() };
    let mut blocks: Vec<Block> = Vec::new();
    // What can be in force here; before the first declaration, nothing is.
    let mut possible = vec![DeclaredSuffix::None];
    // Every suffix the source names, and whether its conditional structure
    // came out even. A branch directive with nothing open means the block
    // was opened somewhere this reader does not see — an `{$I}` include, as
    // a rule — so the structure is unknown and nothing in the file can be
    // held to apply for certain.
    let mut declared: Vec<DeclaredSuffix> = Vec::new();
    let mut structure_understood = true;

    for directive in directives(source) {
        let (name, argument) = split_directive(&directive);
        match name.to_ascii_uppercase().as_str() {
            "IFDEF" => blocks.push(open(known.truth_of(first_word(argument)), &possible)),
            "IFNDEF" => blocks.push(open(known.truth_of(first_word(argument)).not(), &possible)),
            "IF" => blocks.push(open(evaluate(argument, &known), &possible)),
            "IFOPT" => blocks.push(open(Truth::Unknown, &possible)),
            "DEFINE" => known.set(argument, Truth::True, state(&blocks)),
            "UNDEF" => known.set(argument, Truth::False, state(&blocks)),
            "ELSEIF" => {
                let condition = evaluate(argument, &known);
                match blocks.last_mut() {
                    Some(block) => {
                        end_branch(block, &mut possible);
                        block.branch = block.conditions.not().and(condition);
                        block.conditions = block.conditions.or(condition);
                    }
                    _ => structure_understood = false,
                }
            }
            "ELSE" => {
                match blocks.last_mut() {
                    Some(block) => {
                        end_branch(block, &mut possible);
                        block.branch = block.conditions.not();
                        block.conditions = Truth::True;
                    }
                    _ => structure_understood = false,
                }
            }
            "ENDIF" | "IFEND" => {
                match blocks.pop() {
                    Some(block) => close_block(block, &mut possible),
                    _ => structure_understood = false,
                }
            }
            "LIBSUFFIX" => {
                let Some(suffix) = suffix_of(argument) else { continue };
                declared.push(suffix.clone());
                // Read as if the branch around it were taken, the
                // declaration replaces what held before it — the compiler
                // keeps the last one it meets.
                if state(&blocks) != Truth::False {
                    possible = vec![suffix];
                }
            }
            _ => {}
        }
    }

    // A block the source never closed was not understood to its end; it is
    // closed here so that what it may have left behind still counts as
    // possible, rather than the last declaration inside it passing for the
    // one in force.
    while let Some(block) = blocks.pop() {
        close_block(block, &mut possible);
    }
    // With the structure in doubt, no declaration can be ruled out and none
    // can be relied on: every one the source names stays a possibility.
    if !structure_understood {
        declared.extend(possible);
        possible = declared;
    }

    deduplicate(&mut possible);
    if let [only] = possible.as_slice() {
        return only.clone();
    }
    DeclaredSuffix::Ambiguous(possible.iter().map(describe).collect())
}

/// Ends a block: what it leaves possible is what its taken branches leave,
/// plus — unless one branch is certainly taken — what held when it was
/// entered. Deduplicating here and not only at the end is what keeps the
/// set the size of the distinct suffixes: without it every undecidable
/// block doubles it, whether or not it declares anything.
fn close_block(mut block: Block, possible: &mut Vec<DeclaredSuffix>) {
    end_branch(&mut block, possible);
    *possible = match block.conditions {
        Truth::True => Vec::new(),
        _ => block.entered_with,
    };
    possible.extend(block.from_branches);
    deduplicate(possible);
}

fn open(condition: Truth, possible: &[DeclaredSuffix]) -> Block {
    Block { branch: condition, conditions: condition, entered_with: possible.to_vec(), from_branches: Vec::new() }
}

/// Ends the branch now open: what it leaves possible is one of the block's
/// outcomes, unless the branch is not taken at all. The next branch starts
/// again from what held when the block was entered.
fn end_branch(block: &mut Block, possible: &mut Vec<DeclaredSuffix>) {
    if block.branch != Truth::False {
        block.from_branches.append(possible);
    }
    *possible = block.entered_with.clone();
}

/// Whether what follows is reached: every open block's branch must hold.
fn state(blocks: &[Block]) -> Truth {
    blocks.iter().fold(Truth::True, |state, block| state.and(block.branch))
}

/// Keeps the first of each repeated suffix, wherever the repeats stand.
fn deduplicate(suffixes: &mut Vec<DeclaredSuffix>) {
    let mut kept: Vec<DeclaredSuffix> = Vec::new();
    for suffix in suffixes.iter() {
        if !kept.contains(suffix) {
            kept.push(suffix.clone());
        }
    }
    *suffixes = kept;
}

fn describe(suffix: &DeclaredSuffix) -> String {
    match suffix {
        DeclaredSuffix::None => "no suffix".to_string(),
        DeclaredSuffix::Literal(text) => format!("'{text}'"),
        DeclaredSuffix::Auto => "AUTO".to_string(),
        DeclaredSuffix::Ambiguous(candidates) => candidates.join(", "),
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

fn evaluate(expression: &str, symbols: &Symbols) -> Truth {
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
    symbols: &'a Symbols<'a>,
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
                match self.symbols.build.compiler_version {
                    Some(version) => Value::Number(version),
                    // The build does not know it to the precision the
                    // comparison needs; deciding it would be a guess.
                    _ => Value::Truth(Truth::Unknown),
                }
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
        Value::Truth(self.symbols.truth_of(&symbol))
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
            defined: vec!["VER360".into(), "MSWINDOWS".into(), "UNICODE".into(), "WIN64".into(), "DEBUG".into()],
            compiler_version: Some(36.0),
            platform_known: true,
            version_symbols_known: true,
        }
    }

    /// Pre-XE, so the one recorded `VERxxx` is not the whole truth — the
    /// real compiler declares `VER180` next to `VER185`.
    fn delphi_2007() -> BuildSymbols {
        BuildSymbols {
            defined: vec!["VER185".into(), "MSWINDOWS".into(), "WIN32".into(), "CPUX86".into()],
            compiler_version: None,
            platform_known: true,
            version_symbols_known: false,
        }
    }

    /// Delphi 12 for a platform the builder does not know, which therefore
    /// contributes none of its symbols.
    fn unknown_platform() -> BuildSymbols {
        BuildSymbols {
            defined: vec!["VER360".into(), "UNICODE".into()],
            compiler_version: Some(36.0),
            platform_known: false,
            version_symbols_known: true,
        }
    }

    fn ambiguous(candidates: [&str; 2]) -> DeclaredSuffix {
        DeclaredSuffix::Ambiguous(candidates.iter().map(|text| format!("'{text}'")).collect())
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

    #[test]
    fn a_symbol_an_include_file_may_define_decides_nothing() {
        let source = "{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["290", "280"]));
        let source = "{$IF Defined(VEGA_D12)}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$IFEND}";
        assert_eq!(suffix(source), ambiguous(["290", "280"]));
    }

    #[test]
    fn branches_that_agree_need_no_decision() {
        let source = "{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$LIBSUFFIX '290'}{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$LIBSUFFIX 'A'}{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["A", "290"]));
    }

    #[test]
    fn a_declaration_that_may_not_apply_competes_with_no_suffix_at_all() {
        let source = "{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), DeclaredSuffix::Ambiguous(vec!["no suffix".into(), "'290'".into()]));
        // With an `{$ELSE}` a declaration is reached whatever holds.
        let source = "{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["290", "280"]));
    }

    #[test]
    fn a_suffix_possible_in_several_places_is_named_once() {
        let source = "{$LIBSUFFIX 'A'}{$IFOPT D+}{$LIBSUFFIX 'B'}{$ENDIF}{$IFOPT R+}{$LIBSUFFIX 'A'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["A", "B"]));
    }

    #[test]
    fn an_else_after_a_true_elseif_is_not_reached() {
        let source = "{$IF Defined(VEGA_D12)}{$LIBSUFFIX '290'}{$ELSEIF CompilerVersion >= 36}{$LIBSUFFIX '290'}\
                      {$ELSE}{$LIBSUFFIX '280'}{$IFEND}";
        assert_eq!(suffix(source), literal("290"));
        // The branches that are reached still decide nothing between them.
        let source = "{$IF Defined(VEGA_D12)}{$LIBSUFFIX 'V'}{$ELSEIF CompilerVersion >= 36}{$LIBSUFFIX '290'}\
                      {$ELSE}{$LIBSUFFIX '280'}{$IFEND}";
        assert_eq!(suffix(source), ambiguous(["V", "290"]));
    }

    #[test]
    fn the_platform_the_compiler_builds_for_decides_its_whole_family() {
        let source = "{$IFDEF MSWINDOWS}{$LIBSUFFIX '290'}{$ENDIF}{$IFDEF LINUX}{$LIBSUFFIX 'lin'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IFDEF POSIX}{$LIBSUFFIX 'P'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IF Defined(ANDROID) or Defined(IOS)}{$LIBSUFFIX 'M'}{$ELSE}{$LIBSUFFIX '290'}{$IFEND}";
        assert_eq!(suffix(source), literal("290"));
    }

    #[test]
    fn a_platform_the_builder_does_not_know_decides_no_platform_symbol() {
        let source = "{$IFDEF MSWINDOWS}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX 'lin'}{$ENDIF}";
        assert_eq!(declared_suffix(source, &unknown_platform()), ambiguous(["290", "lin"]));
        // What the compiler's own version settles is settled regardless.
        let source = "{$IFDEF VER350}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(declared_suffix(source, &unknown_platform()), literal("290"));
    }

    #[test]
    fn every_version_symbol_but_the_compilers_is_certainly_absent() {
        let source = "{$IFDEF VER350}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$IF Defined(VER340)}{$LIBSUFFIX '270'}{$ELSE}{$LIBSUFFIX '290'}{$IFEND}";
        assert_eq!(suffix(source), literal("290"));
        // `VER` and digits, not merely a name starting in `VER`.
        let source = "{$IFDEF VERBOSE}{$LIBSUFFIX 'A'}{$ELSE}{$LIBSUFFIX 'B'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["A", "B"]));
    }

    /// Delphi 2007 declares `VER180` next to `VER185`, and the pre-XE
    /// compiler configurations record one symbol: the family settles
    /// nothing there, however confident the single recorded value looks.
    #[test]
    fn a_version_symbol_decides_nothing_where_the_build_may_declare_more() {
        let source = "{$IFDEF VER180}{$LIBSUFFIX '110'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(declared_suffix(source, &delphi_2007()), ambiguous(["110", "290"]));
        // The one it does record still holds.
        let source = "{$IFDEF VER185}{$LIBSUFFIX '110'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(declared_suffix(source, &delphi_2007()), literal("110"));
    }

    /// A block the source never closes leaves its declaration conditional;
    /// answering with it would be the guess this reader exists to avoid.
    #[test]
    fn a_block_the_source_never_closes_decides_nothing() {
        let source = "{$LIBSUFFIX 'A'}{$IFDEF SOMETHING}{$LIBSUFFIX 'B'}";
        assert_eq!(suffix(source), ambiguous(["A", "B"]));
        // Certainly taken is still certain, open or not.
        let source = "{$LIBSUFFIX 'A'}{$IFDEF VER360}{$LIBSUFFIX 'B'}";
        assert_eq!(suffix(source), literal("B"));
    }

    /// Undecidable blocks that declare nothing must not multiply the
    /// outcomes: the set is the distinct suffixes, not two per block.
    /// Without that, 24 blocks are already 16.7 million candidates.
    #[test]
    fn blocks_that_declare_nothing_do_not_multiply_the_outcomes() {
        let mut source = String::new();
        for index in 0..24 {
            source.push_str(&format!("{{$IFDEF FEATURE_{index}}}{{$ENDIF}}"));
        }
        source.push_str("{$LIBSUFFIX '290'}");
        assert_eq!(suffix(&source), literal("290"));
    }

    /// A branch directive with nothing open means an enclosing block was
    /// opened out of sight, in an include this reader does not follow.
    #[test]
    fn a_branch_with_nothing_open_puts_the_whole_structure_in_doubt() {
        let source = "{$LIBSUFFIX 'A'}{$ELSE}{$LIBSUFFIX 'B'}";
        assert_eq!(suffix(source), ambiguous(["A", "B"]));
        let source = "{$IFDEF VER360}{$LIBSUFFIX 'A'}{$ENDIF}{$ENDIF}{$LIBSUFFIX 'B'}";
        assert_eq!(suffix(source), ambiguous(["A", "B"]));
        let source = "{$ELSEIF Defined(FOO)}{$LIBSUFFIX 'B'}";
        assert_eq!(suffix(source), DeclaredSuffix::Literal("B".into()));
    }

    /// The compiler reads one identifier and ignores the rest of the line,
    /// which the stock `.dpk` template depends on.
    #[test]
    fn a_directive_names_one_symbol_and_the_rest_is_prose() {
        let source = "{$IFDEF VER360 This IFDEF should not be used by users}{$LIBSUFFIX 'A'}{$ELSE}{$LIBSUFFIX 'B'}{$ENDIF}";
        assert_eq!(suffix(source), literal("A"));
    }

    #[test]
    fn a_symbol_the_compiler_settles_is_absent_where_the_build_lacks_it() {
        let source = "{$IFDEF UNICODE}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX 'ANSI'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        assert_eq!(declared_suffix(source, &delphi_2007()), literal("ANSI"));
    }

    #[test]
    fn the_sources_own_define_and_undef_decide_the_symbol() {
        let source = "{$DEFINE VEGA_D12}{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$DEFINE VEGA_D12}{$UNDEF VEGA_D12}{$IFDEF VEGA_D12}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX '280'}{$ENDIF}";
        assert_eq!(suffix(source), literal("280"));
    }

    #[test]
    fn a_conditional_define_cannot_take_back_what_the_build_already_has() {
        let source = "{$IFDEF VEGA}{$DEFINE UNICODE}{$ENDIF}\
                      {$IFDEF UNICODE}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX 'A'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        // Setting the other truth under the same condition does unsettle it.
        let source = "{$IFDEF VEGA}{$UNDEF UNICODE}{$ENDIF}\
                      {$IFDEF UNICODE}{$LIBSUFFIX '290'}{$ELSE}{$LIBSUFFIX 'A'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["290", "A"]));
    }

    #[test]
    fn a_define_applies_only_as_far_as_the_branch_around_it_is_taken() {
        let source = "{$UNDEF OLD}{$IFDEF VER350}{$DEFINE OLD}{$ENDIF}\
                      {$IFDEF OLD}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), literal("290"));
        let source = "{$UNDEF OLD}{$IFOPT D+}{$DEFINE OLD}{$ENDIF}\
                      {$IFDEF OLD}{$LIBSUFFIX '280'}{$ELSE}{$LIBSUFFIX '290'}{$ENDIF}";
        assert_eq!(suffix(source), ambiguous(["280", "290"]));
    }
}
