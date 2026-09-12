use crate::{Error, Result};
use serde_json::{Value, json};

// Match the original atob validation without decoding or changing stored data.
// Its forgiving-base64 grammar ignores five ASCII whitespace characters,
// accepts nonzero unused bits, and permits padding only for complete quartets.
fn valid_base64(encoded: &str) -> bool {
    let mut data: Vec<u8> = encoded
        .bytes()
        .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c))
        .collect();
    if data.len().is_multiple_of(4) && data.last() == Some(&b'=') {
        data.pop();
        if data.last() == Some(&b'=') {
            data.pop();
        }
    }
    data.len() % 4 != 1
        && data
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

const STYLES: [&str; 3] = ["unspecified", "inline", "attachment"];
const STYLE_EXPECTED: &str = "'unspecified' | 'inline' | 'attachment'";
// Bound diagnostic amplification as well as stored bytes. This intentional
// resource limit is narrower than the original unbounded schema diagnostics.
const MAX_SCHEMA_ISSUES: usize = 1024;
fn bounded(value: &Value) -> Result<()> {
    if value.to_string().len() > crate::protocol::MAX_FRAME {
        Err(Error::invalid("Clipboard exceeds byte limit"))
    } else {
        Ok(())
    }
}
fn value_type(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}
#[derive(serde::Serialize)]
#[serde(untagged)]
enum SchemaIssue {
    Type {
        code: &'static str,
        expected: &'static str,
        received: &'static str,
        path: Vec<Value>,
        message: String,
    },
    EnumType {
        expected: &'static str,
        received: &'static str,
        code: &'static str,
        path: Vec<Value>,
        message: String,
    },
    EnumValue {
        received: String,
        code: &'static str,
        options: [&'static str; 3],
        path: Vec<Value>,
        message: String,
    },
    Exclusive {
        code: &'static str,
        message: &'static str,
        path: Vec<Value>,
    },
}
impl SchemaIssue {
    fn invalid_type(expected: &'static str, value: Option<&Value>, path: Vec<Value>) -> Self {
        let received = value_type(value);
        Self::Type {
            code: "invalid_type",
            expected,
            received,
            path,
            message: if value.is_none() {
                "Required".into()
            } else {
                format!("Expected {expected}, received {received}")
            },
        }
    }
}
fn issue_bound(issues: &[SchemaIssue]) -> Result<()> {
    if issues.len() > MAX_SCHEMA_ISSUES {
        Err(Error::invalid("Clipboard validation exceeds issue limit"))
    } else {
        Ok(())
    }
}
fn schema_result(issues: Vec<SchemaIssue>) -> Result<()> {
    issue_bound(&issues)?;
    if issues.is_empty() {
        Ok(())
    } else {
        Err(Error::invalid(serde_json::to_string_pretty(&issues)?))
    }
}

/// Process-owned clipboard; no synchronization with the user's OS clipboard.
#[derive(Default)]
pub(super) struct Clipboard {
    pub items: Vec<Value>,
}
impl Clipboard {
    pub fn validate(value: &Value) -> Result<Vec<Value>> {
        Self::validate_store(value, "navigator.clipboard.write")
    }
    fn validate_store(value: &Value, operation: &str) -> Result<Vec<Value>> {
        let invalid = |reason: &str| Error::invalid(format!("{operation} requires {reason}"));
        let items = value
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or_else(|| invalid("items"))?;
        bounded(value)?;
        items
            .iter()
            .map(|item| {
                let entries = item
                    .get("entries")
                    .and_then(Value::as_array)
                    .filter(|entries| !entries.is_empty())
                    .ok_or_else(|| invalid("clipboard item entries"))?;
                let style = item
                    .get("presentationStyle")
                    .filter(|style| !style.is_null());
                if style.is_some_and(|style| {
                    !style.as_str().is_some_and(|style| STYLES.contains(&style))
                }) {
                    return Err(invalid("a valid presentation_style"));
                }
                let entries = entries
                    .iter()
                    .map(|entry| {
                        let mime = entry
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .filter(|mime| !mime.is_empty())
                            .ok_or_else(|| invalid("entry mime_type"))?;
                        let text = entry.get("text").and_then(Value::as_str);
                        let binary = entry.get("base64").and_then(Value::as_str);
                        match (text, binary) {
                            (Some(text), None) => Ok(json!({"mimeType":mime,"text":text})),
                            (None, Some(encoded)) => {
                                if !valid_base64(encoded) {
                                    return Err(invalid("valid base64 entry data"));
                                }
                                Ok(json!({"mimeType":mime,"base64":encoded}))
                            }
                            _ => Err(invalid("exactly one of entry text or base64")),
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut result = json!({"entries":entries});
                if let Some(style) = style {
                    result["presentationStyle"] = style.clone();
                }
                Ok(result)
            })
            .collect()
    }
    // Public command schemas run over the entire payload before the store's
    // content checks. Page binding writes retain the distinct store domain.
    pub fn validate_public(value: Option<&Value>) -> Result<Vec<Value>> {
        if let Some(value) = value {
            bounded(value)?;
        }
        let mut issues = Vec::new();
        if let Some(items) = value.and_then(Value::as_array) {
            for (item_index, item) in items.iter().enumerate() {
                issue_bound(&issues)?;
                let path = vec![json!("items"), json!(item_index)];
                if !item.is_object() {
                    issues.push(SchemaIssue::invalid_type("object", Some(item), path));
                    continue;
                }
                let mut entry_path = path.clone();
                entry_path.push(json!("entries"));
                if let Some(entries) = item.get("entries").and_then(Value::as_array) {
                    for (entry_index, entry) in entries.iter().enumerate() {
                        issue_bound(&issues)?;
                        let mut path = entry_path.clone();
                        path.push(json!(entry_index));
                        if !entry.is_object() {
                            issues.push(SchemaIssue::invalid_type("object", Some(entry), path));
                            continue;
                        }
                        let before = issues.len();
                        for (field, original, optional) in [
                            ("mimeType", "mime_type", false),
                            ("text", "text", true),
                            ("base64", "base64", true),
                        ] {
                            let value = entry.get(field);
                            if (!optional || value.is_some())
                                && !value.is_some_and(Value::is_string)
                            {
                                let mut field_path = path.clone();
                                field_path.push(json!(original));
                                issues.push(SchemaIssue::invalid_type("string", value, field_path));
                            }
                        }
                        if issues.len() == before
                            && entry.get("text").is_some() == entry.get("base64").is_some()
                        {
                            issues.push(SchemaIssue::Exclusive {
                                code: "custom",
                                message: "Clipboard entries must set exactly one of text or base64",
                                path,
                            });
                        }
                    }
                } else {
                    issues.push(SchemaIssue::invalid_type(
                        "array",
                        item.get("entries"),
                        entry_path,
                    ));
                }
                if let Some(style) = item.get("presentationStyle") {
                    let mut path = path;
                    path.push(json!("presentation_style"));
                    match style.as_str() {
                        Some(style) if STYLES.contains(&style) => (),
                        Some(style) => issues.push(SchemaIssue::EnumValue {
                            received: style.into(),
                            code: "invalid_enum_value",
                            options: STYLES,
                            path,
                            message: format!(
                                "Invalid enum value. Expected {STYLE_EXPECTED}, received '{style}'"
                            ),
                        }),
                        None => issues.push(SchemaIssue::EnumType {
                            expected: STYLE_EXPECTED,
                            received: value_type(Some(style)),
                            code: "invalid_type",
                            path,
                            message: format!(
                                "Expected {STYLE_EXPECTED}, received {}",
                                value_type(Some(style))
                            ),
                        }),
                    }
                }
            }
        } else {
            issues.push(SchemaIssue::invalid_type(
                "array",
                value,
                vec![json!("items")],
            ));
        }
        schema_result(issues)?;
        Self::validate_store(
            value.expect("schema validated items"),
            "tab_clipboard_write",
        )
    }
    pub fn validate_text(value: Option<&Value>) -> Result<Vec<Value>> {
        if !value.is_some_and(Value::is_string) {
            schema_result(vec![SchemaIssue::invalid_type(
                "string",
                value,
                vec![json!("text")],
            )])?;
        }
        let items = json!([{"presentationStyle":"unspecified","entries":[{"mimeType":"text/plain","text":value.unwrap()}]}]);
        Self::validate_store(&items, "tab_clipboard_write_text")
    }
    pub fn write(&mut self, value: &Value) -> Result<()> {
        let validated = Self::validate(value)?;
        self.items = validated;
        Ok(())
    }
    pub fn text(&self) -> String {
        self.items
            .iter()
            .flat_map(|i| i["entries"].as_array().into_iter().flatten())
            .find(|e| e["mimeType"] == "text/plain")
            .and_then(|e| e["text"].as_str())
            .unwrap_or("")
            .into()
    }
}

pub(super) const INSTALL: &str = r#"(() => {
 const marker='__skyre_clipboard_state';
 if(globalThis[marker])return true;
 const pending=new Map();let next=0;
 const request=(op,items)=>new Promise((resolve,reject)=>{const id=++next;pending.set(id,{resolve,reject});try{globalThis.__skyre_clipboard(JSON.stringify({id,op,items}));}catch(e){pending.delete(id);reject(e);}});
 const original=Object.getOwnPropertyDescriptor(navigator,'clipboard');
 const clipboard={
  async readText(){const items=await request('read');for(const item of items)for(const e of item.entries)if(e.mimeType==='text/plain')return e.text??await new Blob([Uint8Array.from(atob(e.base64),c=>c.charCodeAt(0))]).text();return '';},
  async writeText(text){if(!arguments.length)throw new TypeError('Missing text');await request('write',[{entries:[{mimeType:'text/plain',text:String(text)}]}]);},
  async read(){const items=await request('read');return items.map(item=>{const data=Object.fromEntries(item.entries.map(e=>[e.mimeType,new Blob([e.text??Uint8Array.from(atob(e.base64),c=>c.charCodeAt(0))],{type:e.mimeType})]));return typeof ClipboardItem==='function'?new ClipboardItem(data,{presentationStyle:item.presentationStyle??'unspecified'}):{types:Object.keys(data),presentationStyle:item.presentationStyle??'unspecified',getType:async t=>{if(!data[t])throw new DOMException('Type missing','NotFoundError');return data[t];}};});},
  async write(items){if(!arguments.length)throw new TypeError('Missing items');const out=[];for(const item of items){const entries=[];for(const mimeType of item.types){const blob=await item.getType(mimeType);if(mimeType.startsWith('text/'))entries.push({mimeType,text:await blob.text()});else{const bytes=new Uint8Array(await blob.arrayBuffer());let binary='';for(let i=0;i<bytes.length;i+=32768)binary+=String.fromCharCode(...bytes.subarray(i,i+32768));entries.push({mimeType,base64:btoa(binary)});}}out.push({presentationStyle:item.presentationStyle,entries});}if(out.length)await request('write',out);}
 };
 const getter=()=>clipboard;
 Object.defineProperty(navigator,'clipboard',{configurable:true,get:getter});
 globalThis[marker]={respond(response){const p=pending.get(response.id);if(!p)return;pending.delete(response.id);if(response.ok)p.resolve(response.items??[]);else p.reject(new Error(response.error??'Clipboard request failed'));},cleanup(){for(const p of pending.values())p.reject(new Error('Clipboard bridge closed'));pending.clear();if(Object.getOwnPropertyDescriptor(navigator,'clipboard')?.get===getter){if(original)Object.defineProperty(navigator,'clipboard',original);else delete navigator.clipboard;}if(globalThis[marker]===this)delete globalThis[marker];}};
 return true;
})()"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_base64_matches_original_forgiving_decode_validation() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../tests/oracles/browser_clipboard_base64.json"
        ))
        .unwrap();
        let mut clipboard = Clipboard::default();
        let baseline = json!([{"entries":[{"mimeType":"text/plain","text":"unchanged"}]}]);
        for case in oracle["cases"].as_array().unwrap() {
            clipboard.write(&baseline).unwrap();
            let items = json!([{"entries":[{"mimeType":"application/octet-stream","base64":case["input"]}]}]);
            let result = clipboard.write(&items);
            assert_eq!(
                result.is_ok(),
                case["accepted"].as_bool().unwrap(),
                "{case}"
            );
            assert_eq!(
                clipboard.items,
                if result.is_ok() {
                    items
                } else {
                    baseline.clone()
                }
                .as_array()
                .unwrap()
                .clone(),
                "{case}"
            );
        }
    }
    #[test]
    fn clipboard_store_and_public_schema_match_original_transactional_matrix() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../tests/oracles/browser_clipboard_store.json"
        ))
        .unwrap();
        let baseline = json!([{"presentationStyle":"inline","entries":[{"mimeType":"text/plain","text":"baseline"},{"mimeType":"application/octet-stream","base64":"AB=="}]}]);
        for case in oracle["cases"].as_array().unwrap() {
            for mode in ["store", "public"] {
                let mut clipboard = Clipboard::default();
                clipboard.write(&baseline).unwrap();
                let mut input = case["items"].clone();
                let result = if mode == "store" {
                    clipboard.write(&input)
                } else {
                    Clipboard::validate_public(Some(&input)).map(|items| {
                        clipboard.items = items;
                    })
                };
                let expected = &case["results"][mode];
                assert_eq!(
                    result.is_ok(),
                    expected["accepted"],
                    "{} {mode}",
                    case["id"]
                );
                assert_eq!(
                    result.err().map(|error| error.message),
                    expected["error"].as_str().map(str::to_owned),
                    "{} {mode}",
                    case["id"]
                );
                assert_eq!(
                    json!(clipboard.items),
                    expected["items"],
                    "{} {mode}",
                    case["id"]
                );
                assert_eq!(clipboard.text(), expected["text"], "{} {mode}", case["id"]);
                if let Some(entry) = input
                    .pointer_mut("/0/entries/0")
                    .and_then(Value::as_object_mut)
                {
                    entry.insert("text".into(), json!("input mutation"));
                }
                let mut read = json!(clipboard.items);
                read[0]["entries"][0]["text"] = json!("read mutation");
                read[0]["presentationStyle"] = json!("attachment");
                read.as_array_mut().unwrap().clear();
                assert_eq!(
                    json!(clipboard.items),
                    expected["items"],
                    "{} {mode} copies",
                    case["id"]
                );
            }
        }
        for case in oracle["textCases"].as_array().unwrap() {
            let result = Clipboard::validate_text(Some(&case["text"]));
            assert_eq!(result.is_ok(), case["accepted"], "text {}", case["id"]);
            match result {
                Ok(items) => assert_eq!(json!(items), case["items"]),
                Err(error) => assert_eq!(error.message, case["error"]),
            }
        }
    }
    #[test]
    fn clipboard_public_resource_rejections_preserve_the_store() {
        let mut clipboard = Clipboard::default();
        let baseline = json!([{"entries":[{"mimeType":"text/plain","text":"baseline"}]}]);
        clipboard.write(&baseline).unwrap();
        let oversized = json!([{"entries":[{"mimeType":"text/plain","text":"x".repeat(crate::protocol::MAX_FRAME)}]}]);
        assert_eq!(
            Clipboard::validate_public(Some(&oversized))
                .unwrap_err()
                .message,
            "Clipboard exceeds byte limit"
        );
        assert_eq!(
            Clipboard::validate_text(Some(&json!("x".repeat(crate::protocol::MAX_FRAME))))
                .unwrap_err()
                .message,
            "Clipboard exceeds byte limit"
        );
        let many_errors = json!([{"entries":vec![Value::Null; MAX_SCHEMA_ISSUES + 1]}]);
        assert_eq!(
            Clipboard::validate_public(Some(&many_errors))
                .unwrap_err()
                .message,
            "Clipboard validation exceeds issue limit"
        );
        assert_eq!(json!(clipboard.items), baseline);
        clipboard
            .write(&json!([{"entries":[{"mimeType":"text/plain","text":"after rejection"}]}]))
            .unwrap();
        assert_eq!(clipboard.text(), "after rejection");
    }
    #[test]
    fn writes_are_transactional_and_reads_keep_text_binary_distinction() {
        let mut c = Clipboard::default();
        c.write(&json!([{"entries":[{"mimeType":"text/plain","text":"baseline"}]}]))
            .unwrap();
        assert!(
            c.write(&json!([{"entries":[{"mimeType":"text/plain","text":"bad"}]},{"entries":[]}]))
                .is_err()
        );
        assert_eq!(c.text(), "baseline");
        c.write(
            &json!([{"extra":"discard","entries":[{"mimeType":"text/plain","base64":"zrE="}]}]),
        )
        .unwrap();
        assert_eq!(c.text(), "");
        assert!(c.items[0].get("extra").is_none());
    }
    #[test]
    fn page_bridge_round_trip_and_cleanup_preserve_later_writer() {
        let rt = rquickjs::Runtime::new().unwrap();
        let cx = rquickjs::Context::full(&rt).unwrap();
        cx.with(|ctx|{
            ctx.eval::<(),_>("globalThis.navigator={};globalThis.requests=[];globalThis.__skyre_clipboard=x=>requests.push(JSON.parse(x));").unwrap();assert!(ctx.eval::<bool,_>(INSTALL).unwrap());
            ctx.eval::<(),_>("globalThis.pending=navigator.clipboard.readText().then(x=>globalThis.received=x);globalThis.__skyre_clipboard_state.respond({id:1,ok:true,items:[{entries:[{mimeType:'text/plain',text:'αβ'}]}]});").unwrap();
            while ctx.execute_pending_job(){}assert_eq!(ctx.eval::<String,_>("received").unwrap(),"αβ");
            ctx.eval::<(),_>("Object.defineProperty(navigator,'clipboard',{configurable:true,value:{later:true}});globalThis.__skyre_clipboard_state.cleanup();").unwrap();assert!(ctx.eval::<bool,_>("navigator.clipboard.later").unwrap());
        });
    }
}
