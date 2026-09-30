// Portable adapter derived from Obscura crates/obscura-js/src/ops.rs.
// Copyright the Obscura contributors. Licensed under Apache-2.0; see LICENSE.
// Modified: V8/render/network state replaced by per-document portable state.
use wasm_bindgen::prelude::*;
use std::cell::{RefCell, Cell};
use std::collections::HashSet;
use obscura_dom::{DomTree, NodeData, NodeId, AttachShadowError, ShadowRootMode};
mod write_stream;
use write_stream::DocumentWriteStream;

struct PageState {
    dom: Option<DomTree>,
    url: String,
    referrer: String,
    encoding: String,
    already_started_scripts: RefCell<HashSet<NodeId>>,
    write_stream: RefCell<Option<DocumentWriteStream>>,
    document_write_inserted_script: Cell<bool>,
    lifecycle: RefCell<Vec<String>>,
}

#[wasm_bindgen]
pub struct WasmDom { state: PageState }

#[wasm_bindgen]
impl WasmDom {
    #[wasm_bindgen(constructor)]
    pub fn new(html: &str, url: &str) -> WasmDom {
        WasmDom { state: PageState {
            dom: Some(obscura_dom::parse_html(html)),
            url: url.into(), referrer: String::new(), encoding: "UTF-8".into(),
            already_started_scripts: RefCell::new(HashSet::new()),
            write_stream: RefCell::new(None),
            document_write_inserted_script: Cell::new(false),
            lifecycle: RefCell::new(Vec::new()),
        }}
    }

    /// JSON-text result, exactly as expected by bootstrap.js's op_dom wrapper.
    /// Unknown operations throw rather than pretending to implement them.
    pub fn command(&self, cmd: String, arg1: String, arg2: String) -> Result<String, JsValue> {
        if !SUPPORTED_COMMANDS.contains(&cmd.as_str()) {
            return Err(JsValue::from_str(&format!("Unsupported portable DOM command: {}", cmd)));
        }
        if cmd == "document_lifecycle" {
            if matches!(arg1.as_str(), "init" | "DOMContentLoaded" | "load") {
                let mut events = self.state.lifecycle.borrow_mut();
                if events.len() >= 1024 { events.remove(0); }
                events.push(arg1);
            }
            return Ok("true".into());
        }
        Ok(op_dom_inner(&self.state, cmd, arg1, arg2))
    }

    pub fn supported_commands(&self) -> String {
        serde_json::to_string(SUPPORTED_COMMANDS).unwrap()
    }

    pub fn lifecycle_events(&self) -> String {
        serde_json::to_string(&*self.state.lifecycle.borrow()).unwrap()
    }

    /// Used by the bootstrap's op_script_mark_started / op_script_try_start.
    pub fn script_mark_started(&self, nid: u32) -> bool {
        let node = NodeId::new(nid);
        if !node_is_script(self.state.dom.as_ref().unwrap(), node) { return false; }
        self.state.already_started_scripts.borrow_mut().insert(node);
        true
    }
    pub fn script_try_start(&self, nid: u32) -> bool {
        let node = NodeId::new(nid);
        if !node_is_script(self.state.dom.as_ref().unwrap(), node) { return false; }
        self.state.already_started_scripts.borrow_mut().insert(node)
    }
    /// Native shadow tree identity bridge used by bootstrap.js.
    pub fn shadow_attach(&self, host_nid: u32, mode: &str) -> i32 {
        let mode = match mode {
            "open" => ShadowRootMode::Open,
            "closed" => ShadowRootMode::Closed,
            _ => return -1,
        };
        match self.state.dom.as_ref().unwrap().attach_shadow_root(NodeId::new(host_nid), mode) {
            Ok(root) => root.raw() as i32,
            Err(AttachShadowError::HostAlreadyHasShadowRoot) => -2,
            Err(_) => -1,
        }
    }
    pub fn shadow_root_info(&self, host_nid: u32) -> String {
        let dom = self.state.dom.as_ref().unwrap();
        dom.shadow_root(NodeId::new(host_nid))
            .and_then(|root| dom.shadow_root_info(root))
            .map(|shadow| {
                let mode = match shadow.mode {
                    ShadowRootMode::Open => "open",
                    ShadowRootMode::Closed => "closed",
                };
                format!("{}\0{mode}", shadow.id.raw())
            }).unwrap_or_default()
    }
    pub fn set_url(&mut self, url: String) { self.state.url = url; }
    pub fn set_referrer(&mut self, referrer: String) { self.state.referrer = referrer; }
    pub fn serialize(&self) -> String {
        let dom = self.state.dom.as_ref().unwrap();
        dom.inner_html(dom.document())
    }
}

