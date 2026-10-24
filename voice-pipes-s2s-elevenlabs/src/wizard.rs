//! How `ElevenLabs` Agents offers itself in the setup wizard.
//!
//! The conversational brain (persona prompt, LLM, voice, VAD, tool
//! definitions) lives in the `ElevenLabs` *agent* config: create one at
//! elevenlabs.io → Agents and paste its id. Client tools must be defined on
//! the agent (type "Client") with names matching the pipeline's
//! `[[client_tool]]` entries.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this provider.
const FEATURE: &str = "s2s-elevenlabs";

/// The provider's one entry: an agent in the `ElevenLabs` dashboard.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "ElevenLabs Agents (agent configured in the ElevenLabs dashboard)",
            WizardDeploy::Cloud {
                key_env: "ELEVENLABS_API_KEY".into(),
            },
        )
        .with_feature(Some(FEATURE.into()))
        .with_fields(vec![
            WizardField::text(
                "options.agent_id",
                "ElevenLabs agent id (elevenlabs.io → Agents)",
                "",
            )
            .required(),
            WizardField::fixed("vad", "server_vad"),
            WizardField::fixed("api_key_env", "ELEVENLABS_API_KEY"),
            WizardField::comment(
                "options.public_agent = true      # skip API-key auth for public agents\n\
                 options.language = \"en\"          # agent.language override\n\
                 options.override_prompt = true   # send `instructions` as agent.prompt override",
            ),
            WizardField::secret("ELEVENLABS_API_KEY", "ElevenLabs API key"),
        ])
        .with_notes(vec![
            "Persona, LLM, voice and tools are configured on the ElevenLabs agent itself; \
             overrides must be enabled in the agent's Security tab."
                .into(),
        ])
        .with_menu_rank(30),
    ]
}
