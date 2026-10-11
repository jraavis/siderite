//! Schema comparison, directional: a request schema may accept more and
//! a response schema may promise more, but not the other way round.
//!
//! Compared: `type` (with `null`), `enum`, `properties`, `required`,
//! `additionalProperties: false`, `items` and, for requests, length,
//! range, size and `pattern`/`format` constraints. Schemas using
//! `oneOf`, `anyOf`, `allOf`, `not`, conditionals, `patternProperties`
//! or `discriminator` that differ at all are reported as unknown.

use super::Diff;
use super::model::{Node, Pair, Unresolved, child, resolve};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Which side reads the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// The client writes it, the server reads it.
    Request,
    /// The server writes it, the client reads it.
    Response,
}

const COMPLEX: [&str; 9] = [
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
    "patternProperties",
    "discriminator",
];

/// Deepest nesting compared before giving up as unknown.
const MAX_DEPTH: usize = 32;

/// Upper bounds a request may not lower, and lower bounds it may not
/// raise.
const UPPER: [&str; 5] = [
    "maxLength",
    "maximum",
    "exclusiveMaximum",
    "maxItems",
    "maxProperties",
];
const LOWER: [&str; 5] = [
    "minLength",
    "minimum",
    "exclusiveMinimum",
    "minItems",
    "minProperties",
];

type Types = Option<BTreeSet<String>>;
type Dir = Direction;
type Members = Map<String, Value>;
type Set = BTreeSet<String>;

/// The accepted `type`s; `None` accepts anything. OpenAPI 3.0
/// `nullable: true` counts as `null`.
fn types(schema: &Value) -> Types {
    let mut found: Set = match schema.get("type") {
        Some(Value::String(one)) => [one.clone()].into(),
        Some(Value::Array(many)) => {
            let names = many.iter().filter_map(Value::as_str);
            names.map(str::to_owned).collect()
        }
        _ => return None,
    };
    if schema.get("nullable") == Some(&Value::Bool(true)) {
        found.insert("null".to_owned());
    }
    // An integer is a number; widening integer to number is fine.
    if found.contains("number") {
        found.insert("integer".to_owned());
    }
    Some(found)
}

/// Values of `set` that `of` lacks; `None` is the set of everything.
fn missing(set: &Types, of: &Types) -> Vec<String> {
    match (set, of) {
        (_, None) => Vec::new(),
        (None, Some(_)) => vec!["any type".to_owned()],
        (Some(set), Some(of)) => set.difference(of).cloned().collect(),
    }
}

fn strings(value: Option<&Value>) -> Set {
    let items = value.and_then(Value::as_array).into_iter().flatten();
    items.filter_map(Value::as_str).map(str::to_owned).collect()
}

fn enum_values(schema: &Value) -> Option<Set> {
    let values = schema.get("enum")?.as_array()?;
    Some(values.iter().map(Value::to_string).collect())
}

fn object(schema: &Value) -> Option<&Members> {
    schema.get("properties").and_then(Value::as_object)
}

fn closed(schema: &Value) -> bool {
    schema.get("additionalProperties") == Some(&Value::Bool(false))
}

fn complex(schema: &Value) -> bool {
    COMPLEX.iter().any(|key| schema.get(key).is_some())
}

/// The values of `x` that `y` lacks.
fn minus(x: &Set, y: &Set) -> Vec<String> {
    x.difference(y).cloned().collect()
}

/// `values` joined for a message.
fn list(values: &[String]) -> String {
    values.join(", ")
}

