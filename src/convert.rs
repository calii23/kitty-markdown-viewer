//! Converting JSON, YAML and TOML code blocks between each other, through
//! an order-preserving `serde_json::Value`.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Json,
    Yaml,
    Toml,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Json, Format::Yaml, Format::Toml];

    /// The format a code fence tag names, if it's one we convert.
    pub fn from_lang(lang: &str) -> Option<Format> {
        match lang.to_ascii_lowercase().as_str() {
            "json" => Some(Format::Json),
            "yaml" | "yml" => Some(Format::Yaml),
            "toml" => Some(Format::Toml),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::Yaml => "YAML",
            Format::Toml => "TOML",
        }
    }

    /// Fence tag for highlighting.
    pub fn lang(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Yaml => "yaml",
            Format::Toml => "toml",
        }
    }
}

/// A data block in its source format plus every format it converts to.
#[derive(Clone, Debug, PartialEq)]
pub struct Variants {
    pub source: Format,
    /// Text or the reason it can't be converted, in `Format::ALL` order.
    /// The source format's entry is the original text, untouched.
    pub texts: Vec<Result<String, String>>,
}

impl Variants {
    /// Returns `None` when `text` isn't valid in its own format, so there's
    /// nothing to convert.
    pub fn new(source: Format, text: &str) -> Option<Variants> {
        let value = parse(source, text).ok()?;
        let texts = Format::ALL
            .iter()
            .map(|&f| if f == source { Ok(text.trim_end().to_string()) } else { render(f, &value) })
            .collect();
        Some(Variants { source, texts })
    }

    pub fn get(&self, format: Format) -> &Result<String, String> {
        &self.texts[Format::ALL.iter().position(|f| *f == format).expect("all formats listed")]
    }
}

fn first_line(e: impl std::fmt::Display) -> String {
    e.to_string().lines().next().unwrap_or("invalid").trim().to_string()
}

fn parse(format: Format, text: &str) -> Result<Value, String> {
    match format {
        Format::Json => serde_json::from_str(text).map_err(first_line),
        Format::Yaml => yaml_serde::from_str(text).map_err(first_line),
        Format::Toml => text.parse::<toml::Table>().map(|t| from_toml(toml::Value::Table(t))).map_err(first_line),
    }
}

/// TOML values map onto JSON ones, except datetimes, which become strings.
fn from_toml(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(from_toml).collect()),
        toml::Value::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, from_toml(v))).collect()),
    }
}

fn render(format: Format, value: &Value) -> Result<String, String> {
    let text = match format {
        Format::Json => serde_json::to_string_pretty(value).map_err(first_line)?,
        Format::Yaml => yaml_serde::to_string(value).map_err(first_line)?,
        Format::Toml => {
            if !value.is_object() {
                return Err("TOML needs a table at the top level".into());
            }
            if contains_null(value) {
                return Err("TOML has no null".into());
            }
            toml::to_string_pretty(value).map_err(first_line)?
        }
    };
    Ok(text.trim_end().to_string())
}

fn contains_null(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.iter().any(contains_null),
        Value::Object(o) => o.values().any(contains_null),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_between_all_three() {
        let v = Variants::new(Format::Json, r#"{"name": "mdv", "tags": ["a", "b"], "nested": {"x": 1}}"#).unwrap();
        assert_eq!(v.get(Format::Yaml).as_deref().unwrap(), "name: mdv\ntags:\n- a\n- b\nnested:\n  x: 1");
        let toml = v.get(Format::Toml).as_deref().unwrap();
        assert!(toml.starts_with("name = \"mdv\""), "{toml}");
        assert!(toml.contains("[nested]\nx = 1"), "{toml}");
    }

    #[test]
    fn keeps_source_text_and_key_order() {
        let src = "zeta: 1\nalpha: 2\n";
        let v = Variants::new(Format::Yaml, src).unwrap();
        assert_eq!(v.get(Format::Yaml).as_deref().unwrap(), src.trim_end());
        assert_eq!(v.get(Format::Json).as_deref().unwrap(), "{\n  \"zeta\": 1,\n  \"alpha\": 2\n}");
    }

    #[test]
    fn reports_impossible_conversions() {
        let v = Variants::new(Format::Json, r#"{"a": null}"#).unwrap();
        assert_eq!(v.get(Format::Toml), &Err("TOML has no null".to_string()));
        let v = Variants::new(Format::Json, "[1, 2]").unwrap();
        assert!(v.get(Format::Toml).is_err());
        assert!(v.get(Format::Yaml).is_ok());
    }

    #[test]
    fn invalid_source_has_no_variants() {
        assert!(Variants::new(Format::Json, "{ nope").is_none());
    }

    #[test]
    fn toml_datetimes_become_strings() {
        let v = Variants::new(Format::Toml, "when = 1979-05-27T07:32:00Z\n").unwrap();
        assert!(v.get(Format::Json).as_deref().unwrap().contains("\"1979-05-27T07:32:00Z\""));
    }
}
