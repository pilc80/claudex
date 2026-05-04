use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningEvent {
    pub ts_ms: u128,
    pub session: String,
    pub turn: String,
    pub provider: String,
    pub kind: String,
    pub text: Option<String>,
    pub value: Option<Value>,
}

impl ReasoningEvent {
    pub fn new(session: &str, turn: &str, provider: &str, kind: &str) -> Self {
        Self {
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            session: session.to_string(),
            turn: turn.to_string(),
            provider: provider.to_string(),
            kind: kind.to_string(),
            text: None,
            value: None,
        }
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }
}

pub fn store_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home
        .join(".local")
        .join("share")
        .join("claudex")
        .join("reasoning")
        .join("events.jsonl"))
}

pub fn append_event(event: &ReasoningEvent) {
    if let Err(err) = append_event_inner(event) {
        tracing::warn!(error = %err, "failed to append reasoning event");
    }
}

fn append_event_inner(event: &ReasoningEvent) -> Result<()> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, event)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub fn clear() -> Result<()> {
    let path = store_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    println!("Cleared reasoning store: {}", path.display());
    Ok(())
}

pub fn tail(lines: usize) -> Result<()> {
    let path = store_path()?;
    if !path.exists() {
        println!("No reasoning events yet: {}", path.display());
        return Ok(());
    }
    let file = OpenOptions::new().read(true).open(&path)?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for line in reader.lines() {
        rows.push(line?);
        if rows.len() > lines {
            rows.remove(0);
        }
    }
    for row in rows {
        print_line(&row);
    }
    Ok(())
}

pub async fn watch_live(host: &str, port: u16) -> Result<()> {
    let url = format!("http://{host}:{port}/reasoning/events");
    let response = match reqwest::get(&url).await {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            tracing::warn!(status = %response.status(), "reasoning SSE endpoint unavailable; falling back to file watch");
            return watch_file();
        }
        Err(err) => {
            tracing::warn!(error = %err, "reasoning SSE endpoint unreachable; falling back to file watch");
            return watch_file();
        }
    };

    println!("Watching reasoning events: {url}");
    let mut printer = WatchPrinter::default();
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim_end_matches('\r').to_string();
            buffer = buffer[pos + 1..].to_string();
            if let Some(data) = line.strip_prefix("data: ") {
                printer.print_line(data);
            } else if let Some(data) = line.strip_prefix("data:") {
                printer.print_line(data);
            }
        }
    }

    Ok(())
}

pub fn watch() -> Result<()> {
    watch_file()
}

fn watch_file() -> Result<()> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(&path)?;
    let mut position = fs::metadata(&path)?.len();
    let mut printer = WatchPrinter::default();
    println!("Watching reasoning events: {}", path.display());

    loop {
        let mut file = OpenOptions::new().read(true).open(&path)?;
        file.seek(SeekFrom::Start(position))?;
        let mut reader = BufReader::new(file);
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            position += bytes as u64;
            printer.print_line(line.trim_end());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[derive(Default)]
struct WatchPrinter {
    active_stream: Option<String>,
    printed_any_delta: bool,
}

impl WatchPrinter {
    fn print_line(&mut self, line: &str) {
        match serde_json::from_str::<ReasoningEvent>(line) {
            Ok(event) => self.print_event(&event),
            Err(_) => println!("{line}"),
        }
    }

    fn print_event(&mut self, event: &ReasoningEvent) {
        if event.kind == "response.reasoning_summary_text.delta" {
            let stream = format!("{}:{}", event.session, event.turn);
            if self.active_stream.as_deref() != Some(stream.as_str()) {
                self.finish_delta_line();
                println!("\nreasoning:");
                self.active_stream = Some(stream);
            }
            if let Some(text) = event.text.as_deref() {
                print!("{text}");
                let _ = std::io::stdout().flush();
                self.printed_any_delta = true;
            }
            return;
        }

        if event.kind == "response.reasoning_summary_text.done" {
            self.finish_delta_line();
            return;
        }

        if event.kind == "reasoning_tokens" {
            self.finish_delta_line();
            if let Some(value) = &event.value {
                println!("reasoning tokens: {value}");
            }
        }
    }

    fn finish_delta_line(&mut self) {
        if self.printed_any_delta {
            println!("\n");
            self.printed_any_delta = false;
        }
        self.active_stream = None;
    }
}

fn print_line(line: &str) {
    match serde_json::from_str::<ReasoningEvent>(line) {
        Ok(event) => print_event(&event),
        Err(_) => println!("{line}"),
    }
}

fn print_event(event: &ReasoningEvent) {
    println!(
        "[{} {} {}] {}",
        event.session, event.turn, event.provider, event.kind
    );
    if let Some(text) = event.text.as_deref().filter(|text| !text.is_empty()) {
        for line in text.lines() {
            println!("  {line}");
        }
    }
    if let Some(value) = &event.value {
        println!("  {value}");
    }
}
