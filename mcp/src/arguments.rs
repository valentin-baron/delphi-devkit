//! Reading a tool call's arguments without ever dropping one silently.
//!
//! A client that sends `"project_id": "7"` means it; `as_u64()` with a default
//! fallback would silently act on the active project instead. Every accessor
//! here either understands the value — including the obvious string spellings
//! of a number or a boolean — or fails with a message naming the argument.

use serde_json::Value;

pub struct Arguments<'a> {
    values: &'a Value,
}

/// The error is the message sent back to the caller.
pub type ArgumentResult<T> = Result<T, String>;

impl<'a> Arguments<'a> {
    pub fn new(values: &'a Value) -> Self {
        Arguments { values }
    }

    /// The value of `name`; `None` when it is absent or `null`.
    fn value(&self, name: &str) -> Option<&Value> {
        self.values.get(name).filter(|value| !value.is_null())
    }

    /// A number is accepted as its text: an id given where a name or an id is
    /// expected is still an id.
    pub fn text(&self, name: &str) -> ArgumentResult<Option<String>> {
        match self.value(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(Value::Number(number)) => Ok(Some(number.to_string())),
            Some(other) => Err(invalid(name, "a string", other)),
        }
    }

    pub fn required_text(&self, name: &str) -> ArgumentResult<String> {
        self.text(name)?.ok_or_else(|| format!("Missing required parameter: {name}"))
    }

    /// A non-negative whole number, also accepted as a numeric string.
    pub fn number(&self, name: &str) -> ArgumentResult<Option<u64>> {
        let expected = "a non-negative whole number";
        match self.value(name) {
            None => Ok(None),
            Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| invalid(name, expected, &Value::Number(number.clone()))),
            Some(Value::String(text)) => text.trim().parse().map(Some).map_err(|_| invalid(name, expected, &Value::String(text.clone()))),
            Some(other) => Err(invalid(name, expected, other)),
        }
    }

    pub fn required_number(&self, name: &str) -> ArgumentResult<u64> {
        self.number(name)?.ok_or_else(|| format!("Missing required parameter: {name}"))
    }

    /// A switch that is off unless given: `true`/`false`, the same as
    /// strings in any casing, or `1`/`0`.
    pub fn flag(&self, name: &str) -> ArgumentResult<bool> {
        let expected = "true or false";
        match self.value(name) {
            None => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(Value::Number(number)) => match number.as_u64() {
                Some(0) => Ok(false),
                Some(1) => Ok(true),
                _ => Err(invalid(name, expected, &Value::Number(number.clone()))),
            },
            Some(Value::String(text)) => match text.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(invalid(name, expected, &Value::String(text.clone()))),
            },
            Some(other) => Err(invalid(name, expected, other)),
        }
    }

    /// The project a call targets: `project` (a name, an id or a path) takes
    /// precedence over the numeric `project_id`; `None` means the active one.
    pub fn project_reference(&self) -> ArgumentResult<Option<String>> {
        if let Some(reference) = self.text("project")? {
            return Ok(Some(reference));
        }
        Ok(self.number("project_id")?.map(|id| id.to_string()))
    }
}

fn invalid(name: &str, expected: &str, got: &Value) -> String {
    format!("Invalid parameter \"{name}\": expected {expected}, got {got}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_numeric_string_is_the_number_it_spells() {
        let values = json!({ "project_id": "7", "other": 8 });
        let arguments = Arguments::new(&values);
        assert_eq!(arguments.number("project_id"), Ok(Some(7)));
        assert_eq!(arguments.number("other"), Ok(Some(8)));
        assert_eq!(arguments.number("absent"), Ok(None));
    }

    #[test]
    fn a_value_that_is_no_number_is_rejected_by_name() {
        let values = json!({ "project_id": "seven", "negative": -1, "list": [1] });
        let arguments = Arguments::new(&values);
        let message = arguments.number("project_id").unwrap_err();
        assert!(message.contains("\"project_id\"") && message.contains("seven"), "{message}");
        assert!(arguments.number("negative").is_err());
        assert!(arguments.number("list").is_err());
        assert!(arguments.required_number("absent").unwrap_err().contains("Missing required parameter: absent"));
    }

    #[test]
    fn a_flag_accepts_the_obvious_spellings_and_nothing_else() {
        let values = json!({ "a": true, "b": "TRUE", "c": 1, "d": "false", "e": 0, "f": "yes", "g": 2, "h": null });
        let arguments = Arguments::new(&values);
        for on in ["a", "b", "c"] {
            assert_eq!(arguments.flag(on), Ok(true), "{on}");
        }
        for off in ["d", "e", "h", "absent"] {
            assert_eq!(arguments.flag(off), Ok(false), "{off}");
        }
        for wrong in ["f", "g"] {
            assert!(arguments.flag(wrong).unwrap_err().contains(wrong), "{wrong}");
        }
    }

    #[test]
    fn text_takes_a_number_as_its_text_and_rejects_the_rest() {
        let values = json!({ "project": 3, "compiler": "12.0", "config": true });
        let arguments = Arguments::new(&values);
        assert_eq!(arguments.text("project"), Ok(Some("3".to_string())));
        assert_eq!(arguments.text("compiler"), Ok(Some("12.0".to_string())));
        assert!(arguments.text("config").unwrap_err().contains("\"config\""));
        assert!(arguments.required_text("absent").is_err());
    }

    #[test]
    fn the_project_reference_prefers_project_over_project_id() {
        let both = json!({ "project": "be", "project_id": 7 });
        assert_eq!(Arguments::new(&both).project_reference(), Ok(Some("be".to_string())));
        let id_only = json!({ "project_id": "7" });
        assert_eq!(Arguments::new(&id_only).project_reference(), Ok(Some("7".to_string())));
        let none = json!({});
        assert_eq!(Arguments::new(&none).project_reference(), Ok(None));
        let wrong = json!({ "project_id": "x" });
        assert!(Arguments::new(&wrong).project_reference().is_err());
    }
}
