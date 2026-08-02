//! Delphi / Object Pascal lexical tokens (`logos`-generated).
//!
//! Design notes:
//! * **Case-insensitive** keywords via `ignore(case)`.
//! * **Payload-free.** Tokens carry no borrowed text and no lifetime — a token is
//!   just its kind. Its text is recovered on demand from `(FileId, Span)` via the
//!   source arena. This is what lets the preprocessor splice `{$I}` includes
//!   across multiple files without self-referential borrows.
//! * **Directives are opaque tokens.** `{$…}` / `(*$…*)` are [`Token::DirectiveBrace`]
//!   / [`Token::DirectiveParen`]; their inner text is taken from the source slice
//!   ([`directive_inner_text`]). The lexer knows nothing about `IF`/`ENDIF` nesting.
//! * **Trivia preserved** (whitespace, newlines, comments) for a lossless CST.

use logos::Logos;

/// A single lexical token kind. `Copy` and lifetime-free — text comes from the
/// source via the token's span.
#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[logos(error = LexError)]
pub enum Token {
    // -- Trivia -------------------------------------------------------------
    // `\u{FEFF}` (BOM / zero-width no-break space) is skipped as whitespace: the
    // disk-read path strips a leading BOM (`source::decode`), but an EDITOR
    // BUFFER arrives already-decoded and may still carry it (VS Code can send the
    // BOM in the document text). Treating it as trivia — rather than a hard lex
    // error that fails the WHOLE unit — keeps byte offsets intact (the server's
    // LineIndex is built from the same buffer text), so a BOM-prefixed buffer
    // parses identically to a stripped one. Harmless anywhere (zero-width).
    #[regex(r"[ \t\u{FEFF}]+")]
    Whitespace,
    #[regex(r"\r\n|\r|\n")]
    Newline,

    // -- Comments (NOT directives — `$` as first inner char is excluded) -----
    // First inner char excludes `$` (→ directive) and `}` (else `{}` would greedily
    // consume its own closing brace and run to the next `}` anywhere in the file).
    #[regex(r"\{[^$}][^}]*\}", priority = 1)]
    #[regex(r"\{\}", priority = 1)]
    BlockComment,
    // Body allows `*` runs that aren't immediately before `)`; closes on `*+)`.
    // Handles `(** text **)`, `(***…***)` and all-star banners. First inner char
    // is `[^$]` so `(*$…*)` falls through to the directive token below.
    #[regex(r"\(\*[^$]([^*]|\*+[^*)])*\*+\)", priority = 1)]
    #[regex(r"\(\*\*\)", priority = 1)]
    BlockCommentParen,
    #[regex(r"//[^\r\n]*", allow_greedy = true)]
    LineComment,

    // -- Compiler directives (matched BEFORE generic comments) ---------------
    #[regex(r"\{\$[^}]*\}")]
    DirectiveBrace,
    #[regex(r"\(\*\$([^*]|\*[^)])*\*\)")]
    DirectiveParen,

    // -- Literals -----------------------------------------------------------
    #[regex(r"[0-9]+", priority = 2)]
    #[regex(r"\$[0-9A-Fa-f]+", priority = 2)]
    #[regex(r"%[01]+", priority = 2)]
    #[regex(r"&[0-7]+", priority = 2)]
    IntLiteral,
    #[regex(r"[0-9]+\.[0-9]+([eE][+\-]?[0-9]+)?", priority = 3)]
    #[regex(r"[0-9]+[eE][+\-]?[0-9]+", priority = 3)]
    FloatLiteral,
    #[regex(r"'([^']|'')*'")]
    StringLiteral,
    #[regex(r"#[0-9]+")]
    #[regex(r"#\$[0-9A-Fa-f]+")]
    CharLiteral,

