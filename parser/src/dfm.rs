//! Text-DFM parser: component tree + property values, the data source for
//! pas↔dfm links (component name ↔ form field, event property ↔ handler
//! method) and dfm-side completion.
//!
//! Handles: `object`/`inherited`/`inline` nodes with optional name and
//! `[index]`, dotted property paths (`Font.Name`), integers (incl. `$hex`,
//! negatives), floats, multi-part string literals (`'a' + #13 + 'b'`,
//! line-continued), dotted identifier values (enum members, handler names),
//! sets `[akLeft, akTop]`, binary blobs `{ hexdigits }` (skipped, recorded
//! as Binary), collections `<item ... end>`, string lists `(...)`.
//!
//! Binary DFMs (header `0xFF 'TPF0'`) are rejected with a distinct error —
//! callers may convert (`convert.exe`) or ignore; silently misparsing them
//! is not an option.

use crate::context::{Identifier, ProjectContext};

#[derive(Debug)]
pub struct DfmError {
    /// Byte offset into the DFM text.
    pub position: usize,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfmNodeKind {
    Object,
    Inherited,
    Inline,
}

#[derive(Debug)]
pub struct DfmObject {
    pub kind: DfmNodeKind,
    /// `object Button1: TButton` — None for unnamed nodes
    /// (`object TMenuItem` in some collections).
    pub name: Option<DfmName>,
    pub class_name: DfmName,
    pub properties: Vec<DfmProperty>,
    pub children: Vec<DfmObject>,
    /// Byte offset of the node's keyword (for locations).
    pub position: usize,
}

/// Dual-track interned name with its position in the DFM text.
#[derive(Debug, Clone, Copy)]
pub struct DfmName {
    pub display: Identifier,
    pub key: Identifier,
    pub position: usize,
}

#[derive(Debug)]
pub struct DfmProperty {
    /// Dotted path as written (`Font.Name`), dual-track interned.
    pub path: DfmName,
    pub value: DfmValue,
}

#[derive(Debug)]
pub enum DfmValue {
    Integer(i64),
    Float(f64),
    /// Concatenated string literal content, display-interned.
    Str(Identifier),
    /// Enum member, boolean, or a handler method name.
    Ident(DfmName),
    Set(Vec<DfmName>),
    /// `{ hex... }` blob — content deliberately not retained.
    Binary,
    /// `<item ... end, ...>` — each item is a property bag.
    Collection(Vec<Vec<DfmProperty>>),
    /// `('a' 'b' 42)` string-list style values.
    List(Vec<DfmValue>),
}

impl DfmObject {
    /// All `(property, identifier-value)` pairs — handler-candidate pass for
    /// pas↔dfm linking (whether an ident is truly an event handler is
    /// decided against the .pas side).
    pub fn identifier_properties(&self) -> impl Iterator<Item = (&DfmName, &DfmName)> {
        self.properties.iter().filter_map(|property| {
            if let DfmValue::Ident(value) = &property.value {
                Some((&property.path, value))
            } else {
                None
            }
        })
    }

    /// Depth-first traversal of the component tree, self included.
    pub fn walk(&self) -> Vec<&DfmObject> {
        let mut nodes = vec![self];
        let mut cursor = 0;
        while cursor < nodes.len() {
            let node = nodes[cursor];
            nodes.extend(node.children.iter());
            cursor += 1;
        }
        nodes
    }
}

pub fn parse_dfm(source: &str, context: &ProjectContext) -> Result<DfmObject, DfmError> {
    // Raw byte 0xFF cannot survive into &str; after ANSI decoding the
    // binary-DFM marker byte arrives as U+00FF ('ÿ').
    if source.starts_with('\u{00FF}') || source.starts_with("TPF0") {
        return Err(DfmError {
            position: 0,
            message: "binary DFM (TPF0) — convert to text form first".to_string(),
        });
    }
    let mut parser = DfmParser {
        bytes: source.as_bytes(),
        source,
        position: 0,
        context,
    };
    parser.skip_whitespace();
    let root = parser.parse_object()?;
    parser.skip_whitespace();
    if parser.position < parser.bytes.len() {
        return Err(parser.error("trailing content after root object"));
    }
    Ok(root)
}

struct DfmParser<'a> {
    bytes: &'a [u8],
    source: &'a str,
    position: usize,
    context: &'a ProjectContext,
}

