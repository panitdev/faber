//! Web search as a built-in plugin: the `search` tool every project gets
//! when the server has a search engine, sent as `web__search`.

use std::sync::Arc;

use async_trait::async_trait;
use plugin::{
    ConfigError, Content, Failure, Json, Manifest, Message, Phase, Plugin, SessionCtx, ToolCall,
    ToolResult,
};
use serde_json::json;

pub const ID: &str = "web";

pub struct Web {
    manifest: Manifest,
    web: Arc<harness::Web>,
}

impl Web {
    pub fn new(engine: Arc<dyn search::SearchEngine>) -> Self {
        let tools = harness::Web::definitions()
            .into_iter()
            .map(|tool| plugin::manifest::ToolDefinition {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect();
        Web {
            manifest: Manifest {
                id: ID.to_owned(),
                name: "Web".to_owned(),
                version: "1.0.0".to_owned(),
                description: "Web search.".to_owned(),
                capabilities: Default::default(),
                tools,
                config: plugin::manifest::ConfigSpec {
                    version: 1,
                    schema: json!({ "type": "object", "additionalProperties": false }),
                    default: json!({}),
                },
            },
            web: Arc::new(harness::Web::new(engine)),
        }
    }
}

#[async_trait]
impl Plugin for Web {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    async fn open(
        &self,
        _ctx: &SessionCtx,
        _phase: Phase,
        _config: &Json,
    ) -> Result<Vec<Content>, String> {
        Ok(Vec::new())
    }

    async fn handle_tool(&self, _ctx: &SessionCtx, _config: &Json, call: ToolCall) -> ToolResult {
        let invoker = Arc::clone(&self.web).invoker();
        match invoker(call.name, call.input).await {
            Ok(result) if !result.is_error => ToolResult::ok(result.content),
            // The input passed the schema before it got here, so what is left
            // is the engine: no instance answered, or the network did not.
            Ok(result) => ToolResult::failed(Failure::UpstreamFailure, result.content),
            Err(message) => ToolResult::failed(Failure::HandlerFailure, message),
        }
    }

    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        match config.as_object() {
            Some(object) if object.is_empty() => Ok(config),
            Some(_) => Err(vec![ConfigError::whole("web search takes no settings")]),
            None => Err(vec![ConfigError::whole("expected an object")]),
        }
    }

    fn migrate(&self, config: Json, _from: u32) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }

    fn on_changed(&self, _old: &Json, _new: &Json) -> Vec<Message> {
        Vec::new()
    }

    fn on_removed(&self, _config: &Json) -> Vec<Message> {
        Vec::new()
    }
}
