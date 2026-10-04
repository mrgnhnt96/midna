//! OpenRPC document generated from the catalog (`rpc.discover`, `midna schema`).
use crate::catalog::{MethodSpec, catalog};
use serde_json::{Map, Value, json};

/// Move `$defs` out of `schema` into `defs`, rewriting `#/$defs/X` refs to components refs.
fn hoist(mut schema: Value, defs: &mut Map<String, Value>) -> Value {
    if let Some(Value::Object(d)) = schema.as_object_mut().and_then(|o| o.remove("$defs")) {
        for (k, v) in d {
            let v = rewrite_refs(v);
            defs.insert(k, v);
        }
    }
    if let Some(o) = schema.as_object_mut() {
        o.remove("$schema");
    }
    rewrite_refs(schema)
}

fn rewrite_refs(v: Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| match (k.as_str(), &v) {
                    ("$ref", Value::String(s)) => (k, json!(s.replace("#/$defs/", "#/components/schemas/"))),
                    _ => (k, rewrite_refs(v)),
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(rewrite_refs).collect()),
        v => v,
    }
}

fn method_doc(m: &MethodSpec, defs: &mut Map<String, Value>) -> Value {
    let params = hoist((m.params)(), defs);
    let required: Vec<String> = params
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut descriptors = vec![];
    if let Some(props) = params.get("properties").and_then(Value::as_object) {
        for (name, schema) in props {
            let mut d = json!({ "name": name, "required": required.contains(name), "schema": schema });
            if let Some(desc) = schema.get("description") {
                d["description"] = desc.clone();
            }
            descriptors.push(d);
        }
    }
    let result = hoist((m.result)(), defs);
    json!({
        "name": m.name,
        "description": m.description,
        "paramStructure": "by-name",
        "params": descriptors,
        "result": { "name": "result", "schema": result },
        "x-mutating": m.mutating,
        "x-human-only": m.human_only,
        "x-not-implemented": m.stub,
    })
}

pub fn openrpc() -> Value {
    let mut defs = Map::new();
    let methods: Vec<Value> = catalog().iter().map(|m| method_doc(m, &mut defs)).collect();
    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "midna",
            "version": crate::VERSION,
            "description": "midna daemon API: newline-delimited JSON-RPC 2.0 over the Unix socket at $MIDNA_SOCKET \
                (default $MIDNA_HOME/midnad.sock). Agents: params may include `caller: {session}` (the CLI adds MIDNA_SESSION). \
                Errors: -32601 unknown method, -32602 bad params, 1 refused by policy, 2 human only, 3 not found, 4 conflict.",
        },
        "methods": methods,
        "components": { "schemas": Value::Object(defs) },
    })
}

#[cfg(test)]
mod tests {
    use crate::types::*;
    use serde_json::json;

    #[test]
    fn openrpc_lists_every_method_and_resolves_refs() {
        let doc = super::openrpc();
        let methods = doc["methods"].as_array().unwrap();
        assert_eq!(methods.len(), crate::catalog().len());
        let text = doc.to_string();
        assert!(!text.contains("#/$defs/"));
        // every components ref target exists
        for part in text.split("#/components/schemas/").skip(1) {
            let name: String = part.chars().take_while(|c| *c != '"').collect();
            assert!(doc["components"]["schemas"].get(&name).is_some(), "missing {name}");
        }
        let names: std::collections::HashSet<_> = methods.iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), methods.len(), "duplicate method names");
    }

    #[test]
    fn wire_forms() {
        assert_eq!(serde_json::to_value(RuleScope::Project("p_1".into())).unwrap(), json!({"kind":"project","id":"p_1"}));
        assert_eq!(serde_json::to_value(RuleScope::Global).unwrap(), json!({"kind":"global"}));
        let r = Resolution::Approve { scope: ApprovalScope::Minutes { minutes: 5 } };
        assert_eq!(serde_json::to_value(&r).unwrap(), json!({"kind":"approve","scope":{"kind":"minutes","minutes":5}}));
        assert_eq!(serde_json::to_value(StatusState::NeedsYou).unwrap(), json!("needs_you"));
    }
}