    // -- Reserved words (cannot be identifiers) ------------------------------
    #[token("and", ignore(case))] And,
    #[token("array", ignore(case))] Array,
    #[token("as", ignore(case))] As,
    #[token("asm", ignore(case))] Asm,
    #[token("begin", ignore(case))] Begin,
    #[token("case", ignore(case))] Case,
    #[token("class", ignore(case))] Class,
    #[token("const", ignore(case))] Const,
    #[token("constructor", ignore(case))] Constructor,
    #[token("destructor", ignore(case))] Destructor,
    #[token("dispinterface", ignore(case))] DispInterface,
    #[token("div", ignore(case))] Div,
    #[token("do", ignore(case))] Do,
    #[token("downto", ignore(case))] DownTo,
    #[token("else", ignore(case))] Else,
    #[token("end", ignore(case))] End,
    #[token("except", ignore(case))] Except,
    #[token("exports", ignore(case))] Exports,
    #[token("file", ignore(case))] File,
    #[token("finalization", ignore(case))] Finalization,
    #[token("finally", ignore(case))] Finally,
    #[token("for", ignore(case))] For,
    #[token("function", ignore(case))] Function,
    #[token("goto", ignore(case))] Goto,
    #[token("if", ignore(case))] If,
    #[token("implementation", ignore(case))] Implementation,
    #[token("in", ignore(case))] In,
    #[token("inherited", ignore(case))] Inherited,
    #[token("initialization", ignore(case))] Initialization,
    #[token("inline", ignore(case))] Inline,
    #[token("interface", ignore(case))] Interface,
    #[token("is", ignore(case))] Is,
    #[token("label", ignore(case))] Label,
    #[token("library", ignore(case))] Library,
    #[token("mod", ignore(case))] Mod,
    #[token("nil", ignore(case))] Nil,
    #[token("not", ignore(case))] Not,
    #[token("object", ignore(case))] Object,
    #[token("of", ignore(case))] Of,
    #[token("on", ignore(case))] On,
    #[token("operator", ignore(case))] Operator,
    #[token("or", ignore(case))] Or,
    #[token("out", ignore(case))] Out,
    #[token("packed", ignore(case))] Packed,
    #[token("procedure", ignore(case))] Procedure,
    #[token("program", ignore(case))] Program,
    #[token("property", ignore(case))] Property,
    #[token("raise", ignore(case))] Raise,
    #[token("record", ignore(case))] Record,
    #[token("repeat", ignore(case))] Repeat,
    #[token("resourcestring", ignore(case))] ResourceString,
    #[token("set", ignore(case))] Set,
    #[token("shl", ignore(case))] Shl,
    #[token("shr", ignore(case))] Shr,
    #[token("string", ignore(case))] String,
    #[token("then", ignore(case))] Then,
    #[token("threadvar", ignore(case))] ThreadVar,
    #[token("to", ignore(case))] To,
    #[token("try", ignore(case))] Try,
    #[token("type", ignore(case))] Type,
    #[token("unit", ignore(case))] Unit,
    #[token("until", ignore(case))] Until,
    #[token("uses", ignore(case))] Uses,
    #[token("var", ignore(case))] Var,
    #[token("while", ignore(case))] While,
    #[token("with", ignore(case))] With,
    #[token("xor", ignore(case))] Xor,

    // -- Directive / modifier keywords (context-sensitive; valid as idents) --
    #[token("absolute", ignore(case))] Absolute,
    #[token("abstract", ignore(case))] Abstract,
    #[token("assembler", ignore(case))] Assembler,
    #[token("at", ignore(case))] At,
    #[token("automated", ignore(case))] Automated,
    #[token("cdecl", ignore(case))] CDecl,
    #[token("contains", ignore(case))] Contains,
    #[token("default", ignore(case))] Default,
    #[token("delayed", ignore(case))] Delayed,
    #[token("deprecated", ignore(case))] Deprecated,
    #[token("dispid", ignore(case))] DispId,
    #[token("dynamic", ignore(case))] Dynamic,
    #[token("experimental", ignore(case))] Experimental,
    #[token("export", ignore(case))] Export,
    #[token("external", ignore(case))] External,
    #[token("far", ignore(case))] Far,
    #[token("final", ignore(case))] Final,
    #[token("forward", ignore(case))] Forward,
    #[token("helper", ignore(case))] Helper,
    #[token("implements", ignore(case))] Implements,
    #[token("index", ignore(case))] Index,
    #[token("local", ignore(case))] Local,
    #[token("message", ignore(case))] Message,
    #[token("name", ignore(case))] Name,
    #[token("near", ignore(case))] Near,
    #[token("nodefault", ignore(case))] NoDefault,
    #[token("overload", ignore(case))] Overload,
    #[token("override", ignore(case))] Override,
    #[token("package", ignore(case))] Package,
    #[token("pascal", ignore(case))] Pascal,
    #[token("platform", ignore(case))] Platform,
    #[token("private", ignore(case))] Private,
    #[token("protected", ignore(case))] Protected,
    #[token("public", ignore(case))] Public,
    #[token("published", ignore(case))] Published,
    #[token("read", ignore(case))] Read,
    #[token("readonly", ignore(case))] ReadOnly,
    #[token("reference", ignore(case))] Reference,
    #[token("reintroduce", ignore(case))] Reintroduce,
    #[token("requires", ignore(case))] Requires,
    #[token("resident", ignore(case))] Resident,
    #[token("safecall", ignore(case))] SafeCall,
    #[token("sealed", ignore(case))] Sealed,
    #[token("static", ignore(case))] Static,
    #[token("stdcall", ignore(case))] StdCall,
    #[token("stored", ignore(case))] Stored,
    #[token("strict", ignore(case))] Strict,
    #[token("unsafe", ignore(case))] Unsafe,
    #[token("varargs", ignore(case))] VarArgs,
    #[token("virtual", ignore(case))] Virtual,
    #[token("winapi", ignore(case))] WinApi,
    #[token("write", ignore(case))] Write,
    #[token("writeonly", ignore(case))] WriteOnly,