fn op_dom_inner(gs: &PageState, cmd: String, arg1: String, arg2: String) -> String {
    let dom = gs.dom.as_ref().unwrap();
    match cmd.as_str() {
        "get_form_state" => {
            let nid = NodeId::new(arg1.parse().unwrap_or(u32::MAX));
            dom.form_control_state(nid)
                .map(|control| {
                    serde_json::json!({
                        "value": control.value,
                        "checked": control.checked,
                        "indeterminate": control.indeterminate,
                    })
                    .to_string()
                })
                .unwrap_or_else(|| "null".to_string())
        }
        "set_form_value" | "set_form_checked" | "set_form_indeterminate" => {
            let nid = NodeId::new(arg1.parse().unwrap_or(u32::MAX));
            dom.update_form_control_state(nid, |control| match cmd.as_str() {
                "set_form_value" => control.value = Some(arg2.clone()),
                "set_form_checked" => control.checked = Some(arg2 == "true"),
                _ => control.indeterminate = arg2 == "true",
            });
            "null".to_string()
        }
        "document_node_id" => dom.document().index().to_string(),
        "document_title" => {
            // The DOM is authoritative after parsing. In particular, script
            // changes through title.textContent must be reflected by
            // document.title, not hidden behind the navigation-time snapshot.
            let title = dom
                .query_selector("title")
                .ok()
                .flatten()
                .map(|title_id| {
                    dom.text_content(title_id)
                        .split(|ch| matches!(ch, '\t' | '\n' | '\u{000C}' | '\r' | ' '))
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            serde_json::to_string(&title).unwrap_or("\"\"".into())
        }
        "document_url" => serde_json::to_string(&gs.url).unwrap_or("\"\"".into()),
        // The base for relative URLs. It differs from document_url exactly when the page carries
        // a <base href>, and that is the point: HTML resolves against the base, not the document.
        "document_base_url" => serde_json::to_string(
            &document_base_url(&gs).unwrap_or_else(|| gs.url.clone()),
        )
        .unwrap_or("\"\"".into()),
        // The unresolved attribute. After history.pushState only JS knows the URL, so only JS
        // can resolve a relative base against it.
        "document_base_href" => {
            serde_json::to_string(&document_base_href(&gs).unwrap_or_default())
                .unwrap_or("\"\"".into())
        }
        "document_referrer" => serde_json::to_string(&gs.referrer).unwrap_or("\"\"".into()),
        "document_encoding" => serde_json::to_string(&gs.encoding).unwrap_or("\"UTF-8\"".into()),
        "document_element" => {
            for cid in dom.children(dom.document()) {
                if let Some(n) = dom.get_node(cid) {
                    if n.as_element()
                        .map(|name| name.local.as_ref() == "html")
                        .unwrap_or(false)
                    {
                        return cid.index().to_string();
                    }
                }
            }
            "-1".into()
        }
        "document_doctype" => {
            for cid in dom.children(dom.document()) {
                if let Some(n) = dom.get_node(cid) {
                    if let obscura_dom::NodeData::Doctype {
                        name,
                        public_id,
                        system_id,
                    } = &n.data
                    {
                        return serde_json::json!({
                            "name": name,
                            "publicId": public_id,
                            "systemId": system_id,
                            "nodeId": cid.index(),
                        })
                        .to_string();
                    }
                }
            }
            "null".into()
        }
        "get_element_by_id" => {
            // Verify the indexed node is in the live document. The id_index is best-effort:
            // it only registers nodes at creation time and doesn't update on reparent, so
            // it can point to a detached clone while the live node is elsewhere in the tree.
            let doc = dom.document();
            let nid = dom.get_element_by_id(&arg1);
            let live = nid.filter(|&n| dom.ancestors(n).contains(&doc));
            match live {
                Some(n) => n.index().to_string(),
                None => {
                    // Fall back to full scan for the live document.
                    let sel = format!(
                        "[id=\"{}\"]",
                        arg1.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    dom.query_selector(&sel)
                        .ok()
                        .flatten()
                        .map(|id| id.index().to_string())
                        .unwrap_or("-1".into())
                }
            }
        }
        "query_selector" => dom
            .query_selector(&arg1)
            .ok()
            .flatten()
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "query_selector_all" => {
            let ids: Vec<i32> = dom
                .query_selector_all(&arg1)
                .ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect())
                .unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "query_selector_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            dom.query_selector_from(NodeId::new(root_nid), &arg2)
                .ok()
                .flatten()
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "query_selector_all_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .query_selector_all_from(NodeId::new(root_nid), &arg2)
                .ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect())
                .unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "matches_selector" => {
            let nid = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            dom.matches_selector(nid, &arg2)
                .unwrap_or(false)
                .to_string()
        }
        "node_type" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node(NodeId::new(nid), |n| match &n.data {
                NodeData::Document => "9",
                NodeData::Element { .. } => "1",
                NodeData::Text { .. } => "3",
                NodeData::Comment { .. } => "8",
                NodeData::Doctype { .. } => "10",
                NodeData::ProcessingInstruction { .. } => "7",
            })
            .unwrap_or("0")
            .into()
        }
        "node_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name: String = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::Document => "#document".to_string(),
                    NodeData::Element { name, .. } => name.local.as_ref().to_ascii_uppercase(),
                    NodeData::Text { .. } => "#text".to_string(),
                    NodeData::Comment { .. } => "#comment".to_string(),
                    NodeData::Doctype { name, .. } => name.clone(),
                    NodeData::ProcessingInstruction { target, .. } => target.clone(),
                })
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "text_content" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.text_content(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "parent_node" | "first_child" | "last_child" | "next_sibling" | "prev_sibling" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node(NodeId::new(nid), |n| match cmd.as_str() {
                "parent_node" => n.parent,
                "first_child" => n.first_child,
                "last_child" => n.last_child,
                "next_sibling" => n.next_sibling,
                "prev_sibling" => n.prev_sibling,
                _ => None,
            })
            .flatten()
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into())
        }
        "next_in_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.next_in_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        // Reverse document order within a subtree, for NodeIterator's backward
        // walk (which prunes nothing, so the whole step fits in the DOM layer).
        "prev_in_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.prev_in_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        // Step past a whole subtree rather than into it: NodeFilter.FILTER_REJECT
        // prunes the rejected node's descendants, unlike FILTER_SKIP.
        "next_after_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.next_after_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .children(NodeId::new(nid))
                .iter()
                .map(|id| id.index() as i32)
                .collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        // Nodes directly assigned to an HTML <slot> (named slot assignment; the
        // first same-name slot in the shadow tree wins). `null` when the node is
        // not an HTML slot inside a shadow tree, so JS can tell "no slot" from
        // "slot without assignments" and fall back to the slot's own children.
        "assigned_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            match dom.assigned_nodes(NodeId::new(nid)) {
                Some(ids) => {
                    let ids: Vec<i32> = ids.iter().map(|id| id.index() as i32).collect();
                    serde_json::to_string(&ids).unwrap_or("[]".into())
                }
                None => "null".into(),
            }
        }
        "tag_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| {
                        if name.ns == html5ever::ns!(html) {
                            name.local.as_ref().to_ascii_uppercase()
                        } else {
                            match &name.prefix {
                                Some(prefix) => format!("{}:{}", prefix, name.local),
                                None => name.local.to_string(),
                            }
                        }
                    })
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "local_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| name.local.to_string())
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        // The tree builder already assigns foreign content (an <svg>/<math>
        // subtree) its own namespace; expose it so JS does not have to guess
        // the namespace from the tag name.
        "namespace_uri" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ns = dom
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| name.ns.as_ref().to_string())
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&ns).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| {
                    n.get_attribute(&arg2).map(|s| s.to_string())
                })
                .flatten();
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "attribute_names" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let names: Vec<String> = dom
                .with_node(NodeId::new(nid), |n| {
                    n.attrs()
                        .map(|a| a.iter().map(|x| x.qualified_name()).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            serde_json::to_string(&names).unwrap_or("[]".into())
        }
        "set_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            if let Some((name, value)) = arg2.split_once('\0') {
                if name == "id" {
                    let old_id = dom
                        .with_node(node_id, |n| n.get_attribute("id").map(|s| s.to_string()))
                        .flatten();
                    dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
                    dom.update_id_index(node_id, old_id.as_deref(), Some(value));
                } else {
                    dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
                }
            }
            "true".into()
        }
        "inner_html" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.inner_html(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "outer_html" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.outer_html(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "append_child" => {
            // Reject if either nid failed to parse (was "undefined"/empty) — those
            // default to 0 which is the document root, and silently operating on it
            // corrupts the tree. Require both args to be valid positive integers.
            let parent = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let child = match arg2.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let parent = NodeId::new(parent);
            let child = NodeId::new(child);
            dom.append_child(parent, child);
            (dom.get_node(child).and_then(|node| node.parent) == Some(parent)).to_string()
        }
        "remove_child" => {
            let child = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let child = NodeId::new(child);
            let had_parent = dom
                .get_node(child)
                .is_some_and(|node| node.parent.is_some());
            dom.remove_child(child);
            (had_parent
                && dom
                    .get_node(child)
                    .is_some_and(|node| node.parent.is_none()))
            .to_string()
        }
        "insert_before" => {
            let new_node = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let ref_node = match arg2.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let ref_node = NodeId::new(ref_node);
            let new_node = NodeId::new(new_node);
            let expected_parent = dom.get_node(ref_node).and_then(|node| node.parent);
            dom.insert_before(ref_node, new_node);
            (expected_parent.is_some()
                && dom.get_node(new_node).and_then(|node| node.parent) == expected_parent)
                .to_string()
        }
        "remove_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            // Removing `id` must clear the id_index too, matching set_attribute /
            // remove_attribute_ns; otherwise getElementById keeps returning the
            // still-attached element (#1013).
            let old_id = if arg2 == "id" {
                dom.with_node(node_id, |n| n.get_attribute("id").map(str::to_owned))
                    .flatten()
            } else {
                None
            };
            dom.with_node_mut(node_id, |n| {
                if let NodeData::Element { attrs, .. } = &mut n.data {
                    attrs.retain(|a| !a.qualified_name_eq(&arg2));
                }
            });
            if arg2 == "id" {
                dom.update_id_index(node_id, old_id.as_deref(), None);
            }
            "true".into()
        }
        // Namespace-aware attribute ops. arg2 packs the pieces with a NUL:
        //   get/remove: "<namespace>\0<localName>"
        //   set:        "<namespace>\0<qualifiedName>\0<value>"
        "get_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let (ns, local) = arg2.split_once('\0').unwrap_or(("", arg2.as_str()));
            let val = dom
                .with_node(NodeId::new(nid), |n| {
                    n.get_attribute_ns(ns, local).map(|s| s.to_string())
                })
                .flatten();
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "set_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            let mut parts = arg2.splitn(3, '\0');
            let ns = parts.next().unwrap_or("");
            let qualified = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            if !qualified.is_empty() {
                let local = qualified
                    .split_once(':')
                    .map(|(_, local)| local)
                    .unwrap_or(qualified);
                if ns.is_empty() && local == "id" {
                    let old_id = dom
                        .with_node(node_id, |n| n.get_attribute("id").map(str::to_owned))
                        .flatten();
                    dom.with_node_mut(node_id, |n| {
                        n.set_attribute_ns(ns, qualified, value.to_string())
                    });
                    dom.update_id_index(node_id, old_id.as_deref(), Some(value));
                } else {
                    dom.with_node_mut(node_id, |n| {
                        n.set_attribute_ns(ns, qualified, value.to_string())
                    });
                }
            }
            "true".into()
        }
        "remove_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            let (ns, local) = arg2.split_once('\0').unwrap_or(("", arg2.as_str()));
            if ns.is_empty() && local == "id" {
                let old_id = dom
                    .with_node(node_id, |n| n.get_attribute("id").map(str::to_owned))
                    .flatten();
                dom.with_node_mut(node_id, |n| n.remove_attribute_ns(ns, local));
                dom.update_id_index(node_id, old_id.as_deref(), None);
            } else {
                dom.with_node_mut(node_id, |n| n.remove_attribute_ns(ns, local));
            }
            "true".into()
        }
        "set_inner_html" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                // nid=0 is the document root; never allow innerHTML to clear it.
                // nid parse failure (e.g. "undefined") also falls here.
                _ => return "false".into(),
            };
            let target = NodeId::new(nid);
            let children = dom.children(target);
            for child in children {
                dom.detach(child);
            }
            if !arg2.is_empty() {
                let context_name = dom
                    .with_node(target, |node| match &node.data {
                        NodeData::Element { name, .. } => Some(name.clone()),
                        _ => None,
                    })
                    .flatten();
                let fragment = match context_name {
                    Some(name) => obscura_dom::parse_fragment_with_context(&arg2, name),
                    None => obscura_dom::parse_fragment(&arg2),
                };
                let import_root = fragment.fragment_root();
                dom.import_children_from(target, &fragment, import_root);
                for child in dom.children(target) {
                    mark_script_subtree_started(&gs, child);
                }
            }
            "true".into()
        }
        "set_inner_html_context" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => return "false".into(),
            };
            let target = NodeId::new(nid);
            let (context_name, html) = fragment_context_and_html(&arg2);
            for child in dom.children(target) {
                dom.detach(child);
            }
            if !html.is_empty() {
                let fragment = obscura_dom::parse_fragment_with_context(html, context_name);
                let import_root = fragment.fragment_root();
                dom.import_children_from(target, &fragment, import_root);
                for child in dom.children(target) {
                    mark_script_subtree_started(&gs, child);
                }
            }
            "true".into()
        }
        // Range.createContextualFragment has a deliberately different script
        // policy from innerHTML: scripts remain eligible and are prepared when
        // the returned fragment is inserted into a connected document.
        "set_fragment_html_executable" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => return "false".into(),
            };
            let target = NodeId::new(nid);
            let (context_name, html) = fragment_context_and_html(&arg2);
            for child in dom.children(target) {
                dom.detach(child);
            }
            if !html.is_empty() {
                let fragment = obscura_dom::parse_fragment_with_context(html, context_name);
                let import_root = fragment.fragment_root();
                dom.import_children_from(target, &fragment, import_root);
            }
            "true".into()
        }
        // document.write() feeds the document's input stream, so the calls
        // share one parser and one tokenizer state. Returns the nodes that
        // became complete with this call, for the caller to run scripts among.
        // Returns [[parent, node], …], parents before children. A `parent` of 0 means the node
        // belongs at the insertion point, which the caller knows. Nothing is inserted here:
        // that must go through Node.appendChild on the JS side, because that call also reports
        // the mutation, registers window named access, and loads a written stylesheet.
        "document_write" => {
            let mut slot = gs.write_stream.borrow_mut();
            let stream = slot.get_or_insert_with(DocumentWriteStream::new);
            let placements = stream.write(&arg2, dom);
            if placements
                .iter()
                .any(|placement| node_is_script(dom, placement.node))
            {
                gs.document_write_inserted_script.set(true);
            }
            let pairs: Vec<[i32; 2]> = placements
                .iter()
                .map(|placement| {
                    [
                        placement.parent.map_or(0, |id| id.index() as i32),
                        placement.node.index() as i32,
                    ]
                })
                .collect();
            serde_json::to_string(&pairs).unwrap_or("[]".into())
        }
        // document.open() discards what the input stream holds and starts over.
        "document_write_reset" => {
            *gs.write_stream.borrow_mut() = None;
            gs.document_write_inserted_script.set(false);
            "true".into()
        }
        "set_text_content" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node_mut(NodeId::new(nid), |n| match &mut n.data {
                NodeData::Text { contents } => {
                    *contents = arg2.clone();
                }
                NodeData::Comment { contents } => {
                    *contents = arg2.clone();
                }
                NodeData::ProcessingInstruction { data, .. } => {
                    *data = arg2.clone();
                }
                _ => {}
            });
            "true".into()
        }
        // A <template>'s children live in a separate contents document, so this
        // is the only route to them from JS. Allocates one on demand for
        // templates built via createElement.
        "template_contents" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.template_contents(NodeId::new(nid))
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "create_document_fragment" => dom.new_node(NodeData::Document).index().to_string(),
        "clone_node" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "-1".into(),
            };
            let source = NodeId::new(nid);
            match dom.clone_node(source, arg2 == "true") {
                Some(cloned) => {
                    propagate_script_start_state(dom, source, cloned, &gs.already_started_scripts);
                    cloned.index().to_string()
                }
                None => "-1".into(),
            }
        }
        "create_element" => dom
            .new_node(NodeData::Element {
                name: html5ever::QualName::new(
                    None,
                    html5ever::ns!(html),
                    html5ever::LocalName::from(arg1.as_str()),
                ),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
            .index()
            .to_string(),
        "create_element_ns" => {
            let (namespace, qualified) = arg1.split_once('\0').unwrap_or(("", arg1.as_str()));
            let (prefix, local) = match qualified.split_once(':') {
                Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
                    (Some(html5ever::Prefix::from(prefix)), local)
                }
                None if !qualified.is_empty() => (None, qualified),
                _ => return "-1".into(),
            };
            dom.new_node(NodeData::Element {
                name: html5ever::QualName::new(
                    prefix,
                    html5ever::Namespace::from(namespace),
                    html5ever::LocalName::from(local),
                ),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
            .index()
            .to_string()
        }
        "create_text_node" => dom
            .new_node(NodeData::Text {
                contents: arg1.clone(),
            })
            .index()
            .to_string(),
        "create_comment_node" => dom
            .new_node(NodeData::Comment {
                contents: arg1.clone(),
            })
            .index()
            .to_string(),
        "create_processing_instruction" => {
            // arg1 = target, arg2 = data
            dom.new_node(NodeData::ProcessingInstruction {
                target: arg1.clone(),
                data: arg2.clone(),
            })
            .index()
            .to_string()
        }
        "create_doctype" => {
            // arg1 = name, arg2 = public_id. system_id stored only in the
            // JS wrapper since neither current WPT test reads it back from
            // the underlying tree.
            dom.new_node(NodeData::Doctype {
                name: arg1.clone(),
                public_id: arg2.clone(),
                system_id: String::new(),
            })
            .index()
            .to_string()
        }
        "pi_target" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::ProcessingInstruction { target, .. } => Some(target.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::Doctype { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_public_id" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::Doctype { public_id, .. } => Some(public_id.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "element_children" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .children(NodeId::new(nid))
                .iter()
                .filter(|&&id| dom.get_node(id).map(|n| n.is_element()).unwrap_or(false))
                .map(|id| id.index() as i32)
                .collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "has_child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node(NodeId::new(nid), |n| n.first_child.is_some())
                .unwrap_or(false)
                .to_string()
        }
        "contains" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let other = arg2.parse::<u32>().unwrap_or(0);
            dom.descendants(NodeId::new(nid))
                .contains(&NodeId::new(other))
                .to_string()
        }
        // Connectivity is maintained incrementally by DomTree. Exposing the
        // cached bit avoids an ancestor op crossing for every level when JS
        // builds a deep detached subtree.
        "is_connected" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.is_connected(NodeId::new(nid)).to_string()
        }
        // Index of a node among its parent's children. Walks prev siblings in
        // Rust, avoiding the per-step JS->op round trips a Range comparison
        // would otherwise make.
        "node_index" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            node_child_index(dom, NodeId::new(nid)).to_string()
        }
        // Document (preorder) tree order of two nodes: -1 if a precedes b, 1 if
        // a follows b, 0 if equal. Used by the Range boundary-point algorithms.
        "compare_order" => {
            let a = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let b = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            compare_node_order(dom, a, b).to_string()
        }
        // Root (topmost ancestor) of a node, in one op rather than an O(depth)
        // walk of parentNode ops from JS.
        "node_root" => {
            let mut cur = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            while let Some(p) = dom.with_node(cur, |x| x.parent).flatten() {
                cur = p;
            }
            cur.index().to_string()
        }
        _ => "null".into(),
    }
}

