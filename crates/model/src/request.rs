//! The OpenAI-compatible chat request schema.
//!
//! REQUIEM normalizes message content so it never sends arrays where the
//! endpoint expects strings or vice-versa. The `content` field of a
//! `ChatMessage` is a `serde_json::Value` that is either a JSON string or an
//! array of content parts; `to_json` flattens it to a plain string when the
//! model does not advertise `content_parts` support.

use serde::{Deserialize, Serialize};

/// The role of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        write!(f, "{s}")
    }
}

/// A tool definition (function spec) advertised to the model.
///
/// The OpenAI wire format nests the spec under a `function` key:
/// ```json
/// {"type":"function","function":{"name":"read_file","description":"...","parameters":{}}}
/// ```
/// The custom (de)serializers bridge REQUIEM's flat shape to the nested
/// form so strict gateways can deserialize the `tools` array.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The tool name the model will use in a `tool_call`.
    pub name: String,
    /// A human/LLM description of what the tool does.
    pub description: String,
    /// A JSON-Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

impl ToolDef {
    /// A tool with a JSON-object schema for its parameters.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    fn serialize_as_function<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = ser.serialize_struct("ToolDef", 2)?;
        s.serialize_field("type", "function")?;
        let mut f = serde_json::Map::new();
        f.insert("name".into(), serde_json::Value::String(self.name.clone()));
        f.insert(
            "description".into(),
            serde_json::Value::String(self.description.clone()),
        );
        f.insert("parameters".into(), self.parameters.clone());
        s.serialize_field("function", &f)?;
        s.end()
    }

    fn deserialize_from_function<'de, D: serde::Deserializer<'de>>(
        de: D,
    ) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        struct ToolDefVisitor;
        impl<'de> Visitor<'de> for ToolDefVisitor {
            type Value = ToolDef;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a tool-definition object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut name = String::new();
                let mut description = String::new();
                let mut parameters: serde_json::Value = serde_json::Value::Null;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        // Nested "function" object (OpenAI shape).
                        "function" => {
                            let func: serde_json::Value = map.next_value()?;
                            name = func
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            description = func
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if let Some(p) = func.get("parameters") {
                                parameters = p.clone();
                            }
                        }
                        // Tolerate flat shape for non-OpenAI endpoints.
                        "name" => name = map.next_value()?,
                        "description" => description = map.next_value()?,
                        "parameters" => parameters = map.next_value()?,
                        "type" => {
                            let _ = map.next_value::<serde_json::Value>();
                        }
                        _ => {
                            let _ = map.next_value::<serde_json::Value>();
                        }
                    }
                }
                Ok(ToolDef {
                    name,
                    description,
                    parameters,
                })
            }
        }
        de.deserialize_map(ToolDefVisitor)
    }
}

impl<'de> Deserialize<'de> for ToolDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::deserialize_from_function(d)
    }
}

impl Serialize for ToolDef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.serialize_as_function(s)
    }
}

/// A tool invocation requested by the model.
///
/// The OpenAI wire format nests name and arguments under a `function` key:
/// ```json
/// {"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}
/// ```
/// REQUIEM's flat struct carries the fields at the top level.  The custom
/// (de)serializers below bridge the two shapes so that outgoing requests
/// and incoming responses both match what a strict OpenAI-compatible
/// gateway expects.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// A client-generated id for this call.
    pub id: String,
    /// The name of the tool being called.
    pub name: String,
    /// The JSON-encoded arguments.
    ///
    /// NOTE: the OpenAI protocol carries this as a JSON string, not a JSON
    /// object.  `normalize_arguments` re-serializes it back to the string
    /// form the API expects.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    fn serialize_as_function<S: serde::Serializer>(
        &self,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = ser.serialize_struct("ToolCall", 2)?;
        s.serialize_field("id", &self.id)?;
        // Nested "function" object — the shape the OpenAI gateway expects.
        let mut f = serde_json::Map::new();
        f.insert("name".into(), serde_json::Value::String(self.name.clone()));
        // OpenAI requires `arguments` to be a JSON-encoded *string*, not a
        // raw JSON object.  Always stringify it here so strict gateways
        // (including the 9router proxy) can deserialize it.
        let args_str = match &self.arguments {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string(other)
                .unwrap_or_else(|_| "{}".to_string()),
        };
        f.insert("arguments".into(), serde_json::Value::String(args_str));
        s.serialize_field("function", &f)?;
        s.end()
    }

    fn deserialize_from_function<'de, D: serde::Deserializer<'de>>(
        de: D,
    ) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        struct ToolCallVisitor;
        impl<'de> Visitor<'de> for ToolCallVisitor {
            type Value = ToolCall;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a tool-call object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut id = String::new();
                let mut name = String::new();
                let mut arguments: serde_json::Value = serde_json::Value::Null;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => id = map.next_value()?,
                        "function" => {
                            // The nested object carries name + arguments.
                            let func: serde_json::Value = map.next_value()?;
                            name = func
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            match func.get("arguments") {
                                Some(serde_json::Value::String(s)) => {
                                    // OpenAI carries arguments as a JSON string.
                                    arguments =
                                        serde_json::from_str(s).unwrap_or(serde_json::Value::String(
                                            s.clone(),
                                        ));
                                }
                                Some(v) => arguments = v.clone(),
                                None => arguments = serde_json::Value::String(String::new()),
                            }
                        }
                        // Tolerate a flat shape for endpoints that don't
                        // nest under "function".
                        "name" => name = map.next_value()?,
                        "arguments" => arguments = map.next_value()?,
                        _ => {
                            let _ = map.next_value::<serde_json::Value>();
                        }
                    }
                }
                Ok(ToolCall {
                    id,
                    name,
                    arguments,
                })
            }
        }
        de.deserialize_map(ToolCallVisitor)
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::deserialize_from_function(d)
    }
}

impl Serialize for ToolCall {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.serialize_as_function(s)
    }
}

/// A single chat message.
///
/// `content` is `Option<Value>` to support both plain-text and
/// multimodal (content-parts) messages. When it is `None` on an assistant
/// message that has `tool_calls`, the API expects an empty/null content.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatMessage {
    /// The role.
    pub role: Role,
    /// The message content.
    ///
    /// - For `system`/`user`: usually a JSON string.
    /// - For `assistant` with tool calls: may be `null`.
    /// - For `tool`: the tool's result as a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    /// For `tool` role: the `tool_call_id` this message responds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For `assistant` role: the tool calls the model requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl ChatMessage {
    /// A system message with plain-text content.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(serde_json::Value::String(text.into())),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// A user message with plain-text content.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(serde_json::Value::String(text.into())),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// An assistant message with plain-text content.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(serde_json::Value::String(text.into())),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// A tool-result message.
    pub fn tool_result(tool_call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(serde_json::Value::String(result.into())),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
        }
    }
}

/// The full chat-completions request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// The model identifier. MUST be the globally configured model.
    pub model: String,
    /// The message history.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature (0.0..=2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// The tools to advertise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    /// Whether the model may use tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Whether to stream the response.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// A request id for tracing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ChatRequest {
    /// Build a streaming chat request.
    pub fn streaming(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            stream: true,
            request_id: None,
        }
    }

    /// Set the tools.
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the temperature.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Set max output tokens.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Set a request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Serialize the request to a JSON byte string, normalizing message
    /// content so the payload matches the OpenAI chat schema exactly.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}
