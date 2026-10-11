//! Paths, operations, parameters, request bodies and responses.

use super::Diff;
use super::model::{Impact, Node, Pair, Unresolved, child, resolve};
use super::schema::Direction;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Operation fields that are not compared; a difference is unknown.
const NOT_COMPARED: [&str; 2] = ["security", "callbacks"];

/// `/a/{id}` as `/a/{}`: parameter names do not change the route.
fn template(path: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in path.chars() {
        match c {
            '{' => {
                inside = true;
                out.push_str("{}");
            }
            '}' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

type Paths<'a> = BTreeMap<String, (&'a str, &'a Value)>;

fn paths(doc: &Value) -> Paths<'_> {
    let items = doc.get("paths").and_then(Value::as_object);
    let items = items.into_iter().flatten();
    items.map(|(k, v)| (template(k), (k.as_str(), v))).collect()
}

/// A parameter's `in` and name (header names in lowercase).
type Key = (String, String);

/// Parameters by key, with their pointers.
type Params<'a> = BTreeMap<Key, (&'a Value, String)>;

type Members = Map<String, Value>;

/// The object under `key`, or `empty`.
fn map<'v>(node: Node<'v>, key: &str, empty: &'v Members) -> &'v Members {
    let found = node.value.get(key).and_then(Value::as_object);
    found.unwrap_or(empty)
}

fn required(value: &Value) -> bool {
    value.get("required") == Some(&Value::Bool(true))
}

fn required_parameter(param: &Value) -> bool {
    param.get("in") == Some(&Value::from("path")) || required(param)
}

/// Removing a documented success response breaks clients; removing an
/// error response may not.
fn removal_impact(code: &str) -> Impact {
    if code.starts_with('2') || code.starts_with('3') {
        Impact::Breaking
    } else {
        Impact::Unknown
    }
}

/// The parameter's key, when it has a location and a name.
fn key(param: &Value) -> Option<Key> {
    let place = param.get("in")?.as_str()?;
    let name = param.get("name")?.as_str()?;
    let name = if place == "header" {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    };
    Some((place.to_owned(), name))
}

/// Something referenced in a document: the value and where it is.
type Found<'d> = Result<(&'d Value, String), Unresolved>;

/// The operation's request body, references followed.
fn body<'d>(doc: &'d Value, op: Node<'d>, at: &str) -> Option<Found<'d>> {
    Some(resolve(doc, op.value.get("requestBody")?, at))
}

