//! Generates `src/generated/<api>.rs` from `specs/<api>.json`.
//!
//! Mapping rules (kept deliberately simple):
//! - component schemas become structs, enums, or type aliases;
//! - inline objects and enums become named types (`ParentField`);
//! - a field is `Option<T>` unless the spec marks it required and not nullable;
//! - integer enums get names from `x-enumNames`, `x-enum-varnames`, `oneOf`
//!   titles, or `N = Name` lines in the description;
//! - unions that are not a nullable wrapper become `serde_json::Value`;
//! - every operation becomes one async method on the API's client.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use heck::{ToSnakeCase, ToUpperCamelCase};
use serde_json::{Map, Value};

use crate::{Naming, Result, SPECS, Spec};

const METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// Names the generated code relies on, which a schema must not shadow.
const RESERVED_TYPES: &[&str] = &[
    "Option",
    "Result",
    "Vec",
    "String",
    "Box",
    "Value",
    "HashMap",
    "Bytes",
    "Error",
    "Config",
    "Transport",
    "Default",
    "Serialize",
    "Deserialize",
    "Self",
];

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn",
    "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof", "unsized",
    "virtual", "yield", "try", "gen",
];

pub fn run(root: &Path) -> Result {
    let out_dir = root.join("src/generated");
    std::fs::create_dir_all(&out_dir)?;
    let mut files = Vec::new();
    for spec in SPECS {
        let path = root.join("specs").join(format!("{}.json", spec.name));
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let code = Generator::new(spec, &json)
            .generate()
            .map_err(|e| format!("{}: {e}", spec.name))?;
        let out = out_dir.join(format!("{}.rs", spec.name));
        std::fs::write(&out, code)?;
        files.push(out);
    }
    let status = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .args(&files)
        .status()?;
    if !status.success() {
        return Err("rustfmt failed on generated code".into());
    }
    for file in &files {
        println!("generated {}", file.display());
    }
    Ok(())
}

struct Generator<'a> {
    spec: &'a Spec,
    doc: &'a Value,
    /// Component schema name -> Rust type name.
    names: HashMap<String, String>,
    /// Every type name already used in the module.
    taken: HashSet<String>,
    /// Emitted type definitions.
    types: String,
}

impl<'a> Generator<'a> {
    fn new(spec: &'a Spec, doc: &'a Value) -> Self {
        let mut generator = Generator {
            spec,
            doc,
            names: HashMap::new(),
            taken: RESERVED_TYPES.iter().map(|s| s.to_string()).collect(),
            types: String::new(),
        };
        generator.taken.insert(spec.client.to_owned());
        for name in generator.schemas().keys() {
            let rust = generator.unique(&type_ident(name));
            generator.names.insert(name.clone(), rust);
        }
        generator
    }

    fn schemas(&self) -> &'a Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.doc
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    /// Reserves a type name, adding a number if it is taken.
    fn unique(&mut self, base: &str) -> String {
        let mut name = base.to_owned();
        let mut n = 2;
        while self.taken.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.taken.insert(name.clone());
        name
    }

