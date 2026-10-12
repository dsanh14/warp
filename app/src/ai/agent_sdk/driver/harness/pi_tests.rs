use std::collections::HashMap;
use std::fs;

use serde_json::{Value, json};

use super::{pi_mcp_config, prepare_pi_settings, prepare_pi_system_prompt};
use crate::ai::ambient_agents::task::HarnessModelConfig;
use crate::ai::mcp::JSONMCPServer;

#[test]
fn settings_preserve_existing_values_and_add_only_external_skills() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    fs::write(
        &settings_path,
        json!({
            "theme": "custom",
            "packages": ["npm:another-package"],
            "skills": ["existing"],
            "extensions": ["custom"]
        })
        .to_string(),
    )
    .unwrap();
    let config = HarnessModelConfig {
        model_id: "openrouter/vendor/model".to_owned(),
        reasoning_level: Some("high".to_owned()),
    };
    let skill_dirs = vec![
        dir.path().join(".agents/skills"),
        dir.path().join("factory/skills"),
    ];
    for _ in 0..2 {
        prepare_pi_settings(
            &settings_path,
            dir.path(),
            dir.path(),
            &skill_dirs,
            Some(&config),
        )
        .unwrap();
    }

    let settings: Value = serde_json::from_slice(&fs::read(settings_path).unwrap()).unwrap();
    assert_eq!(settings["theme"], "custom");
    assert_eq!(
        settings["packages"],
        json!(["npm:another-package", "npm:pi-mcp-adapter"])
    );
    assert_eq!(settings["extensions"], json!(["custom", "-builtin:mcp"]));
    assert_eq!(
        settings["skills"],
        json!(["existing", dir.path().join("factory/skills")])
    );
    assert_eq!(settings["defaultProvider"], "openrouter");
    assert_eq!(settings["defaultModel"], "vendor/model");
    assert_eq!(settings["defaultThinkingLevel"], "high");
    assert_eq!(settings["defaultProjectTrust"], "always");
}

#[test]
fn system_prompt_uses_append_slot_without_project_overrides() {
    let dir = tempfile::tempdir().unwrap();
    prepare_pi_system_prompt(dir.path(), &dir.path().join("repo"), "server instructions").unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("APPEND_SYSTEM.md")).unwrap(),
        "server instructions"
    );
    assert!(!dir.path().join("AGENTS.md").exists());
}

#[test]
fn project_append_prompt_routes_server_prompt_to_agent_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    let cwd = dir.path().join("repo");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::create_dir_all(cwd.join(".pi")).unwrap();
    fs::write(cwd.join(".pi/APPEND_SYSTEM.md"), "project instructions").unwrap();
    prepare_pi_system_prompt(&agent_dir, &cwd, "server instructions").unwrap();

    assert_eq!(
        fs::read_to_string(agent_dir.join("AGENTS.md")).unwrap(),
        "server instructions"
    );
    assert!(!agent_dir.join("APPEND_SYSTEM.md").exists());
    assert_eq!(
        fs::read_to_string(cwd.join(".pi/APPEND_SYSTEM.md")).unwrap(),
        "project instructions"
    );
}

#[test]
fn mcp_adapter_configuration_preserves_stdio_and_remote_fields() {
    let servers: HashMap<String, JSONMCPServer> = serde_json::from_value(json!({
        "stdio": {"command": "node", "args": ["server.mjs"], "env": {"NAME": "literal"}},
        "remote": {"url": "https://example.test/mcp", "headers": {"X-Test": "literal"}}
    }))
    .unwrap();
    assert_eq!(
        pi_mcp_config(&servers),
        json!({"mcpServers": {
            "stdio": {"command": "node", "args": ["server.mjs"], "env": {"NAME": "literal"}},
            "remote": {"url": "https://example.test/mcp", "headers": {"X-Test": "literal"}}
        }})
    );
}
