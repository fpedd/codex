//! Exercises custom skill sources, which embedders register alongside the built-in ones.

use super::*;
use codex_skills_extension::SkillProviderSource;
use pretty_assertions::assert_eq;

/// A custom source whose catalog a test can change between turns.
struct CustomSource {
    names: Mutex<Vec<&'static str>>,
}

impl CustomSource {
    fn new(names: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            names: Mutex::new(names.to_vec()),
        })
    }

    fn set(&self, names: &[&'static str]) {
        *self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = names.to_vec();
    }
}

impl SkillProvider for CustomSource {
    fn list<'a>(&'a self, _query: SkillListQuery<'a>) -> SkillProviderFuture<'a, SkillCatalog> {
        let entries = self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|name| {
                test_entry(
                    SkillSourceKind::Custom("embedder".to_string()),
                    "embedder",
                    &format!("embedder/{name}"),
                    &format!("{name}/SKILL.md"),
                )
            })
            .collect();
        Box::pin(async move {
            Ok(SkillCatalog {
                is_partial: false,
                entries,
                warnings: Vec::new(),
            })
        })
    }

    fn read<'a>(
        &'a self,
        request: SkillReadRequest<'a>,
    ) -> SkillProviderFuture<'a, SkillReadResult> {
        Box::pin(async move {
            Ok(SkillReadResult {
                resource: request.resource,
                contents: "Write the release notes.".to_string(),
            })
        })
    }

    fn search(&self, _request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        Box::pin(async { Ok(SkillSearchResult::default()) })
    }
}

struct Thread {
    registry: codex_extension_api::ExtensionRegistry<TestConfig>,
    session_store: ExtensionData,
    thread_store: ExtensionData,
}

impl Thread {
    async fn start(source: Arc<CustomSource>, config: TestConfig) -> Self {
        let mut builder = ExtensionRegistryBuilder::new();
        install_with_providers(
            &mut builder,
            SkillProviders::new().with_provider(SkillProviderSource::new(
                SkillSourceKind::Custom("embedder".to_string()),
                "embedder",
                source,
            )),
            skills_extension_config,
        );
        let thread = Self {
            registry: builder.build(),
            session_store: ExtensionData::new("session"),
            thread_store: ExtensionData::new("thread"),
        };
        thread.registry.thread_lifecycle_contributors()[0]
            .on_thread_start(ThreadStartInput {
                config: &config,
                session_source: &SessionSource::Cli,
                persistent_thread_state_available: true,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store: &thread.session_store,
                thread_store: &thread.thread_store,
            })
            .await;
        thread
    }

    /// One turn against the thread's World State: the custom section's update, which
    /// is recorded in `world_state`, and the turn's input for the user's `text`.
    async fn turn(
        &self,
        turn_id: &str,
        text: &str,
        world_state: &mut serde_json::Map<String, serde_json::Value>,
    ) -> (Option<String>, String) {
        let turn_store = ExtensionData::new(turn_id);
        let step_store = ExtensionData::new("step");
        let previous = world_state.clone();
        let sections = prepared_world_state(
            &self.registry,
            &[],
            WorldStateContributionInput {
                previous_world_state: Some(&previous),
                step_store: &step_store,
                model_info: &catalog_model_info(),
                thread_id: codex_protocol::ThreadId::new(),
                turn_id,
                environments: &[],
                extension_metrics: None,
                session_store: &self.session_store,
                thread_store: &self.thread_store,
                turn_store: &turn_store,
            },
        )
        .await;
        let (snapshot, update) = world_state_section(&sections, "custom_skills").render_diff(
            previous.get("custom_skills").map_or(
                PreviousWorldStateSection::Absent,
                PreviousWorldStateSection::Known,
            ),
        );
        if let Some(snapshot) = snapshot {
            world_state.insert("custom_skills".to_string(), snapshot);
        }
        let input = prepared_turn_input(
            &self.registry,
            TurnInputContext {
                step_store: &step_store,
                turn_id: turn_id.to_string(),
                user_input: vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
                environments: Vec::new(),
            },
            /*extension_metrics*/ None,
            &self.session_store,
            &self.thread_store,
            &turn_store,
        )
        .await
        .iter()
        .map(|fragment| fragment.render().expect_text())
        .collect::<Vec<_>>()
        .join("\n");
        (
            update.map(|fragment| fragment.body().clone().expect_text()),
            input,
        )
    }

    async fn thread_context(&self) -> usize {
        self.registry.context_contributors()[0]
            .contribute_thread_context(&self.session_store, &self.thread_store)
            .await
            .len()
    }
}

fn with_catalog_deltas() -> TestConfig {
    TestConfig {
        catalog_deltas_enabled: true,
        ..default_config()
    }
}

#[tokio::test]
async fn custom_skills_are_listed_once_through_world_state() -> TestResult {
    let thread = Thread::start(CustomSource::new(&["release-notes"]), default_config()).await;
    let mut world_state = serde_json::Map::new();

    let (listed, input) = thread.turn("turn-1", "continue", &mut world_state).await;

    let listed = listed.ok_or("the custom catalog should be in World State")?;
    assert!(listed.contains("- release-notes:"), "{listed}");
    assert!(!input.contains("release-notes"), "{input}");
    assert_eq!(0, thread.thread_context().await);

    let (listed, input) = thread.turn("turn-2", "continue", &mut world_state).await;
    assert_eq!(None, listed);
    assert!(!input.contains("release-notes"), "{input}");
    Ok(())
}

async fn assert_changed_custom_skills_are_listed_once(config: TestConfig) -> TestResult {
    let source = CustomSource::new(&["release-notes"]);
    let thread = Thread::start(Arc::clone(&source), config).await;
    let mut world_state = serde_json::Map::new();
    let (listed, _) = thread.turn("turn-1", "continue", &mut world_state).await;
    assert!(listed.is_some_and(|listed| listed.contains("release-notes")));
    source.set(&["release-notes", "changelog"]);

    let (listed, input) = thread.turn("turn-2", "continue", &mut world_state).await;
    let listed = listed.ok_or("a changed custom catalog should be listed")?;
    assert!(listed.contains("changelog"), "{listed}");
    assert!(!input.contains("changelog"), "{input}");

    let (listed, _) = thread.turn("turn-3", "continue", &mut world_state).await;
    assert_eq!(None, listed);
    Ok(())
}

#[tokio::test]
async fn changed_custom_skills_are_listed_once() -> TestResult {
    assert_changed_custom_skills_are_listed_once(default_config()).await
}

#[tokio::test]
async fn changed_custom_skills_are_listed_once_with_catalog_deltas() -> TestResult {
    assert_changed_custom_skills_are_listed_once(with_catalog_deltas()).await
}

#[tokio::test]
async fn explicit_mentions_resolve_custom_skills_listed_through_world_state() -> TestResult {
    let thread = Thread::start(CustomSource::new(&["release-notes"]), default_config()).await;
    let mut world_state = serde_json::Map::new();

    let (_, input) = thread
        .turn("turn-1", "$release-notes", &mut world_state)
        .await;

    assert!(input.contains("Write the release notes."), "{input}");
    Ok(())
}