    /// Follows a local `$ref` (`#/components/...`).
    fn resolve(&self, reference: &str) -> Result<&'a Value> {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("unsupported $ref {reference}"))?;
        self.doc
            .pointer(pointer)
            .ok_or_else(|| format!("dangling $ref {reference}").into())
    }

    /// Follows `$ref`s until reaching a concrete object.
    fn deref(&self, value: &Value) -> Result<Value> {
        match value.get("$ref").and_then(Value::as_str) {
            Some(r) => self.deref(self.resolve(r)?),
            None => Ok(value.clone()),
        }
    }

    fn ref_type(&self, reference: &str) -> Result<String> {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .ok_or_else(|| format!("unsupported schema $ref {reference}"))?;
        self.names
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown schema {name}").into())
    }

    // ---------------------------------------------------------------- types

    /// The Rust type for a schema (without the outer `Option`). Inline objects
    /// and enums are defined as named types using `hint`.
    fn ty(&mut self, schema: &Value, hint: &str) -> Result<String> {
        let (s, _) = unwrap_nullable(schema);
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            return self.ref_type(r);
        }
        if is_enum(&s) {
            if enum_kind(&s) == EnumKind::Int && enum_names(&s).is_none() {
                return Ok(int_type(&s).to_owned());
            }
            let name = self.unique(hint);
            self.define_enum(&name, &s)?;
            return Ok(name);
        }
        if is_struct(&s) {
            let name = self.unique(hint);
            self.define_struct(&name, &s)?;
            return Ok(name);
        }
        Ok(match s.get("type").and_then(Value::as_str) {
            Some("string") => "String".into(),
            Some("integer") => int_type(&s).into(),
            Some("number") => "f64".into(),
            Some("boolean") => "bool".into(),
            Some("array") => {
                if let Some(Value::Array(items)) = s.get("prefixItems") {
                    let mut parts = Vec::new();
                    for (i, item) in items.iter().enumerate() {
                        parts.push(self.ty(item, &format!("{hint}Item{i}"))?);
                    }
                    format!("({})", parts.join(", "))
                } else {
                    let items = s.get("items").cloned().unwrap_or(Value::Null);
                    format!("Vec<{}>", self.ty(&items, &format!("{hint}Item"))?)
                }
            }
            _ => match s.get("additionalProperties") {
                Some(ap @ Value::Object(m)) if !m.is_empty() => format!(
                    "std::collections::HashMap<String, {}>",
                    self.ty(ap, &format!("{hint}Value"))?
                ),
                _ => "serde_json::Value".into(),
            },
        })
    }

    /// Defines a component schema under its Rust name.
    fn define(&mut self, name: &str, schema: &Value) -> Result {
        let (s, _) = unwrap_nullable(schema);
        if is_enum(&s) && !(enum_kind(&s) == EnumKind::Int && enum_names(&s).is_none()) {
            return self.define_enum(name, &s);
        }
        if is_struct(&s) {
            return self.define_struct(name, &s);
        }
        let target = self.ty(&s, &format!("{name}Inner"))?;
        let target = if target == name {
            "serde_json::Value".into()
        } else {
            target
        };
        let mut out = doc_comment(description(schema), "");
        writeln!(out, "pub type {name} = {target};\n")?;
        self.types.push_str(&out);
        Ok(())
    }

    fn define_enum(&mut self, name: &str, s: &Value) -> Result {
        let kind = enum_kind(s);
        let variants = enum_variants(s);
        if variants.is_empty() || kind == EnumKind::Mixed {
            let mut out = doc_comment(description(s), "");
            writeln!(out, "pub type {name} = serde_json::Value;\n")?;
            self.types.push_str(&out);
            return Ok(());
        }
        let mut seen_values = HashSet::new();
        let mut seen_idents = HashSet::new();
        let mut body = String::new();
        for (value, label, doc) in variants {
            if !seen_values.insert(value.to_string()) {
                continue;
            }
            let base = variant_ident(&label);
            let mut ident = base.clone();
            let mut n = 2;
            while !seen_idents.insert(ident.clone()) {
                ident = format!("{base}{n}");
                n += 1;
            }
            body.push_str(&doc_comment(doc.as_deref(), "        "));
            let literal = match &value {
                Value::String(v) => format!("{v:?}"),
                other => other.to_string(),
            };
            writeln!(body, "        {ident} = {literal},")?;
        }
        let mac = if kind == EnumKind::Int {
            "int_enum"
        } else {
            "str_enum"
        };
        // Docs go inside the macro call; rustfmt leaves macro bodies alone,
        // so indent by hand.
        let docs = doc_comment(description(s), "    ");
        writeln!(
            self.types,
            "{mac}! {{\n{docs}    {name} {{\n{body}    }}\n}}\n"
        )?;
        Ok(())
    }

    fn define_struct(&mut self, name: &str, s: &Value) -> Result {
        let props = self.collect_props(s)?;
        let mut fields = String::new();
        let mut idents = HashSet::new();
        for (json_name, schema, required) in props {
            let base = field_ident(&json_name);
            let mut ident = base.clone();
            let mut n = 2;
            while !idents.insert(ident.clone()) {
                ident = format!("{base}_{n}");
                n += 1;
            }
            let (_, nullable) = unwrap_nullable(&schema);
            let mut ty = self.ty(&schema, &format!("{name}{}", type_ident(&json_name)))?;
            if ty == name {
                ty = format!("Box<{ty}>");
            }
            let optional = !required || nullable;
            let mut doc = description(&schema).unwrap_or_default().trim().to_owned();
            if unwrap_nullable(&schema)
                .0
                .get("format")
                .and_then(Value::as_str)
                == Some("decimal")
            {
                // serde_json parses numbers as f64; lossless decimals would need
                // its `arbitrary_precision` feature, which breaks untagged enums.
                if !doc.is_empty() {
                    doc.push_str("\n\n");
                }
                doc.push_str("A decimal number, parsed as `f64` (about 15 significant digits).");
            }
            fields.push_str(&doc_comment(
                Some(doc.as_str()).filter(|d| !d.is_empty()),
                "    ",
            ));
            let rename = if ident.trim_start_matches("r#") == json_name {
                String::new()
            } else {
                format!("rename = {json_name:?}")
            };
            let mut attrs = vec![];
            if !rename.is_empty() {
                attrs.push(rename);
            }
            if optional {
                attrs.push("skip_serializing_if = \"Option::is_none\"".into());
                ty = format!("Option<{ty}>");
            }
            if !attrs.is_empty() {
                writeln!(fields, "    #[serde({})]", attrs.join(", "))?;
            }
            writeln!(fields, "    pub {ident}: {ty},")?;
        }
        let mut out = doc_comment(description(s), "");
        writeln!(
            out,
            "#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]\n\
             #[serde(default)]\n\
             pub struct {name} {{\n{fields}}}\n"
        )?;
        self.types.push_str(&out);
        Ok(())
    }

    /// Properties of an object schema, including those inherited via `allOf`.
    fn collect_props(&self, s: &Value) -> Result<Vec<(String, Value, bool)>> {
        let mut props: Vec<(String, Value, bool)> = Vec::new();
        if let Some(Value::Array(parts)) = s.get("allOf") {
            for part in parts {
                for prop in self.collect_props(&self.deref(part)?)? {
                    props.retain(|p| p.0 != prop.0);
                    props.push(prop);
                }
            }
        }
        let required: BTreeSet<&str> = s
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if let Some(Value::Object(own)) = s.get("properties") {
            for (name, schema) in own {
                props.retain(|p| &p.0 != name);
                props.push((
                    name.clone(),
                    schema.clone(),
                    required.contains(name.as_str()),
                ));
            }
        }
        Ok(props)
    }

    // ----------------------------------------------------------- operations

    fn method_name(&self, method: &str, path: &str, op: &Value) -> Result<String> {
        let key = format!("{} {path}", method.to_uppercase());
        if let Some((_, name)) = self.spec.overrides.iter().find(|(k, _)| *k == key) {
            return Ok((*name).to_owned());
        }
        let summary = op.get("summary").and_then(Value::as_str);
        let operation_id = op.get("operationId").and_then(Value::as_str);
        let source = match self.spec.naming {
            Naming::Summary => summary.or(operation_id),
            Naming::OperationId => operation_id.or(summary),
        }
        .ok_or_else(|| format!("{key} has no summary or operationId; add an override"))?;
        // "Get the list of profiles" -> get_list_of_profiles
        let name = source
            .replace("(preview)", "")
            .to_snake_case()
            .split('_')
            .filter(|w| !matches!(*w, "a" | "an" | "the"))
            .collect::<Vec<_>>()
            .join("_");
        if name.is_empty() {
            return Err(format!("{key} produced an empty method name; add an override").into());
        }
        Ok(field_ident(&name))
    }

    fn operation(
        &mut self,
        method: &str,
        path: &str,
        op: &Value,
        shared_params: &[Value],
    ) -> Result<(String, String)> {
        let name = self.method_name(method, path, op)?;
        let pascal = name.trim_start_matches("r#").to_upper_camel_case();

        // Parameters: operation-level ones override path-level ones.
        let mut params: Vec<Value> = Vec::new();
        for p in shared_params.iter().chain(
            op.get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        ) {
            let p = self.deref(p)?;
            params.retain(|q| !(q.get("name") == p.get("name") && q.get("in") == p.get("in")));
            params.push(p);
        }

        let mut args = Vec::new();
        let mut format_args = Vec::new();
        let mut format_path = String::new();
        let mut rest = path;
        while let Some(start) = rest.find('{') {
            let end = rest[start..].find('}').ok_or("unclosed { in path")? + start;
            format_path.push_str(&rest[..start]);
            format_path.push_str("{}");
            let pname = &rest[start + 1..end];
            let param = params
                .iter()
                .find(|p| p["name"] == pname && p["in"] == "path")
                .ok_or_else(|| format!("{path}: no path parameter {pname}"))?;
            let ident = field_ident(pname);
            let (schema, _) = unwrap_nullable(param.get("schema").unwrap_or(&Value::Null));
            let ty = match schema.get("type").and_then(Value::as_str) {
                Some("integer") => int_type(&schema).to_owned(),
                _ => "&str".to_owned(),
            };
            args.push(format!("{ident}: {ty}"));
            format_args.push(format!("crate::transport::segment({ident})"));
            rest = &rest[end + 1..];
        }
        format_path.push_str(rest);

        // Query parameters become one struct.
        let query: Vec<&Value> = params.iter().filter(|p| p["in"] == "query").collect();
        let query_type = if query.is_empty() {
            None
        } else {
            let qname = self.unique(&format!("{pascal}Query"));
            let mut fields = String::new();
            let mut idents = HashSet::new();
            // `Default` values: the spec's default (or minimum) for required
            // parameters, so `&Default::default()` makes a valid request.
            let mut defaults = Vec::new();
            let mut custom_default = false;
            for p in query {
                let json_name = p["name"].as_str().ok_or("query parameter without name")?;
                let mut ident = field_ident(json_name);
                while !idents.insert(ident.clone()) {
                    ident.push('_');
                }
                let schema = p.get("schema").cloned().unwrap_or(Value::Null);
                let required = p.get("required") == Some(&Value::Bool(true));
                let ty = self.ty(&schema, &format!("{qname}{}", type_ident(json_name)))?;
                fields.push_str(&doc_comment(
                    p.get("description").and_then(Value::as_str),
                    "    ",
                ));
                if required {
                    writeln!(fields, "    #[serde(rename = {json_name:?})]")?;
                    writeln!(fields, "    pub {ident}: {ty},")?;
                    let (inner, _) = unwrap_nullable(&schema);
                    match default_literal(&inner) {
                        Some(value) => {
                            custom_default = true;
                            defaults.push(format!("{ident}: {value}"));
                        }
                        None => defaults.push(format!("{ident}: Default::default()")),
                    }
                } else {
                    defaults.push(format!("{ident}: None"));
                    writeln!(
                        fields,
                        "    #[serde(rename = {json_name:?}, skip_serializing_if = \"Option::is_none\")]"
                    )?;
                    writeln!(fields, "    pub {ident}: Option<{ty}>,")?;
                }
            }
            let derives = if custom_default {
                "Debug, Clone, PartialEq, Serialize"
            } else {
                "Debug, Clone, Default, PartialEq, Serialize"
            };
            writeln!(
                self.types,
                "/// Query parameters for [`{client}::{name}`].\n\
                 #[derive({derives})]\n\
                 pub struct {qname} {{\n{fields}}}\n",
                client = self.spec.client,
                name = name.trim_start_matches("r#"),
            )?;
            if custom_default {
                writeln!(
                    self.types,
                    "impl Default for {qname} {{\n\
                     fn default() -> Self {{\n\
                     Self {{ {} }}\n\
                     }}\n\
                     }}\n",
                    defaults.join(", ")
                )?;
            }
            args.push(format!("query: &{qname}"));
            Some(qname)
        };

        // Request body.
        let mut body_code = String::new();
        if let Some(body) = op.get("requestBody") {
            let body = self.deref(body)?;
            let content = body
                .get("content")
                .and_then(Value::as_object)
                .ok_or("requestBody without content")?;
            if let Some(schema) = json_content(content) {
                let ty = self.ty(&schema, &format!("{pascal}Body"))?;
                args.push(format!("body: &{ty}"));
                body_code = ".json(body)".into();
            } else if content
                .keys()
                .any(|k| k == "application/octet-stream" || k.starts_with("video/"))
            {
                args.push("body: impl Into<reqwest::Body>".into());
                body_code = ".header(reqwest::header::CONTENT_TYPE, \"application/octet-stream\").body(body)"
                    .into();
            } else {
                return Err(format!(
                    "{} {path}: unsupported request content {:?}",
                    method.to_uppercase(),
                    content.keys().collect::<Vec<_>>()
                )
                .into());
            }
        }

        // Response: the first 2xx response that has a body. If another 2xx
        // answer has none (e.g. 200 with JSON, or 204), the result is optional.
        let mut codes: Vec<(&String, &Value)> = op
            .get("responses")
            .and_then(Value::as_object)
            .map(|r| r.iter().filter(|(c, _)| c.starts_with('2')).collect())
            .unwrap_or_default();
        codes.sort_by(|a, b| a.0.cmp(b.0));
        let mut chosen: Option<(Value, Map<String, Value>)> = None;
        let mut has_empty = false;
        for (code, response) in codes {
            let response = self.deref(response)?;
            let content = response
                .get("content")
                .and_then(Value::as_object)
                .filter(|c| !c.is_empty())
                .cloned();
            match content {
                Some(content) if code != "204" => {
                    if chosen.is_none() {
                        chosen = Some((response, content));
                    }
                }
                _ => has_empty = true,
            }
        }

        let mut ret = "()".to_owned();
        let mut call = "self.transport.empty(request)".to_owned();
        let mut returns = String::new();
        match chosen {
            Some((response, content)) => {
                let what = description(&response).unwrap_or_default().trim();
                if what.split_whitespace().count() > 3 {
                    returns = format!("Returns: {what}");
                }
                // A text type other than plain text (e.g. text/csv) is what
                // such an endpoint is for, even if JSON is also offered.
                let text_media = content.keys().find(|k| {
                    k.starts_with("text/") && !matches!(k.as_str(), "text/plain" | "text/json")
                });
                if let Some(media) = text_media {
                    ret = "String".into();
                    call = "self.transport.text(request)".into();
                    body_code.push_str(&format!(".header(reqwest::header::ACCEPT, {media:?})"));
                } else if let Some(schema) = json_content(&content) {
                    ret = self.ty(&schema, &format!("{pascal}Response"))?;
                    call = match self.list_shape(&params, &schema, &ret)? {
                        Some(shape) => format!(
                            "self.transport.json_list(request, crate::transport::ListShape::{shape})"
                        ),
                        None => "self.transport.json(request)".into(),
                    };
                    if has_empty {
                        ret = format!("Option<{ret}>");
                    }
                } else if content.keys().any(|k| k.starts_with("text/")) {
                    ret = "String".into();
                    call = "self.transport.text(request)".into();
                } else {
                    ret = "bytes::Bytes".into();
                    call = "self.transport.bytes(request)".into();
                }
            }
            // A GET always returns something, even when the spec doesn't
            // describe it (e.g. PDF downloads).
            None if method == "get" => {
                ret = "bytes::Bytes".into();
                call = "self.transport.bytes(request)".into();
            }
            None => {}
        }

        let path_expr = if format_args.is_empty() {
            format!("{format_path:?}")
        } else {
            format!("&format!({format_path:?}, {})", format_args.join(", "))
        };
        let http_method = method.to_uppercase();
        let request = match &query_type {
            Some(_) => format!(
                "self.transport.request_with_query(reqwest::Method::{http_method}, {path_expr}, query)?"
            ),
            None => format!("self.transport.request(reqwest::Method::{http_method}, {path_expr})"),
        };

        let mut doc = String::new();
        let summary = op
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let desc = op
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut text = summary.trim().to_owned();
        if !desc.trim().is_empty() && desc.trim() != summary.trim() {
            text = format!("{text}\n\n{}", desc.trim());
        }
        if !returns.is_empty() {
            text = format!("{text}\n\n{returns}");
        }
        text = format!("{}\n\n`{http_method} {path}`", text.trim());
        doc.push_str(&doc_comment(Some(&text), "    "));
        if op.get("deprecated") == Some(&Value::Bool(true)) {
            doc.push_str("    #[deprecated(note = \"deprecated by bunny.net\")]\n");
        }

        let mut code = String::new();
        let args = std::iter::once("&self".to_owned())
            .chain(args)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            code,
            "{doc}    pub async fn {name}({args}) -> crate::Result<{ret}> {{\n\
             let request = {request}{body_code};\n\
             {call}.await\n\
             }}\n"
        )?;
        Ok((name, code))
    }

    /// For list endpoints (those with a `page` query parameter): the shape
    /// the response must be normalised to, as a `ListShape` variant.
    fn list_shape(&self, params: &[Value], schema: &Value, ret: &str) -> Result<Option<String>> {
        let paged = params.iter().any(|p| {
            p["in"] == "query"
                && p["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case("page"))
        });
        if !paged {
            return Ok(None);
        }
        if ret.starts_with("Vec<") {
            return Ok(Some("Array".into()));
        }
        let (s, _) = unwrap_nullable(schema);
        let items = self
            .collect_props(&self.deref(&s)?)?
            .into_iter()
            .find(|(name, prop, _)| {
                name.eq_ignore_ascii_case("items")
                    && unwrap_nullable(prop).0.get("type").and_then(Value::as_str) == Some("array")
            });
        Ok(items.map(|(name, _, _)| format!("Page({name:?})")))
    }

    // ----------------------------------------------------------- the module

    fn generate(mut self) -> Result<String> {
        let schemas = self.schemas();
        for (name, schema) in schemas {
            let rust = self.names[name].clone();
            self.define(&rust, schema)?;
        }

        let mut methods = String::new();
        let mut method_names = HashSet::new();
        let doc = self.doc;
        let paths = doc
            .get("paths")
            .and_then(Value::as_object)
            .ok_or("spec has no paths")?;
        for (path, item) in paths {
            let shared: Vec<Value> = item
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for method in METHODS {
                let Some(op) = item.get(method) else { continue };
                let (name, code) = self.operation(method, path, op, &shared)?;
                if !method_names.insert(name.clone()) {
                    return Err(format!(
                        "two operations are named `{name}` (latest: {} {path}); add an override in xtask/src/main.rs",
                        method.to_uppercase()
                    )
                    .into());
                }
                methods.push_str(&code);
            }
        }

        let base_url = self
            .doc
            .pointer("/servers/0/url")
            .and_then(Value::as_str)
            .ok_or("spec has no servers[0].url")?
            .trim_end_matches('/');
        let spec = self.spec;
        let version = self
            .doc
            .pointer("/info/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        let mut out = String::new();
        writeln!(
            out,
            "//! bunny.net {title}.\n\
             //!\n\
             //! Generated by `cargo xtask codegen` from `specs/{file}.json` (spec version {version}).\n\
             //! Do not edit by hand.\n\
             \n\
             #![allow(\n\
                 deprecated,\n\
                 clippy::all,\n\
                 rustdoc::bare_urls,\n\
                 rustdoc::broken_intra_doc_links,\n\
                 rustdoc::invalid_html_tags,\n\
                 rustdoc::invalid_rust_codeblocks\n\
             )]\n\
             \n\
             use serde::{{Deserialize, Serialize}};\n\
             \n\
             /// Default API base URL.\n\
             pub const BASE_URL: &str = {base_url:?};\n\
             \n\
             /// Client for the bunny.net {title}.\n\
             ///\n\
             /// Authenticates with {key}. Cheap to clone; clones share the connection pool.\n\
             #[derive(Debug, Clone)]\n\
             pub struct {client} {{\n\
                 pub(crate) transport: crate::transport::Transport,\n\
             }}\n\
             \n\
             impl {client} {{\n\
                 /// Creates a client authenticated with {key}.\n\
                 pub fn new(api_key: &str) -> crate::Result<Self> {{\n\
                     Self::with_config(api_key, crate::Config::default())\n\
                 }}\n\
             \n\
                 /// Like [`{client}::new`], with custom settings.\n\
                 pub fn with_config(api_key: &str, config: crate::Config) -> crate::Result<Self> {{\n\
                     Ok(Self {{ transport: crate::transport::Transport::new(api_key, BASE_URL, config)? }})\n\
                 }}\n\
             \n\
             {methods}}}\n\
             \n\
             {types}\
             {extra}",
            title = spec.title,
            file = spec.name,
            key = spec.key,
            client = spec.client,
            types = self.types,
            extra = spec.extra,
        )?;
        Ok(out)
    }
}

// --------------------------------------------------------------- helpers

/// Strips nullable wrappers: `nullable`, `type: [T, "null"]`, and single-item
/// `oneOf`/`anyOf`/`allOf`. Returns the inner schema and whether it is nullable.
fn unwrap_nullable(schema: &Value) -> (Value, bool) {
    let mut nullable = schema.get("nullable") == Some(&Value::Bool(true));
    if let Some(Value::Array(types)) = schema.get("type") {
        let rest: Vec<&Value> = types
            .iter()
            .filter(|t| t.as_str() != Some("null"))
            .collect();
        if rest.len() < types.len() || types.is_empty() {
            nullable = true;
        }
        let mut inner = schema.clone();
        match rest.as_slice() {
            [one] => inner["type"] = (*one).clone(),
            _ => {
                inner.as_object_mut().map(|m| m.remove("type"));
            }
        }
        return (inner, nullable);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(items)) = schema.get(key) {
            if is_const_union(items) {
                return (schema.clone(), nullable);
            }
            let rest: Vec<&Value> = items.iter().filter(|i| !is_null_schema(i)).collect();
            if rest.len() < items.len() {
                nullable = true;
            }
            if rest.len() == 1 && schema.get("properties").is_none() {
                let (inner, inner_nullable) = unwrap_nullable(rest[0]);
                return (inner, nullable || inner_nullable);
            }
        }
    }
    if let Some(Value::Array(items)) = schema.get("allOf")
        && items.len() == 1
        && schema.get("properties").is_none()
    {
        let (inner, inner_nullable) = unwrap_nullable(&items[0]);
        return (inner, nullable || inner_nullable);
    }
    (schema.clone(), nullable)
}

