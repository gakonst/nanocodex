use crate::{
    Error, Result,
    ax::Node,
    native::{Action, App, Desktop, Image, Target},
    selection::{TextRange, replace_utf16},
};
use serde_json::{Value, json};

pub struct Fixture {
    pub root: Node,
    selection: TextRange,
    focused: String,
    generation: u64,
    pub actions: Vec<Action>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            root: Node {
                identity: "window".into(),
                role: "AXWindow".into(),
                title: Some("Skyre Fixture".into()),
                enabled: true,
                children: vec![
                    Node {
                        identity: "field-0".into(),
                        role: "AXTextField".into(),
                        description: Some("Selection field".into()),
                        value: Some("red alpha blue alpha green".into()),
                        identifier: Some("fixture-input".into()),
                        enabled: true,
                        settable: true,
                        ..Default::default()
                    },
                    Node {
                        identity: "button".into(),
                        role: "AXButton".into(),
                        title: Some("Increment".into()),
                        enabled: true,
                        ..Default::default()
                    },
                    Node {
                        identity: "counter".into(),
                        role: "AXStaticText".into(),
                        value: Some("0".into()),
                        enabled: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            selection: TextRange {
                location: 0,
                length: 0,
            },
            focused: "field-0".into(),
            generation: 0,
            actions: vec![],
        }
    }
}
impl Fixture {
    pub fn from_tree(root: Node) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }
    fn write(&mut self, text: &str) -> Result<()> {
        let node = self
            .root
            .by_identity_mut(&self.focused)
            .ok_or_else(|| Error::action("No focused field"))?;
        let value = node.value.as_deref().unwrap_or_default();
        node.value = Some(replace_utf16(value, self.selection, text)?);
        self.selection.location += text.encode_utf16().count();
        self.selection.length = 0;
        Ok(())
    }
}
impl Desktop for Fixture {
    fn synthetic(&self) -> bool {
        true
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        Ok(vec![App {
            id: "org.skyre.fixture".into(),
            name: "Skyre Fixture".into(),
            path: "fixture://native".into(),
            pid: 0,
        }])
    }
    fn snapshot(&mut self, _: &App) -> Result<Node> {
        Ok(self.root.clone())
    }
    fn action(&mut self, _: &App, action: Action) -> Result<()> {
        match &action {
            Action::SetValue { identity, value } => {
                let n = self
                    .root
                    .by_identity_mut(identity)
                    .ok_or_else(|| Error::action("Missing fixture target"))?;
                if !n.settable {
                    return Err(Error::action(
                        "Cannot set a value for an element that is not settable",
                    ));
                }
                n.value = Some(value.clone());
            }
            Action::SelectText { identity, range } => {
                self.focused = identity.clone();
                self.selection = *range;
            }
            Action::TypeText { text } | Action::Paste { text, .. } => self.write(text)?,
            Action::Click {
                target: Target::Element { identity },
                ..
            } => {
                let n = self
                    .root
                    .by_identity(identity)
                    .ok_or_else(|| Error::action("Missing fixture target"))?;
                if n.enabled && identity == "button" {
                    let n = self.root.by_identity_mut("counter").unwrap();
                    n.value = Some(
                        (n.value.as_deref().unwrap().parse::<usize>().unwrap() + 1).to_string(),
                    );
                } else if n.enabled && n.settable {
                    self.focused = identity.clone();
                }
            }
            Action::PressKey { key } if key == "super+a" || key == "ctrl+a" => {
                self.selection = TextRange {
                    location: 0,
                    length: self
                        .root
                        .by_identity(&self.focused)
                        .and_then(|n| n.value.as_deref())
                        .unwrap_or_default()
                        .encode_utf16()
                        .count(),
                };
            }
            _ => {}
        }
        self.actions.push(action);
        Ok(())
    }
    fn screenshot(&mut self, _: &App) -> Result<Image> {
        Ok(Image{mime_type:"image/png".into(),data:"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into()})
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "fixture",
            "list_apps",
            "get_app_state",
            "get_screenshot",
            "click",
            "drag",
            "press_key",
            "type_text",
            "set_value",
            "select_text",
            "scroll",
            "perform_secondary_action",
            "paste",
        ]
    }
    fn control_fixture(&mut self, method: &str, args: &Value) -> Result<Value> {
        match method {
            "replace" | "duplicate" => {
                let source = self
                    .root
                    .children
                    .first()
                    .ok_or_else(|| Error::action("No field"))?
                    .clone();
                self.root
                    .children
                    .retain(|n| n.identifier != source.identifier);
                self.generation += 1;
                let count = if method == "duplicate" { 2 } else { 1 };
                for i in (0..count).rev() {
                    let mut n = source.clone();
                    n.identity = format!("field-{}-{i}", self.generation);
                    self.root.children.insert(0, n);
                }
            }
            "remove" => {
                let identity = args["identity"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("identity required"))?;
                self.root.children.retain(|n| n.identity != identity);
            }
            "reorder" => {
                if !self.root.children.is_empty() {
                    let n = self.root.children.remove(0);
                    self.root.children.push(n);
                }
            }
            "state" => {
                return Ok(
                    json!({"root":self.root,"actions":self.actions,"selection":self.selection}),
                );
            }
            _ => return Err(Error::unsupported("Unknown fixture control")),
        }
        Ok(Value::Null)
    }
}
