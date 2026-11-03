use super::*;

#[test]
fn serialize_request_minimal() {
    let req = ChatCompletionRequest {
        model: "gpt-4o-mini".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: "Hi".into(),
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        max_completion_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        user: None,
        response_format: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: serde_json::Map::new(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["model"], "gpt-4o-mini");
    assert_eq!(json["stream"], false);
    // Optional fields must be absent, not null.
    assert!(json.get("temperature").is_none());
    assert!(json.get("top_p").is_none());
    assert!(json.get("max_tokens").is_none());
    assert!(json.get("stop").is_none());
    assert!(json.get("seed").is_none());
    assert!(json.get("stream_options").is_none());
}

#[test]
fn serialize_request_full() {
    let req = ChatCompletionRequest {
        model: "gpt-4o".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
        ],
        stream: true,
        temperature: Some(0.8),
        top_p: Some(0.95),
        max_tokens: Some(256),
        max_completion_tokens: Some(512),
        stop: Some(vec!["END".into()]),
        presence_penalty: Some(0.5),
        frequency_penalty: Some(0.3),
        seed: Some(42),
        user: Some("user-123".into()),
        response_format: Some(ResponseFormat {
            format_type: "json_object".into(),
        }),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        tools: Some(vec![ToolDef {
            tool_type: "function".into(),
            function: FunctionDef {
                name: "navigate".into(),
                description: "Navigate the UI.".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }]),
        tool_choice: Some(serde_json::json!("auto")),
        extra: serde_json::Map::new(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["temperature"], 0.8);
    assert_eq!(json["top_p"], 0.95);
    assert_eq!(json["max_tokens"], 256);
    assert_eq!(json["max_completion_tokens"], 512);
    assert_eq!(json["stop"], serde_json::json!(["END"]));
    assert_eq!(json["presence_penalty"], 0.5);
    assert_eq!(json["frequency_penalty"], 0.3);
    assert_eq!(json["seed"], 42);
    assert_eq!(json["user"], "user-123");
    assert_eq!(json["response_format"]["type"], "json_object");
    assert_eq!(json["stream_options"]["include_usage"], true);
    assert_eq!(json["tools"][0]["type"], "function");
    assert_eq!(json["tools"][0]["function"]["name"], "navigate");
    assert_eq!(json["tool_choice"], "auto");
}

#[test]
fn deserialize_streaming_chunk_tool_calls() {
    // First fragment carries id + name; later fragments stream
    // argument text correlated by index.
    let first = r#"{
        "id": "chatcmpl-1",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": 0, "id": "call_abc", "type": "function",
                "function": { "name": "navigate", "arguments": "" }
            }]},
            "finish_reason": null
        }]
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(first).unwrap();
    let tc = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
    assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
    assert_eq!(
        tc[0].function.as_ref().unwrap().name.as_deref(),
        Some("navigate")
    );

    let follow = r#"{
        "id": "chatcmpl-1",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": 0,
                "function": { "arguments": "{\"screen\":" }
            }]},
            "finish_reason": null
        }]
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(follow).unwrap();
    let tc = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
    assert!(tc[0].id.is_none());
    assert_eq!(
        tc[0].function.as_ref().unwrap().arguments.as_deref(),
        Some("{\"screen\":")
    );

    let finish = r#"{
        "id": "chatcmpl-1",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(finish).unwrap();
    assert_eq!(
        chunk.choices[0].finish_reason.as_deref(),
        Some("tool_calls")
    );
}

#[test]
fn deserialize_llama_server_single_chunk_tool_call() {
    // llama-server (--jinja) often emits the whole call — id, name,
    // complete arguments AND finish_reason — in one chunk instead of
    // OpenAI's multi-fragment stream.
    let chunk_json = r#"{
        "id": "chatcmpl-llama",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": 0, "id": "call_x1", "type": "function",
                "function": { "name": "navigate", "arguments": "{\"screen\":\"home\"}" }
            }]},
            "finish_reason": "tool_calls"
        }]
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(chunk_json).unwrap();
    let choice = &chunk.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    let tc = choice.delta.tool_calls.as_ref().unwrap();
    assert_eq!(tc[0].id.as_deref(), Some("call_x1"));
    let f = tc[0].function.as_ref().unwrap();
    assert_eq!(f.name.as_deref(), Some("navigate"));
    assert_eq!(f.arguments.as_deref(), Some("{\"screen\":\"home\"}"));
}

#[test]
fn deserialize_non_streaming_response() {
    let json = r#"{
        "id": "chatcmpl-abc123",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello there!" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
    }"#;
    let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.choices.len(), 1);
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("Hello there!")
    );
    let usage = resp.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.total_tokens, 13);
}

#[test]
fn deserialize_streaming_chunk() {
    let json = r#"{
        "id": "chatcmpl-abc123",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": { "content": "Hello" },
            "finish_reason": null
        }],
        "usage": null
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(json).unwrap();
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    assert!(chunk.choices[0].finish_reason.is_none());
}

#[test]
fn deserialize_streaming_chunk_finish() {
    let json = r#"{
        "id": "chatcmpl-abc123",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    }"#;
    let chunk: ChatCompletionChunk = serde_json::from_str(json).unwrap();
    assert!(chunk.choices[0].delta.content.is_none());
    assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
    assert!(chunk.usage.is_some());
}

#[test]
fn deserialize_error_response() {
    let json = r#"{
        "error": {
            "message": "Rate limit exceeded",
            "type": "rate_limit_error",
            "code": "rate_limit_exceeded"
        }
    }"#;
    let err: ApiErrorResponse = serde_json::from_str(json).unwrap();
    assert_eq!(err.error.message, "Rate limit exceeded");
    assert_eq!(err.error.error_type.as_deref(), Some("rate_limit_error"));
}

/// Extra fields serialise beside the common ones, at the top level.
#[test]
fn extra_fields_are_flattened_into_the_body() {
    let mut extra = serde_json::Map::new();
    extra.insert(
        "chat_template_kwargs".into(),
        serde_json::json!({"enable_thinking": false}),
    );
    let req = ChatCompletionRequest {
        model: "m".into(),
        messages: vec![],
        stream: true,
        temperature: None,
        top_p: None,
        max_tokens: Some(64),
        max_completion_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        user: None,
        response_format: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra,
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(json["max_tokens"], 64);
    assert!(json.get("extra").is_none(), "flattened, not nested");
}
