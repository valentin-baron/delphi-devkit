//! Case-insensitive `$(NAME)` variable expansion.
//!
//! Lives with the compiler code because every source it aggregates is tied to
//! a concrete Delphi installation: `rsvars.bat` variables, the IDE's registry
//! environment-variable overrides, and the per-build `Config`/`Platform`
//! context — unlike `dproj-rs`, which evaluates a `.dproj` in isolation.

use std::collections::HashMap;

/// A case-insensitive `$(NAME)` variable map (Windows environment semantics).
///
/// Names are also kept in their original spelling because `dproj-rs` resolves
/// `$(NAME)` through a **case-sensitive** map: seeding it needs the exact
/// casing the `.dproj` files use (`DCC_UnitSearchPath`, not `DCC_UNITSEARCHPATH`).
#[derive(Debug, Clone, Default)]
pub struct MacroMap {
    /// Upper-cased keys — the lookup used by [`MacroMap::expand`].
    vars: HashMap<String, String>,
    /// The same entries under their original spelling.
    original_case: HashMap<String, String>,
}

impl MacroMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a variable, overwriting any previous value.
    pub fn set(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        let key = key.as_ref();
        let value = value.into();
        self.vars.insert(key.to_ascii_uppercase(), value.clone());
        self.original_case.insert(key.to_string(), value);
    }

    /// Insert a variable only when that name is not already defined.
    pub fn set_default(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        if self.get(key.as_ref()).is_none() {
            self.set(key, value);
        }
    }

    /// Forget a variable, whatever casing it was defined with.
    pub fn remove(&mut self, key: &str) {
        let upper = key.to_ascii_uppercase();
        self.vars.remove(&upper);
        self.original_case.retain(|k, _| k.to_ascii_uppercase() != upper);
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.vars.get(&key.to_ascii_uppercase())
    }

    pub fn extend<I, K, V>(&mut self, entries: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        for (k, v) in entries {
            self.set(k, v);
        }
    }

    /// Expand every `$(NAME)` reference. Unknown names are left **verbatim**
    /// so callers can detect (and report) unresolved macros — this is the one
    /// behavioural difference from MSBuild, which expands them to nothing.
    ///
    /// Expansion is iterative (a resolved value may itself contain macros) and
    /// bounded so a self-referential definition cannot loop forever.
    pub fn expand(&self, value: &str) -> String {
        const MAX_PASSES: usize = 8;
        let mut current = value.to_string();
        for _ in 0..MAX_PASSES {
            if !current.contains("$(") {
                break;
            }
            let next = self.expand_once(&current);
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    fn expand_once(&self, value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let bytes: Vec<char> = value.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '$' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
                if let Some(close) = (i + 2..bytes.len()).find(|&j| bytes[j] == ')') {
                    let name: String = bytes[i + 2..close].iter().collect();
                    match self.get(&name) {
                        Some(resolved) => out.push_str(resolved),
                        // Unknown: keep the token so the caller can warn.
                        _ => out.push_str(&format!("$({name})")),
                    }
                    i = close + 1;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    /// Seed environment for `dproj-rs` property-group evaluation. Every entry
    /// appears both upper-cased and in its original spelling, because that
    /// lookup is case-sensitive.
    pub fn as_env(&self) -> HashMap<String, String> {
        let mut env = self.vars.clone();
        env.extend(self.original_case.iter().map(|(k, v)| (k.clone(), v.clone())));
        env
    }
}
