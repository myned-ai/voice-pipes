use super::*;

#[derive(Debug, Default)]
struct Recorder {
    calls: parking_lot::Mutex<Vec<ClientToolCall>>,
}

#[async_trait]
impl SessionSink for Recorder {
    async fn client_tool_call(&self, call: ClientToolCall) -> Result<(), SessionEnded> {
        self.calls.lock().push(call);
        Ok(())
    }
}

#[derive(Debug)]
struct Ended;

#[async_trait]
impl SessionSink for Ended {
    async fn client_tool_call(&self, _call: ClientToolCall) -> Result<(), SessionEnded> {
        Err(SessionEnded)
    }
}

fn call() -> ClientToolCall {
    ClientToolCall::new("c1", "navigate", serde_json::json!({ "screen": "home" }))
}

/// A call reaches the sink unchanged, through any clone of the handle.
#[tokio::test]
async fn a_call_reaches_the_sink_through_any_clone() {
    let sink = Arc::new(Recorder::default());
    let handle = SessionHandle::new(Arc::clone(&sink) as Arc<dyn SessionSink>, "s-1");
    let clone = handle.clone();
    assert_eq!(clone.session_id(), "s-1");
    clone.client_tool_call(call()).await.unwrap();
    assert_eq!(*sink.calls.lock(), vec![call()]);
}

/// A session that has ended says so instead of losing the call silently.
#[tokio::test]
async fn an_ended_session_reports_it() {
    let handle = SessionHandle::new(Arc::new(Ended), "");
    assert_eq!(handle.client_tool_call(call()).await, Err(SessionEnded));
}

/// The host reads back what it attached, through any clone; asked for
/// another type, or on a handle without an attachment, there is nothing.
#[test]
fn host_data_is_read_back_as_its_own_type_only() {
    let plain = SessionHandle::new(Arc::new(Ended), "s");
    assert!(plain.host_data::<String>().is_none());

    let handle = SessionHandle::new(Arc::new(Ended), "s")
        .with_host_data(Arc::new(String::from("engine handle")));
    let clone = handle.clone();
    assert_eq!(
        clone.host_data::<String>().map(String::as_str),
        Some("engine handle")
    );
    assert!(clone.host_data::<u32>().is_none());
}