fn is_null_schema(s: &Value) -> bool {
    match s.get("type") {
        Some(Value::String(t)) => t == "null",
        Some(Value::Array(t)) => t.is_empty() || t.iter().all(|t| t == "null"),
        None => s.as_object().is_some_and(|m| {
            m.keys().all(|k| k == "nullable" || k == "description") && m.contains_key("nullable")
        }),
        _ => false,
    }
}

/// `oneOf: [{enum: [0], title: "A"}, {enum: [1], title: "B"}]` style enums.
fn is_const_union(items: &[Value]) -> bool {
    items.len() > 1
        && items.iter().all(|i| {
            i.get("const").is_some()
                || i.get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|e| e.len() == 1)
        })
        && items.iter().all(|i| i.get("properties").is_none())
}

fn is_enum(s: &Value) -> bool {
    s.get("enum").is_some()
        || ["oneOf", "anyOf"].iter().any(|k| {
            s.get(*k)
                .and_then(Value::as_array)
                .is_some_and(|i| is_const_union(i))
        })
}

fn is_struct(s: &Value) -> bool {
    s.get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty())
        || s.get("allOf")
            .and_then(Value::as_array)
            .is_some_and(|a| a.len() > 1)
}

#[derive(PartialEq)]
enum EnumKind {
    Int,
    Str,
    Mixed,
}

