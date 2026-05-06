use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, Response};
use bytes::Bytes;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::proxy::ProxyState;
use crate::reasoning::{append_event, ReasoningEvent};

const RING_CAPACITY: usize = 512;
const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct ReasoningBus {
    sender: broadcast::Sender<ReasoningEvent>,
    ring: Arc<Mutex<VecDeque<ReasoningEvent>>>,
}

impl ReasoningBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            sender,
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAPACITY))),
        }
    }

    pub fn publish(&self, event: ReasoningEvent) {
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() == RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }
        let _ = self.sender.send(event);
    }

    fn subscribe(&self) -> broadcast::Receiver<ReasoningEvent> {
        self.sender.subscribe()
    }
}

pub fn publish(event: ReasoningEvent) {
    append_event(&event);
    if let Some(bus) = GLOBAL_BUS.get() {
        bus.publish(event);
    }
}

pub fn set_global_bus(bus: ReasoningBus) {
    let _ = GLOBAL_BUS.set(bus);
}

static GLOBAL_BUS: OnceLock<ReasoningBus> = OnceLock::new();

pub async fn overlay() -> Html<&'static str> {
    Html(OVERLAY_HTML)
}

const OVERLAY_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Claudex Reasoning</title>
<style>
:root { color-scheme: dark; }
html, body {
  margin: 0;
  width: 100%;
  height: 100%;
  background: transparent;
}
body {
  margin: 0;
  width: 100%;
  height: 100%;
  background: transparent;
  color: #f2efe7;
  font: 14px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  overflow: hidden;
}
#app {
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  background: rgba(15, 17, 23, .72);
  border-radius: 14px;
  backdrop-filter: blur(18px);
  padding: 14px 16px;
  border: 1px solid rgba(255,255,255,.18);
  box-shadow: 0 18px 60px rgba(0,0,0,.28), inset 0 1px 0 rgba(255,255,255,.08);
}
.header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
  color: #d8c7a3;
  letter-spacing: .02em;
  font-size: 12px;
  text-transform: uppercase;
}
.status { color: #8f8778; text-transform: none; letter-spacing: 0; }
#reasoning {
  white-space: pre-wrap;
  overflow: auto;
  height: calc(100% - 62px);
  padding-right: 4px;
}
.empty { color: #8f8778; }
.tokens { color: #8f8778; margin-top: 10px; font-size: 12px; }
</style>
</head>
<body>
<div id="app">
  <div class="header"><span>Reasoning</span><span id="status" class="status">connecting</span></div>
  <div id="reasoning" class="empty">Waiting for provider reasoning...</div>
  <div id="tokens" class="tokens"></div>
</div>
<script>
const reasoning = document.getElementById('reasoning');
const status = document.getElementById('status');
const tokens = document.getElementById('tokens');
let activeKey = null;
let currentText = '';
function turnKey(event) {
  return `${event.session}:${event.turn}`;
}
function showEmpty(text) {
  currentText = '';
  reasoning.classList.add('empty');
  reasoning.textContent = text;
  tokens.textContent = '';
}
function startTurn(event) {
  const key = turnKey(event);
  if (activeKey === key) return;
  activeKey = key;
  currentText = '';
  reasoning.classList.remove('empty');
  reasoning.textContent = '';
  tokens.textContent = '';
}
function setReasoningText(text) {
  currentText = text;
  if (currentText.trim() === '') {
    showEmpty('Waiting for provider reasoning...');
    return;
  }
  reasoning.classList.remove('empty');
  reasoning.textContent = currentText;
  reasoning.scrollTop = reasoning.scrollHeight;
}
function appendReasoningText(text) {
  setReasoningText(currentText + text);
}
const source = new EventSource('/reasoning/events');
source.onopen = () => { status.textContent = 'live'; };
source.onerror = () => { status.textContent = 'reconnecting'; };
source.onmessage = (message) => {
  const event = JSON.parse(message.data);
  if (event.kind === 'response.reasoning_summary_text.delta') {
    startTurn(event);
    appendReasoningText(event.text || '');
  } else if (event.kind === 'response.reasoning_summary_text.done') {
    startTurn(event);
    if (event.text && currentText.trim() === '') setReasoningText(event.text);
  } else if (event.kind === 'reasoning_tokens' && event.value !== null && event.value !== undefined) {
    tokens.textContent = `reasoning tokens: ${event.value}`;
  } else if (event.text) {
    startTurn(event);
    setReasoningText(event.text);
  }
};
</script>
</body>
</html>"#;

pub async fn events(State(state): State<Arc<ProxyState>>) -> Response {
    let receiver = state.reasoning_bus.subscribe();
    let stream = BroadcastStream::new(receiver).filter_map(|event| match event {
        Ok(event) => match serde_json::to_string(&event) {
            Ok(json) => Some(Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                "data: {json}\n\n"
            )))),
            Err(_) => None,
        },
        Err(_) => None,
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
