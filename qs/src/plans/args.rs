//! Queue-item arguments. A plan's arguments arrive as
//! `{"args": [...], "kwargs": {...}}` (bluesky-queueserver's item shape);
//! [`Args`] maps the positional ones onto the plan's parameter names so a
//! factory reads every parameter by name whichever way the client sent it.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// One plan's arguments, addressed by parameter name.
#[derive(Debug)]
pub struct Args<'a> {
    plan: &'a str,
    names: &'static [&'static str],
    positional: Vec<Value>,
    kwargs: Map<String, Value>,
}

impl<'a> Args<'a> {
    /// Split `raw` against `names`, the plan's parameters in order. A
    /// positional argument beyond the last name, an unknown keyword, or a
    /// parameter given both ways is an error naming the plan.
    pub fn parse(
        plan: &'a str,
        names: &'static [&'static str],
        raw: &Value,
    ) -> Result<Self, String> {
        let obj = raw
            .as_object()
            .ok_or_else(|| format!("{plan}: arguments must be an object, got {raw}"))?;
        let positional = match obj.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(other) => return Err(format!("{plan}: 'args' must be an array, got {other}")),
        };
        let kwargs = match obj.get("kwargs") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(other) => return Err(format!("{plan}: 'kwargs' must be an object, got {other}")),
        };
        if positional.len() > names.len() {
            return Err(format!(
                "{plan}: takes at most {} positional arguments, got {}",
                names.len(),
                positional.len()
            ));
        }
        for key in kwargs.keys() {
            match names.iter().position(|n| n == key) {
                None => return Err(format!("{plan}: unknown argument {key}")),
                Some(i) if i < positional.len() => {
                    return Err(format!(
                        "{plan}: {key} given both positionally and by keyword"
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(Self {
            plan,
            names,
            positional,
            kwargs,
        })
    }

    fn get(&self, name: &str) -> Option<&Value> {
        let i = self
            .names
            .iter()
            .position(|n| *n == name)
            .unwrap_or_else(|| panic!("{}: {name} is not a declared parameter", self.plan));
        self.positional.get(i).or_else(|| self.kwargs.get(name))
    }

    fn required(&self, name: &str) -> Result<&Value, String> {
        self.get(name)
            .filter(|v| !v.is_null())
            .ok_or_else(|| format!("{}: missing argument {name}", self.plan))
    }

    fn number(&self, name: &str, v: &Value) -> Result<f64, String> {
        v.as_f64()
            .ok_or_else(|| format!("{}: {name} must be a number, got {v}", self.plan))
    }

    /// Whether `name` was given a non-null value.
    pub fn given(&self, name: &str) -> bool {
        self.get(name).is_some_and(|v| !v.is_null())
    }

    /// An optional number; `None` when absent or null.
    pub fn f64_opt(&self, name: &str) -> Result<Option<f64>, String> {
        self.get(name)
            .filter(|v| !v.is_null())
            .map(|v| self.number(name, v))
            .transpose()
    }

    /// A number with a default.
    pub fn f64_or(&self, name: &str, default: f64) -> Result<f64, String> {
        Ok(self.f64_opt(name)?.unwrap_or(default))
    }

    /// A non-negative integer with a default.
    pub fn count_or(&self, name: &str, default: usize) -> Result<usize, String> {
        match self.get(name).filter(|v| !v.is_null()) {
            None => Ok(default),
            Some(v) => {
                let n = self.number(name, v)?;
                if n < 0.0 || n.fract() != 0.0 {
                    return Err(format!(
                        "{}: {name} must be a non-negative integer, got {v}",
                        self.plan
                    ));
                }
                Ok(n as usize)
            }
        }
    }

    /// A boolean with a default (0/1 accepted, as Qt's `isChecked()` may
    /// arrive through JSON).
    pub fn bool_or(&self, name: &str, default: bool) -> Result<bool, String> {
        match self.get(name).filter(|v| !v.is_null()) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(Value::Number(n)) if n.as_f64() == Some(0.0) => Ok(false),
            Some(Value::Number(n)) if n.as_f64() == Some(1.0) => Ok(true),
            Some(v) => Err(format!("{}: {name} must be a boolean, got {v}", self.plan)),
        }
    }

    /// An optional string; `None` when absent, null or empty.
    pub fn str_opt(&self, name: &str) -> Result<Option<String>, String> {
        match self.get(name).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| (!s.is_empty()).then(|| s.to_string()))
                .ok_or_else(|| format!("{}: {name} must be a string, got {v}", self.plan)),
        }
    }

    /// A string with a default.
    pub fn str_or(&self, name: &str, default: &str) -> Result<String, String> {
        Ok(self.str_opt(name)?.unwrap_or_else(|| default.to_string()))
    }

    /// A required string.
    pub fn str(&self, name: &str) -> Result<String, String> {
        let v = self.required(name)?;
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{}: {name} must be a string, got {v}", self.plan))
    }

    /// A required list of numbers.
    pub fn f64_list(&self, name: &str) -> Result<Vec<f64>, String> {
        let v = self.required(name)?;
        self.numbers(name, v)
    }

    fn numbers(&self, name: &str, v: &Value) -> Result<Vec<f64>, String> {
        v.as_array()
            .ok_or_else(|| format!("{}: {name} must be a list of numbers, got {v}", self.plan))?
            .iter()
            .map(|x| self.number(name, x))
            .collect()
    }

    /// The `md` object, empty when absent.
    pub fn md(&self) -> Result<HashMap<String, Value>, String> {
        match self.get("md").filter(|v| !v.is_null()) {
            None => Ok(HashMap::new()),
            Some(Value::Object(m)) => Ok(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            Some(other) => Err(format!("{}: md must be an object, got {other}", self.plan)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NAMES: &[&str] = &["name", "ti", "th_list", "md"];

    #[test]
    fn positional_and_keyword_forms_read_the_same() {
        let a = Args::parse(
            "t",
            NAMES,
            &json!({"args": ["f", 2.5], "kwargs": {"th_list": [0.1, 0.2]}}),
        )
        .unwrap();
        assert_eq!(a.str("name").unwrap(), "f");
        assert_eq!(a.f64_or("ti", 3.0).unwrap(), 2.5);
        assert_eq!(a.f64_list("th_list").unwrap(), vec![0.1, 0.2]);
        assert!(a.md().unwrap().is_empty());
        let k = Args::parse(
            "t",
            NAMES,
            &json!({"kwargs": {"name": "g", "md": {"a": 1}}}),
        )
        .unwrap();
        assert_eq!(k.str("name").unwrap(), "g");
        assert_eq!(k.f64_or("ti", 3.0).unwrap(), 3.0);
        assert_eq!(k.md().unwrap()["a"], json!(1));
    }

    #[test]
    fn shape_errors_name_the_plan() {
        let e = Args::parse("t", NAMES, &json!({"args": [1, 2, 3, 4, 5]})).unwrap_err();
        assert!(e.starts_with("t: takes at most 4"), "{e}");
        let e = Args::parse("t", NAMES, &json!({"kwargs": {"bogus": 1}})).unwrap_err();
        assert_eq!(e, "t: unknown argument bogus");
        let e =
            Args::parse("t", NAMES, &json!({"args": ["f"], "kwargs": {"name": "g"}})).unwrap_err();
        assert!(e.contains("both positionally and by keyword"), "{e}");
        let a = Args::parse(
            "t",
            NAMES,
            &json!({"kwargs": {"ti": "x", "th_list": [1, "y"]}}),
        )
        .unwrap();
        assert_eq!(a.str("name").unwrap_err(), "t: missing argument name");
        assert_eq!(
            a.f64_or("ti", 0.0).unwrap_err(),
            "t: ti must be a number, got \"x\""
        );
        assert!(a.f64_list("th_list").is_err());
        assert!(a.count_or("ti", 0).is_err());
    }

    #[test]
    fn defaulted_readers() {
        let a = Args::parse(
            "t",
            NAMES,
            &json!({"kwargs": {"ti": 1, "th_list": 0, "name": "", "md": null}}),
        )
        .unwrap();
        assert!(a.given("ti") && a.given("name"));
        assert!(!a.given("md"));
        assert_eq!(a.count_or("th_list", 5).unwrap(), 0);
        assert_eq!(a.count_or("md", 5).unwrap(), 5);
        assert!(a.count_or("ti", 5).is_ok_and(|n| n == 1));
        assert!(a.bool_or("ti", false).unwrap());
        assert!(!a.bool_or("th_list", true).unwrap());
        assert!(a.bool_or("md", true).unwrap());
        assert!(a.bool_or("name", true).is_err());
        assert_eq!(a.f64_opt("ti").unwrap(), Some(1.0));
        assert_eq!(a.f64_opt("md").unwrap(), None);
        assert!(a.f64_opt("name").is_err());
        assert_eq!(a.str_opt("name").unwrap(), None);
        assert_eq!(a.str_or("name", "x").unwrap(), "x");
        assert!(a.str_opt("ti").is_err());
    }
}