impl Diff<'_> {
    /// Compare the whole documents.
    pub(super) fn document(&mut self) {
        let (base, current) = (self.base, self.current);
        if base.get("security") != current.get("security") {
            let b = Node {
                value: base,
                at: "/security",
            };
            let c = Node {
                value: current,
                at: "/security",
            };
            let msg = "global security requirements changed".to_owned();
            self.unknown("security_changed", msg, Pair { b, c });
        }
        let (before, after) = (paths(base), paths(current));
        for (key, (path, item)) in &before {
            let at = child("/paths", path);
            let Some((now_path, now_item)) = after.get(key) else {
                self.one_sided(item, &at, path, Impact::Breaking);
                continue;
            };
            let now_at = child("/paths", now_path);
            let b = Node {
                value: item,
                at: &at,
            };
            let c = Node {
                value: now_item,
                at: &now_at,
            };
            self.path_item(Pair { b, c }, (path, now_path));
        }
        for (key, (path, item)) in &after {
            if !before.contains_key(key) {
                let at = child("/paths", path);
                self.one_sided(item, &at, path, Impact::Additive);
            }
        }
    }

    /// Every operation of a path that one side lacks.
    fn one_sided(&mut self, item: &Value, at: &str, path: &str, i: Impact) {
        for method in METHODS.iter().filter(|m| item.get(**m).is_some()) {
            let name = format!("{} {path}", method.to_uppercase());
            self.operation = Some(name.clone());
            self.operation_change(i, &name, &child(at, method));
        }
        self.operation = None;
    }

    fn operation_change(&mut self, impact: Impact, name: &str, at: &str) {
        if impact == Impact::Additive {
            self.added("operation_added", format!("{name} added"), at);
        } else {
            let msg = format!("{name} removed");
            self.removed(impact, "operation_removed", msg, at);
        }
    }

    /// One path on both sides; operations are named by their CURRENT
    /// path, removed ones by their BASE path.
    fn path_item(&mut self, p: Pair<'_>, paths: (&str, &str)) {
        for method in METHODS {
            let verb = method.to_uppercase();
            let at = p.child(method);
            let (b, c) = (p.b.value.get(method), p.c.value.get(method));
            let path = if c.is_some() { paths.1 } else { paths.0 };
            let name = format!("{verb} {path}");
            self.operation = Some(name.clone());
            match (b, c) {
                (Some(_), None) => {
                    self.operation_change(Impact::Breaking, &name, &at.0);
                }
                (None, Some(_)) => {
                    self.operation_change(Impact::Additive, &name, &at.1);
                }
                (Some(b), Some(c)) => {
                    let b = Node {
                        value: b,
                        at: &at.0,
                    };
                    let c = Node {
                        value: c,
                        at: &at.1,
                    };
                    self.operation_pair(p, Pair { b, c });
                }
                (None, None) => {}
            }
        }
        self.operation = None;
    }

    fn operation_pair(&mut self, items: Pair<'_>, op: Pair<'_>) {
        for key in NOT_COMPARED {
            if op.b.value.get(key) != op.c.value.get(key) {
                let at = op.child(key);
                let b = Node {
                    value: op.b.value,
                    at: &at.0,
                };
                let c = Node {
                    value: op.c.value,
                    at: &at.1,
                };
                let msg = format!("`{key}` changed; not compared");
                self.unknown("not_compared", msg, Pair { b, c });
            }
        }
        let before = self.params(self.base, [items.b, op.b]);
        let after = self.params(self.current, [items.c, op.c]);
        self.parameter_pairs(&before, &after);
        self.request_body(op);
        self.responses(op);
    }

    /// Path-level parameters overridden by operation-level ones.
    fn params<'d>(&mut self, doc: &'d Value, n: [Node<'d>; 2]) -> Params<'d> {
        let mut found = Params::new();
        for node in n {
            let list = node.value.get("parameters").and_then(Value::as_array);
            let at = child(node.at, "parameters");
            for (i, raw) in list.into_iter().flatten().enumerate() {
                let raw_at = child(&at, &i.to_string());
                match resolve(doc, raw, &raw_at) {
                    Ok((param, param_at)) => {
                        if let Some(key) = key(param) {
                            found.insert(key, (param, param_at));
                        }
                    }
                    Err(why) => {
                        let n = Node {
                            value: raw,
                            at: &raw_at,
                        };
                        self.unresolved(&why, Pair { b: n, c: n });
                    }
                }
            }
        }
        found
    }

    fn parameter_pairs(&mut self, before: &Params<'_>, after: &Params<'_>) {
        for (key, (now, now_at)) in after {
            let label = format!("{} parameter `{}`", key.0, key.1);
            let Some((old, old_at)) = before.get(key) else {
                if required_parameter(now) {
                    let msg = format!("required {label} added");
                    self.required("required_parameter_added", msg, now_at);
                } else {
                    let msg = format!("{label} added");
                    self.added("parameter_added", msg, now_at);
                }
                continue;
            };
            let b = Node {
                value: old,
                at: old_at,
            };
            let c = Node {
                value: now,
                at: now_at,
            };
            let p = Pair { b, c };
            if required_parameter(now) && !required_parameter(old) {
                let msg = format!("{label} is now required");
                self.breaking("parameter_became_required", msg, p);
            }
            let at = p.child("schema");
            if let Some(schemas) = p.get("schema", &at) {
                self.schema(Direction::Request, schemas, 0);
            }
        }
        for (key, (_, old_at)) in before {
            if !after.contains_key(key) {
                let msg = format!(
                    "{} parameter `{}` removed; clients that send it may be \
                     ignored or rejected",
                    key.0, key.1
                );
                let kind = "parameter_removed";
                self.removed(Impact::Unknown, kind, msg, old_at);
            }
        }
    }

    fn request_body(&mut self, op: Pair<'_>) {
        let at = op.child("requestBody");
        let b = body(self.base, op.b, &at.0);
        let c = body(self.current, op.c, &at.1);
        match (b, c) {
            (Some(Err(why)), _) | (_, Some(Err(why))) => {
                let b = Node {
                    value: op.b.value,
                    at: &at.0,
                };
                let c = Node {
                    value: op.c.value,
                    at: &at.1,
                };
                self.unresolved(&why, Pair { b, c });
            }
            (None, Some(Ok((now, now_at)))) => self.body_added(now, &now_at),
            (Some(Ok((_, old_at))), None) => {
                let msg = "request body removed; clients that send one may \
                           be rejected";
                let kind = "request_body_removed";
                self.removed(Impact::Unknown, kind, msg.to_owned(), &old_at);
            }
            (Some(Ok((old, old_at))), Some(Ok((now, now_at)))) => {
                let b = Node {
                    value: old,
                    at: &old_at,
                };
                let c = Node {
                    value: now,
                    at: &now_at,
                };
                if required(now) && !required(old) {
                    let msg = "the request body is now required".to_owned();
                    self.breaking("request_body_required", msg, Pair { b, c });
                }
                self.content(Direction::Request, Pair { b, c });
            }
            (None, None) => {}
        }
    }

    fn body_added(&mut self, body: &Value, at: &str) {
        if required(body) {
            let msg = "a request body is now required".to_owned();
            self.required("request_body_required", msg, at);
        } else {
            let msg = "optional request body added".to_owned();
            self.added("request_body_added", msg, at);
        }
    }

    /// Media types and their schemas.
    fn content(&mut self, dir: Direction, p: Pair<'_>) {
        let empty = Members::new();
        let before = map(p.b, "content", &empty);
        let after = map(p.c, "content", &empty);
        let at = p.child("content");
        for (kind, old) in before {
            let old_at = child(&at.0, kind);
            let Some(now) = after.get(kind) else {
                let msg = format!("media type `{kind}` removed");
                let id = "media_type_removed";
                self.removed(Impact::Breaking, id, msg, &old_at);
                continue;
            };
            let now_at = child(&at.1, kind);
            let b = Node {
                value: old,
                at: &old_at,
            };
            let c = Node {
                value: now,
                at: &now_at,
            };
            let media = Pair { b, c };
            let schema_at = media.child("schema");
            if let Some(schemas) = media.get("schema", &schema_at) {
                self.schema(dir, schemas, 0);
            }
        }
        for kind in after.keys().filter(|k| !before.contains_key(*k)) {
            let msg = format!("media type `{kind}` added");
            self.added("media_type_added", msg, &child(&at.1, kind));
        }
    }

    fn responses(&mut self, op: Pair<'_>) {
        let empty = Members::new();
        let before = map(op.b, "responses", &empty);
        let after = map(op.c, "responses", &empty);
        let at = op.child("responses");
        for (code, old) in before {
            let old_at = child(&at.0, code);
            let Some(now) = after.get(code) else {
                let msg = format!("response `{code}` removed");
                let impact = removal_impact(code);
                self.removed(impact, "response_removed", msg, &old_at);
                continue;
            };
            let now_at = child(&at.1, code);
            let b = resolve(self.base, old, &old_at);
            let c = resolve(self.current, now, &now_at);
            match (b, c) {
                (Ok((bv, bp)), Ok((cv, cp))) => {
                    let b = Node { value: bv, at: &bp };
                    let c = Node { value: cv, at: &cp };
                    self.content(Direction::Response, Pair { b, c });
                }
                (Err(why), _) | (_, Err(why)) => {
                    let b = Node {
                        value: old,
                        at: &old_at,
                    };
                    let c = Node {
                        value: now,
                        at: &now_at,
                    };
                    self.unresolved(&why, Pair { b, c });
                }
            }
        }
        for code in after.keys().filter(|k| !before.contains_key(*k)) {
            let msg = format!("response `{code}` added");
            self.added("response_added", msg, &child(&at.1, code));
        }
    }
}
