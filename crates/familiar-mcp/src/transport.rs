use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use familiar_core::FamiliarError;

/// Maximum allowed message size (1 MB). Larger messages are rejected.
pub const MAX_MESSAGE_SIZE: usize = 1_048_576;

#[async_trait]
pub trait Transport: Send {
    /// Read the next message body. Returns `Ok(None)` on clean EOF.
    async fn read_message(&mut self) -> Result<Option<String>, FamiliarError>;

    /// Write a message body (framing handled by the transport).
    async fn write_message(&mut self, msg: &str) -> Result<(), FamiliarError>;
}

/// Stdin/stdout transport with Content-Length framing (per MCP spec).
pub struct StdioTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

impl StdioTransport<tokio::io::Stdin, tokio::io::Stdout> {
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            writer: tokio::io::stdout(),
        }
    }
}

impl Default for StdioTransport<tokio::io::Stdin, tokio::io::Stdout> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R, W> StdioTransport<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub fn from_io(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }
}

#[async_trait]
impl<R, W> Transport for StdioTransport<R, W>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    async fn read_message(&mut self) -> Result<Option<String>, FamiliarError> {
        let mut content_length: Option<usize> = None;

        // Read headers until empty line
        loop {
            let mut header_line = String::new();
            let n = self
                .reader
                .read_line(&mut header_line)
                .await
                .map_err(|e| FamiliarError::Mcp(format!("transport read error: {e}")))?;

            if n == 0 {
                // EOF
                return Ok(None);
            }

            // Strip CRLF or LF
            let trimmed = header_line.trim_end_matches(['\r', '\n']);

            if trimmed.is_empty() {
                break;
            }

            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                let len: usize = rest
                    .trim()
                    .parse()
                    .map_err(|e| FamiliarError::Mcp(format!("invalid Content-Length: {e}")))?;
                content_length = Some(len);
            }
            // Other headers ignored
        }

        let len = content_length
            .ok_or_else(|| FamiliarError::Mcp("missing Content-Length header".to_string()))?;

        if len > MAX_MESSAGE_SIZE {
            return Err(FamiliarError::Mcp(format!(
                "message too large: {len} bytes (max {MAX_MESSAGE_SIZE})"
            )));
        }

        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| FamiliarError::Mcp(format!("transport read body error: {e}")))?;

        let body = String::from_utf8(buf)
            .map_err(|e| FamiliarError::Mcp(format!("invalid UTF-8 in message body: {e}")))?;

        Ok(Some(body))
    }

    async fn write_message(&mut self, msg: &str) -> Result<(), FamiliarError> {
        let header = format!("Content-Length: {}\r\n\r\n", msg.len());
        self.writer
            .write_all(header.as_bytes())
            .await
            .map_err(|e| FamiliarError::Mcp(format!("transport write header error: {e}")))?;
        self.writer
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| FamiliarError::Mcp(format!("transport write body error: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| FamiliarError::Mcp(format!("transport flush error: {e}")))?;
        Ok(())
    }
}

/// Event log entry for MockTransport — captures full ordering of reads and writes.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    Read(String),
    Write(String),
}

/// In-memory transport for testing. Bypasses framing.
pub struct MockTransport {
    inbound: VecDeque<String>,
    pub events: Arc<Mutex<Vec<TransportEvent>>>,
}