/// `(value, label, doc)` for each enum value, null excluded.
fn enum_variants(s: &Value) -> Vec<(Value, String, Option<String>)> {
    let mut out = Vec::new();
    if let Some(Value::Array(values)) = s.get("enum") {
        let names = enum_names(s);
        for (i, value) in values.iter().enumerate() {
            if value.is_null() {
                continue;
            }
            let label = names
                .as_ref()
                .and_then(|n| n.get(i).cloned())
                .unwrap_or_else(|| match value {
                    Value::String(v) => v.clone(),
                    other => format!("V{other}").replace('-', "Neg"),
                });
            out.push((value.clone(), label, None));
        }
    } else {
        let items = s
            .get("oneOf")
            .or_else(|| s.get("anyOf"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in items {
            let value = item
                .get("const")
                .cloned()
                .or_else(|| item.pointer("/enum/0").cloned())
                .unwrap_or(Value::Null);
            if value.is_null() {
                continue;
            }
            let label = item
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| match &value {
                    Value::String(v) => v.clone(),
                    other => format!("V{other}").replace('-', "Neg"),
                });
            let doc = item
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            out.push((value, label, doc));
        }
    }
    out
}

fn enum_kind(s: &Value) -> EnumKind {
    let values: Vec<Value> = enum_variants(s).into_iter().map(|v| v.0).collect();
    if !values.is_empty() && values.iter().all(|v| v.is_i64()) {
        EnumKind::Int
    } else if !values.is_empty() && values.iter().all(Value::is_string) {
        EnumKind::Str
    } else {
        EnumKind::Mixed
    }
}

/// Variant names for an `enum` list, if the spec provides them.
fn enum_names(s: &Value) -> Option<Vec<String>> {
    for key in ["x-enumNames", "x-enum-varnames"] {
        if let Some(Value::Array(names)) = s.get(key) {
            return Some(
                names
                    .iter()
                    .map(|n| n.as_str().unwrap_or_default().to_owned())
                    .collect(),
            );
        }
    }
    if s.get("oneOf").is_some() || s.get("anyOf").is_some() {
        return Some(enum_variants(s).into_iter().map(|v| v.1).collect());
    }
    if s.get("type").and_then(Value::as_str) == Some("string") {
        return None; // string values are their own names
    }
    // "0 = None\n1 = Allow" in the description.
    let values = s.get("enum").and_then(Value::as_array)?;
    let mut by_value = HashMap::new();
    for line in description(s).unwrap_or_default().lines() {
        if let Some((v, n)) = line.split_once('=')
            && let Ok(v) = v.trim().parse::<i64>()
        {
            by_value.insert(v, n.trim().to_owned());
        }
    }
    let names: Option<Vec<String>> = values
        .iter()
        .filter(|v| !v.is_null())
        .map(|v| v.as_i64().and_then(|v| by_value.get(&v).cloned()))
        .collect();
    names
}

/// A Rust literal for a scalar schema's `default`, or for an integer's
/// positive `minimum` (where the type's zero default would be invalid).
fn default_literal(s: &Value) -> Option<String> {
    match (s.get("type").and_then(Value::as_str), s.get("default")) {
        (Some("string"), Some(Value::String(v))) => Some(format!("{v:?}.into()")),
        (Some("boolean"), Some(Value::Bool(v))) => Some(v.to_string()),
        (Some("integer"), Some(Value::Number(v))) if v.is_i64() || v.is_u64() => {
            Some(v.to_string())
        }
        (Some("number"), Some(Value::Number(v))) => Some(format!("{:?}", v.as_f64()?)),
        (Some("integer"), None) => s
            .get("minimum")
            .and_then(Value::as_i64)
            .filter(|min| *min > 0)
            .map(|min| min.to_string()),
        _ => None,
    }
}

fn int_type(s: &Value) -> &'static str {
    match s.get("format").and_then(Value::as_str) {
        Some("int32") => "i32",
        Some("byte") => "u8",
        Some("uint32" | "uint") => "u32",
        Some("uint64") => "u64",
        _ => "i64",
    }
}

