//! Judge prompt templates (D163).
//!
//! One owned template named `prompt`. Undefined values fail, HTML is not
//! escaped, and a render stops after 100 000 units of fuel. Engine globals
//! such as `range` are not missing variables: the template analyzer names
//! them, and the engine supplies them.

use std::collections::BTreeSet;
use std::fmt;

use minijinja::value::Serde;
use minijinja::{AutoEscape, Environment, Error, ErrorKind, UndefinedBehavior, Value};

use crate::config::PROMPT_MAX_BYTES;
use crate::error::TemplateError;
use crate::mapping::resolve::Variables;
use crate::text::render_value;

const TEMPLATE_NAME: &str = "prompt";
const FUEL: u64 = 100_000;
/// A rendered prompt larger than this is rejected. 2 MiB.
const PROMPT_RESULT_MAX_BYTES: usize = 2 * 1024 * 1024;

/// A compiled judge prompt.
pub struct PromptTemplate {
    env: Environment<'static>,
}

impl PromptTemplate {
    /// Compiles one template.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::Syntax`] when the source is longer than 64 KiB
    /// or the engine rejects it. The kind is a fixed category, not the engine's
    /// detail text.
    pub fn compile(source: &str) -> Result<Self, TemplateError> {
        if source.len() > PROMPT_MAX_BYTES {
            return Err(TemplateError::Syntax {
                line: 0,
                kind: "template is longer than 64 KiB".to_owned(),
            });
        }
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_auto_escape_callback(|_| AutoEscape::None);
        env.set_fuel(Some(FUEL));
        env.set_formatter(|out, _state, value| write_rendered(out, value));
        env.add_template_owned(TEMPLATE_NAME, source.to_owned())
            .map_err(|err| syntax_error(&err))?;
        Ok(Self { env })
    }

    /// Top-level names the template reads and does not assign.
    ///
    /// A loop variable is not included. A filter name is not included.
    #[must_use]
    pub fn variables(&self) -> BTreeSet<String> {
        self.env
            .get_template(TEMPLATE_NAME)
            .map(|template| template.undeclared_variables(false).into_iter().collect())
            .unwrap_or_default()
    }

    /// Renders the prompt with `vars`.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::MissingVariable`] for the first supplied-name
    /// gap in sorted order. An undefined attribute is
    /// [`TemplateError::Undefined`]. Fuel exhaustion is
    /// [`TemplateError::TooComplex`]. A result over 2 MiB is
    /// [`TemplateError::PromptTooLarge`].
    pub fn render(&self, vars: &Variables) -> Result<String, TemplateError> {
        for name in self.variables() {
            if vars.get(&name).is_none() && !self.is_global(&name) {
                return Err(TemplateError::MissingVariable { name });
            }
        }
        let template = self
            .env
            .get_template(TEMPLATE_NAME)
            .map_err(|err| syntax_error(&err))?;
        let context = Value::from_pairs(
            vars.iter()
                .map(|(name, value)| (name, Value::from(Serde(value)))),
        );
        let rendered = template.render(context).map_err(|err| render_error(&err))?;
        if rendered.len() > PROMPT_RESULT_MAX_BYTES {
            return Err(TemplateError::PromptTooLarge {
                bytes: rendered.len(),
            });
        }
        Ok(rendered)
    }

    fn is_global(&self, name: &str) -> bool {
        self.env.globals().any(|(global, _)| global == name)
    }
}

impl fmt::Debug for PromptTemplate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromptTemplate")
            .finish_non_exhaustive()
    }
}

fn write_rendered(out: &mut minijinja::Output, value: &Value) -> Result<(), Error> {
    if value.is_undefined() || value.is_none() {
        return Ok(());
    }
    let json = serde_json::to_value(value)
        .map_err(|_| Error::new(ErrorKind::BadSerialization, "could not serialize value"))?;
    out.write_str(&render_value(&json))
        .map_err(|_| Error::new(ErrorKind::WriteFailure, "failed to write output"))
}

fn syntax_error(err: &Error) -> TemplateError {
    TemplateError::Syntax {
        line: err.line().unwrap_or(0),
        kind: err.kind().to_string(),
    }
}