    // -- Predefined type identifiers (shadowable; fall back to Ident) ---------
    #[token("boolean", ignore(case))] Boolean,
    #[token("byte", ignore(case))] Byte,
    #[token("bytebool", ignore(case))] ByteBool,
    #[token("cardinal", ignore(case))] Cardinal,
    #[token("char", ignore(case))] Char,
    #[token("comp", ignore(case))] Comp,
    #[token("currency", ignore(case))] Currency,
    #[token("double", ignore(case))] Double,
    #[token("extended", ignore(case))] Extended,
    #[token("int8", ignore(case))] Int8,
    #[token("int16", ignore(case))] Int16,
    #[token("int32", ignore(case))] Int32,
    #[token("int64", ignore(case))] Int64,
    #[token("integer", ignore(case))] Integer,
    #[token("longbool", ignore(case))] LongBool,
    #[token("longint", ignore(case))] LongInt,
    #[token("longword", ignore(case))] LongWord,
    #[token("nativeint", ignore(case))] NativeInt,
    #[token("nativeuint", ignore(case))] NativeUInt,
    #[token("pansichar", ignore(case))] PAnsiChar,
    #[token("pchar", ignore(case))] PChar,
    #[token("pointer", ignore(case))] Pointer,
    #[token("pwidechar", ignore(case))] PWideChar,
    #[token("real", ignore(case))] Real,
    #[token("real48", ignore(case))] Real48,
    #[token("shortint", ignore(case))] ShortInt,
    #[token("shortstring", ignore(case))] ShortString,
    #[token("single", ignore(case))] Single,
    #[token("smallint", ignore(case))] SmallInt,
    #[token("text", ignore(case))] Text,
    #[token("uint8", ignore(case))] UInt8,
    #[token("uint16", ignore(case))] UInt16,
    #[token("uint32", ignore(case))] UInt32,
    #[token("uint64", ignore(case))] UInt64,
    #[token("word", ignore(case))] Word,
    #[token("wordbool", ignore(case))] WordBool,
    #[token("ansichar", ignore(case))] AnsiChar,
    #[token("ansistring", ignore(case))] AnsiString,
    #[token("rawbytestring", ignore(case))] RawByteString,
    #[token("unicodestring", ignore(case))] UnicodeString,
    #[token("utf8string", ignore(case))] Utf8String,
    #[token("widechar", ignore(case))] WideChar,
    #[token("widestring", ignore(case))] WideString,

    // -- Special predefined identifiers --------------------------------------
    #[token("true", ignore(case))] True,
    #[token("false", ignore(case))] False,
    #[token("result", ignore(case))] Result,
    #[token("self", ignore(case))] Self_,

    // -- Operators & punctuation --------------------------------------------
    #[token("+")] Plus,
    #[token("-")] Minus,
    #[token("*")] Star,
    #[token("/")] Slash,
    #[token("=")] Eq,
    #[token("<>")] NEq,
    #[token("<")] Lt,
    #[token(">")] Gt,
    #[token("<=")] LtEq,
    #[token(">=")] GtEq,
    #[token(":=")] Assign,
    #[token(":")] Colon,
    #[token(";")] Semicolon,
    #[token(",")] Comma,
    #[token("..")] DotDot,
    #[token(".")] Dot,
    #[token("^")] Caret,
    #[token("@")] At_,
    #[token("(")] LParen,
    #[token(")")] RParen,
    #[token("[")] LBracket,
    #[token("]")] RBracket,

    // -- Identifier (lowest priority) ----------------------------------------
    /// Also matches `&`-escaped identifiers (`&begin`, `&string`) used to use a
    /// reserved word as an identifier.
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*", priority = 0)]
    #[regex(r"&[A-Za-z_][A-Za-z0-9_]*", priority = 0)]
    Ident,

    /// Emitted by the lexer wrapper for any byte sequence that matches no rule.
    Error,
}

impl Token {
    /// `true` for whitespace, newlines and comments.
    pub fn is_trivia(&self) -> bool {
        matches!(
            self,
            Token::Whitespace | Token::Newline | Token::BlockComment
                | Token::BlockCommentParen | Token::LineComment
        )
    }

    /// `true` if this token is a compiler directive (`{$…}` / `(*$…*)`).
    pub fn is_directive(&self) -> bool {
        matches!(self, Token::DirectiveBrace | Token::DirectiveParen)
    }

