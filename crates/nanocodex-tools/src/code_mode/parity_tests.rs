//! Compare local contracts with fixtures compiled from the pinned upstream constructors.
use super::*;
use crate::{Tool, code_mode_spec, standard::StandardTool};
use serde_json::json;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/codex-parity/shared-tools.json"
    ))
    .unwrap()
}

#[test]
fn shared_tools_and_code_mode_wrappers_match_pinned_upstream() {
    let fixture = fixture();
    for definition in [
        StandardTool::ExecCommand.definition(),
        StandardTool::WriteStdin.definition(),
        StandardTool::UpdatePlan.definition(),
        StandardTool::ViewImage.definition(),
        code_mode_spec::wait_spec(),
        crate::image_generation::ImageGenerationHandler::new(crate::ImageGenerationConfig {
            api_base_url: "https://example.invalid/v1".into(),
            auth: nanocodex_oai_api::auth::OpenAiAuth::api_key("synthetic-test-key"),
            save_root: std::path::PathBuf::from("."),
        })
        .definition(),
    ] {
        let name = definition.name().to_owned();
        let mut wire = serde_json::to_value(&definition).unwrap();
        if let Some(output_schema) = definition.output_schema() {
            wire["output_schema"] = output_schema.as_value().clone();
        }
        assert_eq!(wire, fixture["tools"][&name], "{name} definition");
        assert_eq!(
            augment_definition_for_code_mode(definition).description(),
            fixture["wrappers"][&name].as_str().unwrap(),
            "{name} Code Mode declaration"
        );
    }
}

#[test]
fn exec_preamble_and_grammar_match_upstream_with_runtime_name_adaptation() {
    let fixture = fixture();
    assert_eq!(
        EXEC_DESCRIPTION,
        fixture["exec_description"]
            .as_str()
            .unwrap()
            .replace("fresh V8 isolate", "fresh JavaScript context")
    );
    assert_eq!(
        MCP_TYPESCRIPT_PREAMBLE,
        fixture["mcp_preamble"].as_str().unwrap()
    );
    let wire = serde_json::to_value(code_mode_spec::exec_spec(&[], &[], false, false)).unwrap();
    assert_eq!(wire["format"]["definition"], fixture["exec_grammar"]);
}

#[test]
fn wrappers_retain_refs_unions_tuple_types_and_freeform_inputs() {
    let schema = json!({
        "type": "object", "properties": {
            "choice": {"$ref": "#/$defs/Choice"},
            "tuple": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "number"}]}
        }, "$defs": {"Choice": {"oneOf": [{"const": "a"}, {"const": "b"}]}}
    });
    let definition = ToolDefinition::function("test.tool", "Test", schema);
    let rendered = exec_tool_declaration(&definition).unwrap();
    assert!(rendered.contains("test_tool(args:"));
    assert!(rendered.contains("choice?: \"a\" | \"b\""));
    assert!(rendered.contains("tuple?: [string, number]"));
    let patch = exec_tool_declaration(&StandardTool::ApplyPatch.definition()).unwrap();
    assert!(patch.contains("apply_patch(input: string): Promise<unknown>"));
}