impl DfmParser<'_> {
    fn error(&self, message: impl Into<String>) -> DfmError {
        DfmError {
            position: self.position,
            message: message.into(),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(&byte) = self.bytes.get(self.position) {
            if byte == b' ' || byte == b'\t' || byte == b'\r' || byte == b'\n' {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    /// True at a child-object header (`object`/`inherited`/`inline`). A
    /// property literally NAMED one of those (`Object = ...`) is disambiguated
    /// by the `=` that follows a property name — a real header is followed by
    /// the component identifier instead (L16).
    fn starts_child_object(&self) -> bool {
        for keyword in ["object", "inherited", "inline"] {
            if self.at_keyword(keyword) {
                let mut cursor = self.position + keyword.len();
                while self
                    .bytes
                    .get(cursor)
                    .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    cursor += 1;
                }
                return self.bytes.get(cursor) != Some(&b'=');
            }
        }
        false
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        let end = self.position + keyword.len();
        if end > self.bytes.len() {
            return false;
        }
        self.source[self.position..end].eq_ignore_ascii_case(keyword)
            && !self
                .bytes
                .get(end)
                .is_some_and(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if self.at_keyword(keyword) {
            self.position += keyword.len();
            self.skip_whitespace();
            true
        } else {
            false
        }
    }

    fn scan_identifier(&mut self) -> Result<&str, DfmError> {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.position += 1;
        }
        if start == self.position {
            return Err(self.error("expected identifier"));
        }
        Ok(&self.source[start..self.position])
    }

    /// Dotted identifier (`Font.Name`, `System.Classes.TAlign` values).
    fn scan_dotted_name(&mut self) -> Result<DfmName, DfmError> {
        let position = self.position;
        let start = self.position;
        self.scan_identifier()?;
        while self.peek() == Some(b'.') {
            self.position += 1;
            self.scan_identifier()?;
        }
        let text = &self.source[start..self.position];
        let name = DfmName {
            display: self.context.intern(text),
            key: self.context.intern_key(text),
            position,
        };
        self.skip_whitespace();
        Ok(name)
    }

    fn parse_object(&mut self) -> Result<DfmObject, DfmError> {
        let position = self.position;
        let kind = if self.consume_keyword("object") {
            DfmNodeKind::Object
        } else if self.consume_keyword("inherited") {
            DfmNodeKind::Inherited
        } else if self.consume_keyword("inline") {
            DfmNodeKind::Inline
        } else {
            return Err(self.error("expected 'object', 'inherited' or 'inline'"));
        };

        let first = self.scan_dotted_name()?;
        let (name, class_name) = if self.peek() == Some(b':') {
            self.position += 1;
            self.skip_whitespace();
            (Some(first), self.scan_dotted_name()?)
        } else {
            (None, first) // `object TMenuItem` — class only
        };

        // optional creation-order index `[0]`
        if self.peek() == Some(b'[') {
            while self.peek().is_some_and(|byte| byte != b']') {
                self.position += 1;
            }
            self.position += 1;
            self.skip_whitespace();
        }

        let mut properties = Vec::new();
        let mut children = Vec::new();
        loop {
            if self.consume_keyword("end") {
                break;
            }
            if self.starts_child_object() {
                children.push(self.parse_object()?);
                continue;
            }
            if self.peek().is_none() {
                return Err(self.error("unterminated object (missing 'end')"));
            }
            let path = self.scan_dotted_name()?;
            if self.peek() != Some(b'=') {
                return Err(self.error("expected '=' after property name"));
            }
            self.position += 1;
            self.skip_whitespace();
            let value = self.parse_value()?;
            properties.push(DfmProperty { path, value });
        }

        Ok(DfmObject {
            kind,
            name,
            class_name,
            properties,
            children,
            position,
        })
    }

    fn parse_value(&mut self) -> Result<DfmValue, DfmError> {
        match self.peek() {
            Some(b'\'') | Some(b'#') => self.parse_string_value(),
            Some(b'[') => self.parse_set(),
            Some(b'{') => self.parse_binary(),
            Some(b'<') => self.parse_collection(),
            Some(b'(') => self.parse_list(),
            Some(byte) if byte == b'-' || byte == b'+' || byte == b'$' || byte.is_ascii_digit() => {
                self.parse_number()
            }
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'_' => {
                Ok(DfmValue::Ident(self.scan_dotted_name()?))
            }
            _ => Err(self.error("expected property value")),
        }
    }

    /// `'a''b' + #13#10 'next line'` — parts concatenate; `+` and line
    /// breaks between parts are both legal.
    fn parse_string_value(&mut self) -> Result<DfmValue, DfmError> {
        let mut content = String::new();
        loop {
            match self.peek() {
                Some(b'\'') => {
                    self.position += 1;
                    loop {
                        match self.peek() {
                            None => return Err(self.error("unterminated string")),
                            Some(b'\'') => {
                                self.position += 1;
                                if self.peek() == Some(b'\'') {
                                    content.push('\'');
                                    self.position += 1;
                                } else {
                                    break;
                                }
                            }
                            Some(_) => {
                                let character = self.source[self.position..]
                                    .chars()
                                    .next()
                                    .expect("in-bounds char");
                                content.push(character);
                                self.position += character.len_utf8();
                            }
                        }
                    }
                }
                Some(b'#') => {
                    let code = self.read_char_code()?;
                    // Non-BMP characters (emoji, some CJK) are written as a
                    // UTF-16 surrogate PAIR of `#$` codes (`#$D83D#$DE00`).
                    // `char::from_u32` rejects a lone surrogate, so combine the
                    // pair into the real scalar before decoding.
                    let scalar = if (0xD800..=0xDBFF).contains(&code) && self.peek() == Some(b'#') {
                        let low = self.read_char_code()?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return Err(self.error("unpaired UTF-16 surrogate in character code"));
                        }
                        0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                    } else {
                        code
                    };
                    content.push(
                        char::from_u32(scalar)
                            .ok_or_else(|| self.error("invalid character code"))?,
                    );
                }
                _ => {
                    // Concatenation continues only (a) immediately (`'a'#13`)
                    // or (b) after a `+` (line-wrapped long strings). A bare
                    // newline before the next quote is NOT concatenation —
                    // in `(...)` lists that starts the next item.
                    let checkpoint = self.position;
                    while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
                        self.position += 1;
                    }
                    if self.peek() == Some(b'+') {
                        self.position += 1;
                        self.skip_whitespace();
                        if matches!(self.peek(), Some(b'\'') | Some(b'#')) {
                            continue;
                        }
                    } else if matches!(self.peek(), Some(b'\'') | Some(b'#')) {
                        continue;
                    }
                    self.position = checkpoint;
                    break;
                }
            }
        }
        self.skip_whitespace();
        Ok(DfmValue::Str(self.context.intern(&content)))
    }

    /// Read one `#NN` / `#$HH` character code, consuming the leading `#`.
    /// Caller must have ensured `peek() == Some(b'#')`.
    fn read_char_code(&mut self) -> Result<u32, DfmError> {
        self.position += 1; // '#'
        if self.peek() == Some(b'$') {
            self.position += 1;
            let digits = self.scan_while(|byte| byte.is_ascii_hexdigit())?;
            u32::from_str_radix(digits, 16)
        } else {
            let digits = self.scan_while(|byte| byte.is_ascii_digit())?;
            digits.parse()
        }
        .map_err(|_| self.error("invalid character code"))
    }

    fn scan_while(&mut self, predicate: impl Fn(u8) -> bool) -> Result<&str, DfmError> {
        let start = self.position;
        while self.peek().is_some_and(&predicate) {
            self.position += 1;
        }
        if start == self.position {
            return Err(self.error("expected digits"));
        }
        Ok(&self.source[start..self.position])
    }

    fn parse_number(&mut self) -> Result<DfmValue, DfmError> {
        let start = self.position;
        if matches!(self.peek(), Some(b'-') | Some(b'+')) {
            self.position += 1;
        }
        if self.peek() == Some(b'$') {
            self.position += 1;
            let negative = self.bytes[start] == b'-';
            let digits = self.scan_while(|byte| byte.is_ascii_hexdigit())?.to_string();
            self.skip_whitespace();
            let value =
                i64::from_str_radix(&digits, 16).map_err(|_| self.error("invalid hex number"))?;
            return Ok(DfmValue::Integer(if negative { -value } else { value }));
        }
        self.scan_while(|byte| byte.is_ascii_digit())?;
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.position += 1;
            self.scan_while(|byte| byte.is_ascii_digit())?;
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.position += 1;
            if matches!(self.peek(), Some(b'-') | Some(b'+')) {
                self.position += 1;
            }
            self.scan_while(|byte| byte.is_ascii_digit())?;
        }
        let text = &self.source[start..self.position];
        self.skip_whitespace();
        if is_float {
            Ok(DfmValue::Float(
                text.parse().map_err(|_| self.error("invalid float"))?,
            ))
        } else {
            Ok(DfmValue::Integer(
                text.parse().map_err(|_| self.error("invalid integer"))?,
            ))
        }
    }

    fn parse_set(&mut self) -> Result<DfmValue, DfmError> {
        self.position += 1; // '['
        self.skip_whitespace();
        let mut members = Vec::new();
        if self.peek() != Some(b']') {
            loop {
                members.push(self.scan_dotted_name()?);
                match self.peek() {
                    Some(b',') => {
                        self.position += 1;
                        self.skip_whitespace();
                    }
                    Some(b']') => break,
                    _ => return Err(self.error("expected ',' or ']' in set")),
                }
            }
        }
        self.position += 1; // ']'
        self.skip_whitespace();
        Ok(DfmValue::Set(members))
    }

    fn parse_binary(&mut self) -> Result<DfmValue, DfmError> {
        self.position += 1; // '{'
        while let Some(byte) = self.peek() {
            self.position += 1;
            if byte == b'}' {
                self.skip_whitespace();
                return Ok(DfmValue::Binary);
            }
        }
        Err(self.error("unterminated binary blob"))
    }

    fn parse_collection(&mut self) -> Result<DfmValue, DfmError> {
        self.position += 1; // '<'
        self.skip_whitespace();
        let mut items = Vec::new();
        loop {
            if self.peek() == Some(b'>') {
                self.position += 1;
                self.skip_whitespace();
                break;
            }
            if !self.consume_keyword("item") {
                return Err(self.error("expected 'item' or '>' in collection"));
            }
            let mut properties = Vec::new();
            while !self.consume_keyword("end") {
                if self.peek().is_none() {
                    return Err(self.error("unterminated collection item"));
                }
                let path = self.scan_dotted_name()?;
                if self.peek() != Some(b'=') {
                    return Err(self.error("expected '=' in collection item"));
                }
                self.position += 1;
                self.skip_whitespace();
                let value = self.parse_value()?;
                properties.push(DfmProperty { path, value });
            }
            items.push(properties);
        }
        Ok(DfmValue::Collection(items))
    }

    fn parse_list(&mut self) -> Result<DfmValue, DfmError> {
        self.position += 1; // '('
        self.skip_whitespace();
        let mut values = Vec::new();
        while self.peek() != Some(b')') {
            if self.peek().is_none() {
                return Err(self.error("unterminated value list"));
            }
            values.push(self.parse_value()?);
        }
        self.position += 1; // ')'
        self.skip_whitespace();
        Ok(DfmValue::List(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;

    fn test_context() -> ProjectContext {
        ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: Vec::new(),
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        }
    }

    fn resolve(_context: &ProjectContext, name: Identifier) -> String {
        crate::globals::resolve(name).to_string()
    }

    const FORM_DFM: &str = "object MainForm: TMainForm\n\
        \x20 Left = 0\n\
        \x20 Caption = 'Haupt'#13#10'fenster' + 'X'\n\
        \x20 ClientHeight = -12\n\
        \x20 Color = clBtnFace\n\
        \x20 Font.Height = $F5\n\
        \x20 Anchors = [akLeft, akTop]\n\
        \x20 Scale = 1.25\n\
        \x20 OnCreate = FormCreate\n\
        \x20 Icon.Data = {\n\
        \x20   FF00AB}\n\
        \x20 Columns = <\n\
        \x20   item\n\
        \x20     Width = 50\n\
        \x20   end\n\
        \x20   item\n\
        \x20     Width = 75\n\
        \x20   end>\n\
        \x20 Strings.Strings = (\n\
        \x20   'one'\n\
        \x20   'two')\n\
        \x20 object OkButton: TButton\n\
        \x20   OnClick = OkButtonClick\n\
        \x20 end\n\
        \x20 inherited Frame: TSharedFrame\n\
        \x20 end\n\
        end\n";

    #[test]
    fn parses_component_tree_and_values() {
        let context = test_context();
        let root = parse_dfm(FORM_DFM, &context).unwrap();

        assert_eq!(root.kind, DfmNodeKind::Object);
        assert_eq!(resolve(&context, root.name.unwrap().display), "MainForm");
        assert_eq!(resolve(&context, root.class_name.display), "TMainForm");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].kind, DfmNodeKind::Inherited);

        let property = |name: &str| {
            let key = context.intern_key(name);
            root.properties
                .iter()
                .find(|property| property.path.key == key)
                .unwrap_or_else(|| panic!("property {name}"))
        };
        assert!(matches!(property("Left").value, DfmValue::Integer(0)));
        assert!(matches!(
            property("ClientHeight").value,
            DfmValue::Integer(-12)
        ));
        assert!(matches!(
            property("Font.Height").value,
            DfmValue::Integer(0xF5)
        ));
        assert!(matches!(property("Scale").value, DfmValue::Float(v) if v == 1.25));
        assert!(matches!(property("Icon.Data").value, DfmValue::Binary));

        let DfmValue::Str(caption) = property("Caption").value else {
            panic!("caption");
        };
        assert_eq!(resolve(&context, caption), "Haupt\r\nfensterX");

        let DfmValue::Set(anchors) = &property("Anchors").value else {
            panic!("anchors");
        };
        assert_eq!(anchors.len(), 2);

        let DfmValue::Collection(items) = &property("Columns").value else {
            panic!("columns");
        };
        assert_eq!(items.len(), 2);

        let DfmValue::List(strings) = &property("Strings.Strings").value else {
            panic!("strings");
        };
        assert_eq!(strings.len(), 2);
    }

    #[test]
    fn property_named_like_a_structural_keyword_is_not_a_child() {
        // L16: a property literally named `Object` (followed by `=`) must be a
        // property, not misread as a nested object header.
        let context = test_context();
        let dfm = "object Form1: TForm1\n  Object = 5\n  Inline = 7\nend\n";
        let root = parse_dfm(dfm, &context).unwrap();
        assert_eq!(root.children.len(), 0);
        assert_eq!(root.properties.len(), 2);
        assert!(matches!(
            root.properties
                .iter()
                .find(|p| p.path.key == context.intern_key("Object"))
                .unwrap()
                .value,
            DfmValue::Integer(5)
        ));
    }

    #[test]
    fn string_property_decodes_surrogate_pair_char_codes() {
        // M12: a non-BMP character (emoji U+1F600) is written as a UTF-16
        // surrogate pair of `#$` codes — both halves must combine into one
        // scalar rather than failing on the lone high surrogate.
        let context = test_context();
        let dfm = "object Form1: TForm1\n  Caption = #$D83D#$DE00\nend\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let caption = root
            .properties
            .iter()
            .find(|property| property.path.key == context.intern_key("Caption"))
            .expect("caption");
        let DfmValue::Str(text) = caption.value else {
            panic!("caption not a string");
        };
        assert_eq!(resolve(&context, text), "\u{1F600}");
    }

    #[test]
    fn handler_candidates_for_pas_links() {
        let context = test_context();
        let root = parse_dfm(FORM_DFM, &context).unwrap();

        let mut handlers = Vec::new();
        for node in root.walk() {
            for (property, value) in node.identifier_properties() {
                handlers.push((
                    resolve(&context, property.display),
                    resolve(&context, value.display),
                ));
            }
        }
        assert!(handlers.contains(&("OnCreate".to_string(), "FormCreate".to_string())));
        assert!(handlers.contains(&("OnClick".to_string(), "OkButtonClick".to_string())));
        // enum value Color=clBtnFace also appears — filtering against the
        // .pas side (is it a method?) is the linker's job
        assert!(handlers.contains(&("Color".to_string(), "clBtnFace".to_string())));
    }

    #[test]
    fn binary_dfm_rejected_distinctly() {
        let context = test_context();
        let error = parse_dfm("\u{FF}TPF0...", &context).unwrap_err();
        assert!(error.message.contains("binary DFM"));
    }

    #[test]
    fn class_only_nodes_and_errors() {
        let context = test_context();
        let root = parse_dfm(
            "object TMenuItem\n  Caption = 'x'\nend\n",
            &context,
        )
        .unwrap();
        assert!(root.name.is_none());
        assert_eq!(resolve(&context, root.class_name.display), "TMenuItem");

        assert!(parse_dfm("object A: TB\n  Left 0\nend", &context).is_err());
        assert!(parse_dfm("object A: TB\n", &context).is_err());
    }
}