    /// `true` if this token may be used as an identifier (e.g. a unit-name part):
    /// `Ident` plus the context-sensitive keyword/type groups, but NOT the true
    /// reserved words. Delphi unit names like `Winapi.Windows` rely on this
    /// (`Winapi` is the `winapi` calling-convention keyword).
    pub fn can_be_identifier(&self) -> bool {
        use Token::*;
        matches!(
            self,
            Ident
            | Absolute | Abstract | Assembler | At | Automated | CDecl | Contains
            | Default | Delayed | Deprecated | DispId | Dynamic | Experimental
            | Export | External | Far | Final | Forward | Helper | Implements
            | Index | Local | Message | Name | Near | NoDefault | Overload
            | Override | Package | Pascal | Platform | Private | Protected | Public
            | Published | Read | ReadOnly | Reference | Reintroduce | Requires
            | Resident | SafeCall | Sealed | Static | StdCall | Stored | Strict
            | Unsafe | VarArgs | Virtual | WinApi | Write | WriteOnly | Operator | Out
            | Boolean | Byte | ByteBool | Cardinal | Char | Comp | Currency | Double
            | Extended | Int8 | Int16 | Int32 | Int64 | Integer | LongBool | LongInt
            | LongWord | NativeInt | NativeUInt | PAnsiChar | PChar | Pointer
            | PWideChar | Real | Real48 | ShortInt | ShortString | Single | SmallInt
            | Text | UInt8 | UInt16 | UInt32 | UInt64 | Word | WordBool | AnsiChar
            | AnsiString | RawByteString | UnicodeString | Utf8String | WideChar
            | WideString
            | True | False | Result | Self_
        )
    }
}

/// Given the full source slice of a directive token (`{$…}` or `(*$…*)`), return
/// its inner text (between the opening `$` form and the closing delimiter).
pub fn directive_inner_text(slice: &str) -> &str {
    if let Some(inner) = slice.strip_prefix("{$").and_then(|s| s.strip_suffix('}')) {
        inner
    } else if let Some(inner) = slice.strip_prefix("(*$").and_then(|s| s.strip_suffix("*)")) {
        inner
    } else {
        slice
    }
}

/// Error type for unrecognised input. The lexer turns this into a spanned
/// [`Token::Error`] rather than dropping bytes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LexError;

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unrecognised token")
    }
}

/// Severity level for `{$MESSAGE …}` directives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Hint,
    Warn,
    Error,
    Fatal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Vec<Token> {
        Token::lexer(src).map(|r| r.unwrap_or(Token::Error)).collect()
    }

    #[test]
    fn reserved_words_case_insensitive() {
        for src in &["begin", "BEGIN", "Begin", "bEgIn"] {
            assert_eq!(lex(src), vec![Token::Begin], "failed for `{src}`");
        }
    }

    #[test]
    fn identifier_not_confused_with_keyword() {
        assert_eq!(lex("MyVar"), vec![Token::Ident]);
        assert_eq!(lex("begin2"), vec![Token::Ident]);
        assert_eq!(lex("_result"), vec![Token::Ident]);
    }

    #[test]
    fn operators() {
        assert_eq!(lex(":="), vec![Token::Assign]);
        assert_eq!(lex("<>"), vec![Token::NEq]);
        assert_eq!(lex(".."), vec![Token::DotDot]);
        assert_eq!(lex("<="), vec![Token::LtEq]);
        assert_eq!(lex(">="), vec![Token::GtEq]);
    }

    #[test]
    fn literals() {
        assert_eq!(lex("42"), vec![Token::IntLiteral]);
        assert_eq!(lex("$FF"), vec![Token::IntLiteral]);
        assert_eq!(lex("%1010"), vec![Token::IntLiteral]);
        assert_eq!(lex("3.14"), vec![Token::FloatLiteral]);
        assert_eq!(lex("'it''s'"), vec![Token::StringLiteral]);
        assert_eq!(lex("#65"), vec![Token::CharLiteral]);
    }

    #[test]
    fn comments_vs_directives() {
        assert_eq!(lex("{ a comment }"), vec![Token::BlockComment]);
        assert_eq!(lex("(* a comment *)"), vec![Token::BlockCommentParen]);
        assert_eq!(lex("// line"), vec![Token::LineComment]);
        // A space after `{` makes it a comment, NOT a directive.
        assert_eq!(lex("{ $DEFINE FOO }"), vec![Token::BlockComment]);
    }

    #[test]
    fn directive_inner_text_extracts_body() {
        let mut l = Token::lexer("{$IFDEF DEBUG}");
        assert_eq!(l.next(), Some(Ok(Token::DirectiveBrace)));
        assert_eq!(directive_inner_text(l.slice()), "IFDEF DEBUG");

        let mut l = Token::lexer("(*$IFDEF WIN32*)");
        assert_eq!(l.next(), Some(Ok(Token::DirectiveParen)));
        assert_eq!(directive_inner_text(l.slice()), "IFDEF WIN32");
    }
}