fn render_error(err: &Error) -> TemplateError {
    match err.kind() {
        ErrorKind::UndefinedError => TemplateError::Undefined {
            line: err.line().unwrap_or(0),
        },
        ErrorKind::OutOfFuel => TemplateError::TooComplex,
        _ => syntax_error(err),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::PromptTemplate;
    use crate::config::PROMPT_MAX_BYTES;
    use crate::error::{TemplateError, assert_no_payload};
    use crate::mapping::resolve::Variables;

    const PLANTED: &str = "SECRET-PAYLOAD";

    fn vars(entries: &[(&str, Value)]) -> Variables {
        Variables::from_map(
            entries
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        )
    }

    fn render(source: &str, entries: &[(&str, Value)]) -> Result<String, TemplateError> {
        PromptTemplate::compile(source)?.render(&vars(entries))
    }

    #[test]
    fn renders_values_without_escaping() {
        let rendered = render("Hello {{ name }}", &[("name", json!("Ada"))]).unwrap();
        assert_eq!(rendered, "Hello Ada");

        let messages = json!([
            {"role": "user", "parts": [{"type": "text", "content": "hi"}]},
            {"role": "assistant", "parts": [{"type": "text", "content": "there"}]}
        ]);
        let rendered = render("{{ messages }}", &[("messages", messages)]).unwrap();
        assert_eq!(rendered, "user: hi\nassistant: there");

        let output = json!({"answer": "yes", "n": 1});
        let rendered = render("{{ output }}", &[("output", output.clone())]).unwrap();
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed, output);
        let rendered = render("{{ output.answer }}", &[("output", output)]).unwrap();
        assert_eq!(rendered, "yes");

        let rendered = render("X{{ value }}Y", &[("value", Value::Null)]).unwrap();
        assert_eq!(rendered, "XY");

        let rendered = render("{{ text }}", &[("text", json!("<b> & \"quotes\""))]).unwrap();
        assert_eq!(rendered, "<b> & \"quotes\"");

        let rendered = render(
            "{% for item in items %}{{ item }}{% endfor %}",
            &[("items", json!(["a", "b", "c"]))],
        )
        .unwrap();
        assert_eq!(rendered, "abc");
    }

    #[test]
    fn names_the_first_missing_variable_and_undefined_attribute() {
        let template = PromptTemplate::compile("{{ b }}{{ a }}").unwrap();
        let err = template.render(&vars(&[])).unwrap_err();
        assert_eq!(
            err,
            TemplateError::MissingVariable {
                name: "a".to_owned(),
            }
        );

        let err = template
            .render(&vars(&[("note", json!(PLANTED)), ("a", json!("here"))]))
            .unwrap_err();
        assert_eq!(
            err,
            TemplateError::MissingVariable {
                name: "b".to_owned(),
            }
        );
        assert_no_payload(&err.to_string(), PLANTED);

        let template = PromptTemplate::compile("line\n{{ output.answer }}").unwrap();
        let err = template
            .render(&vars(&[("output", json!({})), ("note", json!(PLANTED))]))
            .unwrap_err();
        assert_eq!(err, TemplateError::Undefined { line: 2 });
        assert_no_payload(&err.to_string(), PLANTED);
    }

    #[test]
    fn variables_skip_the_loop_name_and_the_filter() {
        let template = PromptTemplate::compile(
            "{% for item in items %}{{ item|upper }}{% endfor %}{{ name }}",
        )
        .unwrap();
        let expected: BTreeSet<String> = ["items", "name"].into_iter().map(str::to_owned).collect();
        assert_eq!(template.variables(), expected);
    }

    #[test]
    fn limits_fuel_size_and_syntax() {
        let err = render(
            "{% for i in range(100000) %}{% for j in range(100000) %}x{% endfor %}{% endfor %}",
            &[],
        )
        .unwrap_err();
        assert_eq!(err, TemplateError::TooComplex);
        assert_no_payload(&err.to_string(), PLANTED);

        let over = format!("{{{{ 'a' * {} }}}}", 2 * 1024 * 1024 + 1);
        let err = render(&over, &[]).unwrap_err();
        match err {
            TemplateError::PromptTooLarge { bytes } => {
                assert!(bytes > 2 * 1024 * 1024);
            }
            other => panic!("expected the size limit, got {other}"),
        }

        let exact = format!("{{{{ 'a' * {} }}}}", 2 * 1024 * 1024);
        let rendered = render(&exact, &[]).unwrap();
        assert_eq!(rendered.len(), 2 * 1024 * 1024);

        let err = PromptTemplate::compile(&format!("{{{{ {PLANTED}")).unwrap_err();
        assert_eq!(
            err,
            TemplateError::Syntax {
                line: 1,
                kind: "syntax error".to_owned(),
            }
        );
        assert_no_payload(&err.to_string(), PLANTED);

        let err = PromptTemplate::compile("ok\n{{").unwrap_err();
        assert_eq!(
            err,
            TemplateError::Syntax {
                line: 2,
                kind: "syntax error".to_owned(),
            }
        );

        assert!(PromptTemplate::compile(&"a".repeat(PROMPT_MAX_BYTES)).is_ok());
        let err = PromptTemplate::compile(&"a".repeat(PROMPT_MAX_BYTES + 1)).unwrap_err();
        assert_eq!(
            err,
            TemplateError::Syntax {
                line: 0,
                kind: "template is longer than 64 KiB".to_owned(),
            }
        );
    }
}