impl Diff<'_> {
    /// Compare the schemas of `p` in `direction`.
    pub(crate) fn schema(&mut self, dir: Dir, p: Pair<'_>, depth: usize) {
        let (base, current) = (self.base, self.current);
        let b = resolve(base, p.b.value, p.b.at);
        let c = resolve(current, p.c.value, p.c.at);
        let ((bv, bp), (cv, cp)) = match (b, c) {
            (Ok(b), Ok(c)) => (b, c),
            (Err(why), _) | (_, Err(why)) => return self.unresolved(&why, p),
        };
        if bv == cv || !self.seen.insert((bp.clone(), cp.clone())) {
            return;
        }
        let b = Node { value: bv, at: &bp };
        let c = Node { value: cv, at: &cp };
        let p = Pair { b, c };
        if depth > MAX_DEPTH {
            let msg = "schema nested too deeply to compare".to_owned();
            return self.unknown("schema_not_compared", msg, p);
        }
        if complex(bv) || complex(cv) {
            let msg = "composed or conditional schema changed".to_owned();
            return self.unknown("schema_not_compared", msg, p);
        }
        self.schema_types(dir, p);
        self.schema_enum(dir, p);
        if dir == Direction::Request {
            self.schema_bounds(p);
        }
        self.schema_object(dir, p, depth);
        let at = p.child("items");
        if let Some(items) = p.get("items", &at) {
            self.schema(dir, items, depth + 1);
        }
    }

    /// A reference that was not followed, at `p`.
    pub(crate) fn unresolved(&mut self, why: &Unresolved, p: Pair<'_>) {
        let message = match why {
            Unresolved::External(target) => {
                format!("external reference `{target}` was not fetched")
            }
            Unresolved::Missing(target) => {
                format!("reference `{target}` does not resolve")
            }
        };
        self.unknown("reference_not_resolved", message, p);
    }

    fn schema_types(&mut self, dir: Direction, p: Pair<'_>) {
        let (b, c) = (types(p.b.value), types(p.c.value));
        let (lost, verb) = match dir {
            Direction::Request => (missing(&b, &c), "no longer accepts"),
            Direction::Response => (missing(&c, &b), "may now return"),
        };
        if !lost.is_empty() {
            let msg = format!("{verb} {}", list(&lost));
            self.breaking("type_changed", msg, p);
        }
    }

    fn schema_enum(&mut self, dir: Direction, p: Pair<'_>) {
        let (b, c) = (enum_values(p.b.value), enum_values(p.c.value));
        match (dir, b, c) {
            (Direction::Request, Some(b), Some(c)) => {
                let gone = minus(&b, &c);
                if !gone.is_empty() {
                    let msg = format!("no longer accepts {}", list(&gone));
                    self.breaking("enum_value_removed", msg, p);
                }
            }
            (Direction::Request, None, Some(_)) => {
                let msg = "values are now limited to an enum".to_owned();
                self.breaking("enum_added", msg, p);
            }
            (Direction::Response, Some(b), Some(c)) => {
                let new = minus(&c, &b);
                if !new.is_empty() {
                    let msg = format!("may now return {}", list(&new));
                    self.unknown("enum_value_added", msg, p);
                }
            }
            (Direction::Response, Some(_), None) => {
                let msg = "values are no longer limited to an enum";
                self.unknown("enum_removed", msg.to_owned(), p);
            }
            _ => {}
        }
    }

    fn schema_bounds(&mut self, p: Pair<'_>) {
        let number = |n: Node<'_>, key| n.value.get(key)?.as_f64();
        for (keys, tighter) in [(UPPER, -1.0), (LOWER, 1.0)] {
            for key in keys {
                let narrowed = match (number(p.b, key), number(p.c, key)) {
                    (None, Some(_)) => true,
                    (Some(old), Some(new)) => (new - old) * tighter > 0.0,
                    _ => false,
                };
                if narrowed {
                    let msg = format!("`{key}` is stricter");
                    self.breaking("constraint_tightened", msg, p);
                }
            }
        }
        for key in ["pattern", "format"] {
            let (old, new) = (p.b.value.get(key), p.c.value.get(key));
            if new.is_some() && old != new {
                let msg = format!("`{key}` changed; accepted values differ");
                self.unknown("constraint_changed", msg, p);
            }
        }
    }

    fn schema_object(&mut self, dir: Direction, p: Pair<'_>, depth: usize) {
        let (b, c) = (object(p.b.value), object(p.c.value));
        let (Some(bp), Some(cp)) = (b, c) else {
            return;
        };
        let props = p.child("properties");
        for (name, old) in bp {
            let at = (child(&props.0, name), child(&props.1, name));
            let b = Node {
                value: old,
                at: &at.0,
            };
            let Some(now) = cp.get(name) else {
                self.property_removed(dir, name, Pair { b, c: p.c });
                continue;
            };
            let c = Node {
                value: now,
                at: &at.1,
            };
            self.schema(dir, Pair { b, c }, depth + 1);
        }
        self.schema_required(dir, p, cp);
        let required = strings(p.c.value.get("required"));
        for name in cp.keys().filter(|n| !bp.contains_key(*n)) {
            let optional = !required.contains(name);
            if dir == Direction::Response || optional {
                let at = child(&props.1, name);
                let msg = format!("property `{name}` added");
                self.added("property_added", msg, &at);
            }
        }
    }

    fn property_removed(&mut self, dir: Direction, name: &str, p: Pair<'_>) {
        let msg = format!("property `{name}` removed");
        if dir == Direction::Request && !closed(p.c.value) {
            self.unknown("property_removed", msg, p);
        } else {
            self.breaking("property_removed", msg, p);
        }
    }

    fn schema_required(&mut self, dir: Dir, p: Pair<'_>, now: &Members) {
        let b = strings(p.b.value.get("required"));
        let c = strings(p.c.value.get("required"));
        let at = p.child("required");
        let rb = Node {
            value: p.b.value,
            at: &at.0,
        };
        let rc = Node {
            value: p.c.value,
            at: &at.1,
        };
        let p = Pair { b: rb, c: rc };
        match dir {
            Direction::Request => {
                for name in c.difference(&b) {
                    let msg = format!("property `{name}` is now required");
                    self.breaking("required_property_added", msg, p);
                }
            }
            Direction::Response => {
                let kept = b.difference(&c).filter(|n| now.contains_key(*n));
                for name in kept {
                    let msg = format!("property `{name}` may now be absent");
                    self.breaking("property_no_longer_required", msg, p);
                }
            }
        }
    }
}