/// Index of `n` among its parent's children (0-based).
fn node_child_index(dom: &DomTree, n: NodeId) -> usize {
    let mut i = 0usize;
    let mut cur = dom.with_node(n, |x| x.prev_sibling).flatten();
    while let Some(p) = cur {
        i += 1;
        cur = dom.with_node(p, |x| x.prev_sibling).flatten();
    }
    i
}

/// Ancestor chain of `n` from the root down to `n` (root first).
fn node_ancestors_root_first(dom: &DomTree, n: NodeId) -> Vec<NodeId> {
    let mut v = vec![n];
    let mut cur = n;
    while let Some(p) = dom.with_node(cur, |x| x.parent).flatten() {
        v.push(p);
        cur = p;
    }
    v.reverse();
    v
}

/// Preorder (document) order comparison of two nodes: -1 before, 1 after, 0 same.
fn compare_node_order(dom: &DomTree, a: NodeId, b: NodeId) -> i32 {
    if a == b {
        return 0;
    }
    let aa = node_ancestors_root_first(dom, a);
    let bb = node_ancestors_root_first(dom, b);
    // Different roots: order is undefined per spec; keep it stable by node id.
    if aa[0] != bb[0] {
        return if a.index() < b.index() { -1 } else { 1 };
    }
    let mut i = 0usize;
    while i < aa.len() && i < bb.len() && aa[i] == bb[i] {
        i += 1;
    }
    if i >= aa.len() {
        return -1; // a is an ancestor of b -> a precedes
    }
    if i >= bb.len() {
        return 1; // b is an ancestor of a -> a follows
    }
    if node_child_index(dom, aa[i]) < node_child_index(dom, bb[i]) {
        -1
    } else {
        1
    }
}


