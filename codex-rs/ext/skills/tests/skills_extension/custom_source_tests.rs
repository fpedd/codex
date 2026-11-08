//! Exercises custom skill sources, which embedders register alongside the built-in ones.

use super::*;
use codex_skills_extension::SkillProviderSource;

fn static_provider(entry: SkillCatalogEntry) -> Arc<StaticSkillProvider> {
    Arc::new(StaticSkillProvider {
        catalog: SkillCatalog {
            is_partial: false,
            entries: vec![entry],
            warnings: Vec::new(),
        },
        read_requests: Arc::new(Mutex::new(Vec::new())),
        list_calls: None,
        fail_first_list: false,
    })
}

#[tokio::test]
async fn custom_skills_are_listed_while_host_skills_are_in_world_state() -> TestResult {
    let custom = SkillSourceKind::Custom("embedder".to_string());
    let providers = SkillProviders::new()
        .with_host_provider(static_provider(test_entry(
            SkillSourceKind::Host,
            "host",
            "host/host-skill",
            "host-skill/SKILL.md",
        )))
        .with_provider(SkillProviderSource::new(
            custom.clone(),
            "embedder",
            static_provider(test_entry(
                custom,
                "embedder",
                "embedder/custom-skill",
                "custom-skill/SKILL.md",
            )),
        ));
    let mut builder = ExtensionRegistryBuilder::new();
    install_with_providers(&mut builder, providers, skills_extension_config);
    let registry = builder.build();
    let session_store = ExtensionData::new("session");
    let thread_store = ExtensionData::new("thread");
    registry.thread_lifecycle_contributors()[0]
        .on_thread_start(ThreadStartInput {
            config: &default_config(),
            session_source: &SessionSource::Cli,
            persistent_thread_state_available: true,
            environments: &[],
            mcp_resource_client: None,
            extension_metrics: None,
            session_store: &session_store,
            thread_store: &thread_store,
        })
        .await;
    let turn_store = ExtensionData::new("turn-1");
    turn_store.insert(HostSkillsSnapshot::new(Arc::new(
        SkillLoadOutcome::default(),
    )));
    let step_store = ExtensionData::new("step");

    // World State is recorded before the turn's input, as in a real turn.
    let sections = prepared_world_state(
        &registry,
        &[],
        WorldStateContributionInput {
            previous_world_state: None,
            step_store: &step_store,
            model_info: &catalog_model_info(),
            thread_id: codex_protocol::ThreadId::new(),
            turn_id: "turn-1",
            environments: &[],
            extension_metrics: None,
            session_store: &session_store,
            thread_store: &thread_store,
            turn_store: &turn_store,
        },
    )
    .await;
    let host = world_state_section(&sections, "host_skills")
        .render_diff(PreviousWorldStateSection::Absent)
        .1
        .ok_or("host skills should be in World State")?;
    assert!(host.body().clone().expect_text().contains("- host-skill:"));

    let fragments = prepared_turn_input(
        &registry,
        TurnInputContext {
            step_store: &step_store,
            turn_id: "turn-1".to_string(),
            user_input: Vec::new(),
            environments: Vec::new(),
        },
        /*extension_metrics*/ None,
        &session_store,
        &thread_store,
        &turn_store,
    )
    .await;

    // Only the host catalog moved to World State; the custom catalog must still be listed.
    let listed = fragments
        .iter()
        .map(|fragment| fragment.render().expect_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(listed.contains("- custom-skill:"), "{listed}");
    assert!(!listed.contains("- host-skill:"), "{listed}");
    Ok(())
}