/// The schema of the JSON media type in a `content` map, if any.
fn json_content(content: &Map<String, Value>) -> Option<Value> {
    let key = content
        .keys()
        .find(|k| k.as_str() == "application/json")
        .or_else(|| content.keys().find(|k| k.contains("json")))?;
    Some(
        content[key]
            .get("schema")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    )
}

fn description(s: &Value) -> Option<&str> {
    s.get("description")
        .and_then(Value::as_str)
        .filter(|d| !d.trim().is_empty())
}

/// Renders text as `///` lines. Leading indentation is removed (it would turn
/// into doctests) and bare code fences are marked as `text`.
fn doc_comment(text: Option<&str>, indent: &str) -> String {
    let Some(text) = text else {
        return String::new();
    };
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.trim().lines() {
        let mut line = line.trim().to_owned();
        if line.starts_with("```") {
            if !in_fence && line == "```" {
                line = "```text".into();
            }
            in_fence = !in_fence;
        }
        if line.is_empty() {
            writeln!(out, "{indent}///").unwrap();
        } else {
            writeln!(out, "{indent}/// {line}").unwrap();
        }
    }
    if in_fence {
        writeln!(out, "{indent}/// ```").unwrap();
    }
    out
}

fn type_ident(raw: &str) -> String {
    let name = raw.to_upper_camel_case();
    let name: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
    if name.is_empty() {
        "Unnamed".into()
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("T{name}")
    } else {
        name
    }
}

fn field_ident(raw: &str) -> String {
    let name: String = raw
        .to_snake_case()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let name = if name.is_empty() {
        "value".to_owned()
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("n{name}")
    } else {
        name
    };
    match name.as_str() {
        "self" | "super" | "crate" => format!("{name}_"),
        n if KEYWORDS.contains(&n) => format!("r#{name}"),
        _ => name,
    }
}

fn variant_ident(raw: &str) -> String {
    let name = type_ident(raw);
    match name.as_str() {
        "Unnamed" => "Empty".into(),
        "Self" => "SelfValue".into(),
        // `Other` is the catch-all variant added by the enum macros.
        "Other" => "OtherValue".into(),
        n if n.starts_with('T') && raw.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("V{}", &n[1..])
        }
        _ => name,
    }
}