pub(crate) fn node_is_script(dom: &DomTree, node_id: NodeId) -> bool {
    dom.with_node(node_id, |node| {
        node.as_element()
            .map(|name| name.local.as_ref().eq_ignore_ascii_case("script"))
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

fn script_nodes_including_template_contents(dom: &DomTree, root: NodeId) -> Vec<NodeId> {
    let mut scripts = Vec::new();
    let mut stack = vec![root];
    while let Some(node_id) = stack.pop() {
        if node_is_script(dom, node_id) {
            scripts.push(node_id);
        }
        let template_contents = dom
            .with_node(node_id, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        if let Some(contents) = template_contents {
            stack.push(contents);
        }
        let children = dom.children(node_id);
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    scripts
}

pub(crate) fn mark_script_subtree_started(state: &PageState, root: NodeId) {
    let Some(dom) = state.dom.as_ref() else {
        return;
    };
    let scripts = script_nodes_including_template_contents(dom, root);
    state.already_started_scripts.borrow_mut().extend(scripts);
}

fn propagate_script_start_state(
    dom: &DomTree,
    source_root: NodeId,
    cloned_root: NodeId,
    started: &RefCell<HashSet<NodeId>>,
) {
    let mut pairs = vec![(source_root, cloned_root)];
    let mut additions = Vec::new();
    let current = started.borrow();
    while let Some((source, cloned)) = pairs.pop() {
        if current.contains(&source) {
            additions.push(cloned);
        }

        let source_template = dom
            .with_node(source, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        let cloned_template = dom
            .with_node(cloned, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        if let (Some(source_contents), Some(cloned_contents)) = (source_template, cloned_template) {
            pairs.push((source_contents, cloned_contents));
        }

        let source_children = dom.children(source);
        let cloned_children = dom.children(cloned);
        for pair in source_children.into_iter().zip(cloned_children).rev() {
            pairs.push(pair);
        }
    }
    drop(current);
    started.borrow_mut().extend(additions);
}


fn fragment_context_and_html(arg: &str) -> (html5ever::QualName, &str) {
    let mut parts = arg.splitn(3, '\0');
    let first = parts.next().unwrap_or("body");
    let second = parts.next();
    let third = parts.next();
    let (namespace, qualified, html) = match (second, third) {
        // Namespace-aware encoding used by the current bootstrap.
        (Some(qualified), Some(html)) => (first, qualified, html),
        // Backward-compatible encoding for older snapshots: `local\0html`.
        (Some(html), None) => ("http://www.w3.org/1999/xhtml", first, html),
        (None, None) => ("http://www.w3.org/1999/xhtml", "body", first),
        (None, Some(_)) => unreachable!(),
    };
    let (prefix, local) = match qualified.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
            (Some(html5ever::Prefix::from(prefix)), local)
        }
        _ => (
            None,
            if qualified.is_empty() {
                "body"
            } else {
                qualified
            },
        ),
    };
    (
        html5ever::QualName::new(
            prefix,
            html5ever::Namespace::from(namespace),
            html5ever::LocalName::from(local),
        ),
        html,
    )
}


pub(crate) fn document_base_url(state: &PageState) -> Option<String> {
    let document_url = url::Url::parse(&state.url).ok()?;
    let base_href = state.dom.as_ref().and_then(|dom| {
        dom.query_selector("base[href]")
            .ok()
            .flatten()
            .and_then(|id| {
                dom.get_node(id)
                    .and_then(|node| node.get_attribute("href").map(str::to_string))
            })
    });
    match base_href {
        // https://html.spec.whatwg.org/multipage/semantics.html#set-the-frozen-base-url
        // A data: or javascript: base falls back to the document URL. Accepting it would instead
        // make every later relative resolution fail.
        Some(href) => match document_url.join(&href) {
            Ok(base) if base.scheme() != "data" && base.scheme() != "javascript" => {
                Some(base.to_string())
            }
            _ => Some(document_url.to_string()),
        },
        None => Some(document_url.to_string()),
    }
}

/// The raw `href` attribute of the first `<base href>`, unresolved. The JS layer needs it after
/// `history.pushState`: the document URL has moved, only JS knows the new one, so only JS can
/// resolve a relative base against it.
fn document_base_href(state: &PageState) -> Option<String> {
    state.dom.as_ref().and_then(|dom| {
        dom.query_selector("base[href]")
            .ok()
            .flatten()
            .and_then(|id| {
                dom.get_node(id)
                    .and_then(|node| node.get_attribute("href").map(str::to_string))
            })
    })
}


const SUPPORTED_COMMANDS: &[&str] = &[
    "get_form_state",
    "set_form_value",
    "set_form_checked",
    "set_form_indeterminate",
    "document_node_id",
    "document_title",
    "document_url",
    "document_base_url",
    "document_base_href",
    "document_referrer",
    "document_encoding",
    "document_element",
    "document_doctype",
    "get_element_by_id",
    "query_selector",
    "query_selector_all",
    "query_selector_scoped",
    "query_selector_all_scoped",
    "matches_selector",
    "node_type",
    "node_name",
    "text_content",
    "parent_node",
    "first_child",
    "last_child",
    "next_sibling",
    "prev_sibling",
    "next_in_subtree",
    "prev_in_subtree",
    "next_after_subtree",
    "child_nodes",
    "assigned_nodes",
    "tag_name",
    "local_name",
    "namespace_uri",
    "get_attribute",
    "attribute_names",
    "set_attribute",
    "inner_html",
    "outer_html",
    "append_child",
    "remove_child",
    "insert_before",
    "remove_attribute",
    "get_attribute_ns",
    "set_attribute_ns",
    "remove_attribute_ns",
    "set_inner_html",
    "set_inner_html_context",
    "set_fragment_html_executable",
    "document_write",
    "document_write_reset",
    "set_text_content",
    "template_contents",
    "create_document_fragment",
    "clone_node",
    "create_element",
    "create_element_ns",
    "create_text_node",
    "create_comment_node",
    "create_processing_instruction",
    "create_doctype",
    "pi_target",
    "doctype_name",
    "doctype_public_id",
    "element_children",
    "has_child_nodes",
    "contains",
    "is_connected",
    "node_index",
    "compare_order",
    "node_root",
    "document_lifecycle",
];
