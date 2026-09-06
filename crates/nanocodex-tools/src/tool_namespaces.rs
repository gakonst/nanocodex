//! Namespace presentation shared by native and embedded model contracts.
use crate::ToolDefinition;

pub(crate) fn namespace_description(namespace: &str) -> Option<&'static str> {
    if namespace == "collaboration" {
        Some("Agent collaboration tools.")
    } else {
        crate::context_management::namespace_description(namespace)
    }
}

pub(crate) fn group_definitions(definitions: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let mut grouped = Vec::new();
    for mut definition in definitions {
        let canonical = definition.name().to_owned();
        let Some((namespace, name)) = canonical.rsplit_once("__") else {
            grouped.push(definition);
            continue;
        };
        let Some(description) = namespace_description(namespace) else {
            grouped.push(definition);
            continue;
        };
        let ToolDefinition::Function {
            name: direct_name, ..
        } = &mut definition
        else {
            grouped.push(definition);
            continue;
        };
        *direct_name = name.into();
        if let Some(ToolDefinition::Namespace { tools, .. }) = grouped.iter_mut().find(
            |group| matches!(group, ToolDefinition::Namespace { name, .. } if &**name == namespace),
        ) {
            tools.push(definition);
        } else {
            grouped.push(ToolDefinition::namespace(
                namespace,
                description,
                [definition],
            ));
        }
    }
    grouped
}