impl MockTransport {
    pub fn new(inbound: Vec<String>) -> Self {
        Self {
            inbound: inbound.into(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn events_handle(&self) -> Arc<Mutex<Vec<TransportEvent>>> {
        self.events.clone()
    }

    pub fn outbound(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                TransportEvent::Write(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn read_message(&mut self) -> Result<Option<String>, FamiliarError> {
        match self.inbound.pop_front() {
            Some(msg) => {
                self.events
                    .lock()
                    .unwrap()
                    .push(TransportEvent::Read(msg.clone()));
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    async fn write_message(&mut self, msg: &str) -> Result<(), FamiliarError> {
        self.events
            .lock()
            .unwrap()
            .push(TransportEvent::Write(msg.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_transport_round_trip() {
        let mut t = MockTransport::new(vec!["hello".into(), "world".into()]);
        assert_eq!(t.read_message().await.unwrap(), Some("hello".to_string()));
        t.write_message("response1").await.unwrap();
        assert_eq!(t.read_message().await.unwrap(), Some("world".to_string()));
        t.write_message("response2").await.unwrap();
        assert_eq!(t.read_message().await.unwrap(), None);

        let outbound = t.outbound();
        assert_eq!(outbound, vec!["response1", "response2"]);
    }

    #[tokio::test]
    async fn mock_transport_event_ordering() {
        let mut t = MockTransport::new(vec!["a".into(), "b".into()]);
        t.read_message().await.unwrap();
        t.write_message("x").await.unwrap();
        t.read_message().await.unwrap();
        t.write_message("y").await.unwrap();

        let events = t.events.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                TransportEvent::Read("a".into()),
                TransportEvent::Write("x".into()),
                TransportEvent::Read("b".into()),
                TransportEvent::Write("y".into()),
            ]
        );
    }

    #[tokio::test]
    async fn stdio_transport_round_trip() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let input = format!("{header}{body}");

        let (server, mut client) = tokio::io::duplex(1024);
        // Write the test input to the client end
        client.write_all(input.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        let mut transport: StdioTransport<_, tokio::io::DuplexStream> = StdioTransport::from_io(
            server,
            tokio::io::duplex(1024).0, // dummy writer
        );

        let msg = transport.read_message().await.unwrap();
        assert_eq!(msg, Some(body.to_string()));

        let next = transport.read_message().await.unwrap();
        assert_eq!(next, None);
    }

    #[tokio::test]
    async fn stdio_transport_write_framing() {
        let (mut server, client) = tokio::io::duplex(1024);
        let mut transport: StdioTransport<tokio::io::DuplexStream, tokio::io::DuplexStream> =
            StdioTransport::from_io(tokio::io::duplex(1024).0, client);

        transport.write_message(r#"{"ok":true}"#).await.unwrap();
        drop(transport);

        let mut buf = Vec::new();
        server.read_to_end(&mut buf).await.unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("Content-Length: 11\r\n\r\n"));
        assert!(s.ends_with(r#"{"ok":true}"#));
    }

    #[tokio::test]
    async fn stdio_transport_rejects_oversized() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_SIZE + 1);
        let (server, mut client) = tokio::io::duplex(1024);
        client.write_all(header.as_bytes()).await.unwrap();
        drop(client);

        let mut transport: StdioTransport<_, tokio::io::DuplexStream> =
            StdioTransport::from_io(server, tokio::io::duplex(1024).0);

        let result = transport.read_message().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too large"));
    }

    #[tokio::test]
    async fn stdio_transport_eof_returns_none() {
        let (server, client) = tokio::io::duplex(64);
        drop(client);

        let mut transport: StdioTransport<_, tokio::io::DuplexStream> =
            StdioTransport::from_io(server, tokio::io::duplex(64).0);

        let msg = transport.read_message().await.unwrap();
        assert_eq!(msg, None);
    }

    #[tokio::test]
    async fn stdio_transport_missing_content_length() {
        let input = "Some-Other-Header: value\r\n\r\n";
        let (server, mut client) = tokio::io::duplex(64);
        client.write_all(input.as_bytes()).await.unwrap();
        drop(client);

        let mut transport: StdioTransport<_, tokio::io::DuplexStream> =
            StdioTransport::from_io(server, tokio::io::duplex(64).0);

        let result = transport.read_message().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Content-Length"));
    }

    #[tokio::test]
    async fn stdio_transport_invalid_content_length() {
        let input = "Content-Length: notanumber\r\n\r\n";
        let (server, mut client) = tokio::io::duplex(64);
        client.write_all(input.as_bytes()).await.unwrap();
        drop(client);

        let mut transport: StdioTransport<_, tokio::io::DuplexStream> =
            StdioTransport::from_io(server, tokio::io::duplex(64).0);

        let result = transport.read_message().await;
        assert!(result.is_err());
    }
}
