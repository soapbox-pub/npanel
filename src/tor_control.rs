//! A connection to Tor's control port.
//!
//! The protocol is line-based and strictly ordered — one reply per command, in
//! order — so commands take turns and each waits for the next reply. The one
//! wrinkle is that asynchronous event lines (any 6xx code) can arrive between
//! a command and its reply, so replies are assembled first and routed by
//! status code second.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

/// How long a command waits for its reply. Generous, because `ADD_ONION` does
/// key generation before it answers; it exists so that a Tor which stops
/// replying is noticed and reconnected to.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the control socket itself to come up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The virtual port an onion answers on.
const ONION_PORT: u16 = 80;

/// One complete reply: its status code, and the text of each line.
#[derive(Debug)]
struct Reply {
    code: u16,
    lines: Vec<String>,
}

/// An onion service's stable identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionKey {
    /// The address, with its `.onion` suffix.
    pub address: String,
    /// `ED25519-V3:<base64>`, verbatim as Tor spells it in both directions.
    pub secret_key: String,
}

/// A non-250 reply, kept with its code so callers can tell them apart.
#[derive(Debug)]
pub struct TorControlError {
    pub code: u16,
    pub detail: String,
}

impl std::fmt::Display for TorControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tor refused the command ({}): {}", self.code, self.detail)
    }
}

impl std::error::Error for TorControlError {}

type Writer = Box<dyn AsyncWrite + Send + Unpin>;

pub struct TorControl {
    writer: Mutex<(Writer, mpsc::Receiver<Reply>)>,
    /// Cancelled when the connection ends, for any reason.
    pub closed: CancellationToken,
}

impl TorControl {
    /// Connect and authenticate, or fail explaining which of the two failed.
    pub async fn connect(url: &str, password: &str) -> Result<TorControl> {
        let (reader, writer): (Box<dyn AsyncRead + Send + Unpin>, Writer) = if let Some(path) = url.strip_prefix("unix:") {
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::UnixStream::connect(path))
                .await
                .map_err(|_| anyhow!("Tor's control port at {url} did not accept a connection within {}s.", CONNECT_TIMEOUT.as_secs()))?
                .map_err(|e| anyhow!("Could not reach Tor's control port at {url}: {e}"))?;
            let (r, w) = stream.into_split();
            (Box::new(r), Box::new(w))
        } else {
            let target = url.strip_prefix("tcp://").unwrap_or(url);
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(target))
                .await
                .map_err(|_| anyhow!("Tor's control port at {url} did not accept a connection within {}s.", CONNECT_TIMEOUT.as_secs()))?
                .map_err(|e| anyhow!("Could not reach Tor's control port at {url}: {e}"))?;
            let _ = stream.set_nodelay(true);
            let (r, w) = stream.into_split();
            (Box::new(r), Box::new(w))
        };

        let (tx, rx) = mpsc::channel(16);
        let closed = CancellationToken::new();
        tokio::spawn(read_replies(reader, tx, closed.clone()));

        let control = TorControl { writer: Mutex::new((writer, rx)), closed };
        if let Err(error) = control.authenticate(password).await {
            control.close();
            return Err(error);
        }
        Ok(control)
    }

    /// Register an onion service forwarding to `target_port` on loopback.
    ///
    /// Pass `existing` to republish a service npanel already has the key for;
    /// omit it to have Tor generate a new one. Services are deliberately not
    /// created with `Flags=Detach`: an onion outliving this connection would
    /// keep answering at an address whose server is gone.
    pub async fn add_onion(&self, target_port: u16, existing: Option<&OnionKey>) -> Result<OnionKey> {
        let spec = existing.map_or("NEW:ED25519-V3", |key| key.secret_key.as_str());
        let reply = match self.command(&format!("ADD_ONION {spec} Port={ONION_PORT},127.0.0.1:{target_port}")).await {
            Ok(reply) => reply,
            Err(error) => {
                // Tor still has the service from a connection it has not
                // finished tearing down. That is the state wanted, so take it.
                if let (Some(existing), Some(refused)) = (existing, error.downcast_ref::<TorControlError>()) {
                    if refused.detail.to_lowercase().contains("collision") {
                        return Ok(existing.clone());
                    }
                }
                return Err(error);
            }
        };

        let Some(service_id) = field(&reply, "ServiceID") else {
            bail!("Tor accepted the onion service but did not say what its address is.");
        };
        let Some(secret_key) = field(&reply, "PrivateKey").or_else(|| existing.map(|k| k.secret_key.clone())) else {
            bail!("Tor created an onion service but did not return its key, so the address could not be kept.");
        };

        Ok(OnionKey { address: format!("{service_id}.onion"), secret_key })
    }

    /// Unregister an onion service. Its key is unaffected.
    pub async fn del_onion(&self, address: &str) -> Result<()> {
        self.command(&format!("DEL_ONION {}", address.trim_end_matches(".onion"))).await.map(|_| ())
    }

    pub fn close(&self) {
        self.closed.cancel();
    }

    async fn authenticate(&self, password: &str) -> Result<()> {
        let info = self.command("PROTOCOLINFO 1").await?;
        let auth = info.lines.iter().find(|l| l.starts_with("AUTH ")).cloned().unwrap_or_default();
        let methods: Vec<&str> =
            auth.split_whitespace().find_map(|p| p.strip_prefix("METHODS=")).map(|m| m.split(',').collect()).unwrap_or_default();
        let cookie_file = auth.find("COOKIEFILE=\"").map(|at| quoted(&auth[at + "COOKIEFILE=".len()..]));

        if !password.is_empty() && methods.contains(&"HASHEDPASSWORD") {
            self.command(&format!("AUTHENTICATE {}", quote(password))).await?;
            return Ok(());
        }

        if let Some(path) = cookie_file.filter(|_| methods.contains(&"COOKIE") || methods.contains(&"SAFECOOKIE")) {
            // Plain COOKIE rather than SAFECOOKIE's challenge-response: the
            // difference only matters to something that can read the socket,
            // which on loopback or a unix socket can read the cookie file too.
            let cookie = tokio::fs::read(&path).await.map_err(|e| {
                anyhow!(
                    "Could not read Tor's authentication cookie at {path}: {e}. The user npanel runs as has to be in the group that owns it — on Debian and Ubuntu that is debian-tor."
                )
            })?;
            self.command(&format!("AUTHENTICATE {}", hex::encode(cookie))).await?;
            return Ok(());
        }

        if methods.contains(&"NULL") {
            self.command("AUTHENTICATE").await?;
            return Ok(());
        }

        if methods.contains(&"HASHEDPASSWORD") {
            bail!(
                "Tor's control port wants a password. Set the Tor control password under Settings to the one its HashedControlPassword was made from."
            );
        }

        let offered = if methods.is_empty() { "no method".to_string() } else { methods.join(", ") };
        bail!("npanel cannot authenticate to Tor, which offers {offered}.")
    }

    /// Send one command once every command before it has been answered.
    async fn command(&self, command: &str) -> Result<Reply> {
        let mut guard = self.writer.lock().await;
        let (writer, replies) = &mut *guard;
        if self.closed.is_cancelled() {
            bail!("The Tor control connection is closed.");
        }

        writer.write_all(format!("{command}\r\n").as_bytes()).await.map_err(|e| {
            self.closed.cancel();
            anyhow!("The Tor control connection failed: {e}")
        })?;

        let verb = command.split(' ').next().unwrap_or("");
        let reply = tokio::select! {
            reply = replies.recv() => reply,
            _ = tokio::time::sleep(COMMAND_TIMEOUT) => {
                // Nothing can be assumed about a connection that owes a reply.
                self.closed.cancel();
                bail!("Tor did not answer {verb} within {}s.", COMMAND_TIMEOUT.as_secs());
            }
            _ = self.closed.cancelled() => None,
        };
        let Some(reply) = reply else {
            bail!("The Tor control connection closed.");
        };

        if reply.code == 250 || reply.code == 251 {
            Ok(reply)
        } else {
            Err(TorControlError { code: reply.code, detail: reply.lines.join(" ") }.into())
        }
    }
}

/// Read replies off the socket and pass on the ones owed to a command.
async fn read_replies(reader: Box<dyn AsyncRead + Send + Unpin>, tx: mpsc::Sender<Reply>, closed: CancellationToken) {
    let mut reader = BufReader::new(reader);
    let mut lines: Vec<String> = Vec::new();
    let mut data_block: Option<Vec<String>> = None;
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let read = tokio::select! {
            read = reader.read_line(&mut buffer) => read,
            _ = closed.cancelled() => return,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = buffer.trim_end_matches(['\r', '\n']).to_string();

        if let Some(block) = data_block.as_mut() {
            if line == "." {
                lines.push(block.join("\n"));
                data_block = None;
            } else {
                // A line beginning with a dot is sent with the dot doubled.
                block.push(line.strip_prefix('.').filter(|rest| rest.starts_with('.')).map_or(line.clone(), str::to_string));
            }
            continue;
        }

        let code = line.get(..3).and_then(|c| c.parse::<u16>().ok());
        let separator = line.as_bytes().get(3).copied();
        let (Some(code), Some(separator @ (b'-' | b'+' | b' '))) = (code, separator) else {
            break;
        };
        let rest = line.get(4..).unwrap_or("").to_string();

        match separator {
            b'+' => {
                lines.push(rest);
                data_block = Some(Vec::new());
            }
            b'-' => lines.push(rest),
            _ => {
                lines.push(rest);
                let reply = Reply { code, lines: std::mem::take(&mut lines) };
                // 6xx is an asynchronous event, which owes nothing to any command.
                if code >= 600 {
                    continue;
                }
                if tx.send(reply).await.is_err() {
                    break;
                }
            }
        }
    }

    closed.cancel();
}

/// The value of a `Key=Value` line, if the reply carries one.
fn field(reply: &Reply, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    reply.lines.iter().find_map(|line| line.strip_prefix(&prefix)).map(str::to_string)
}

/// Tor's QuotedString: wrapped in quotes, with quotes and backslashes escaped.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Read a QuotedString from the start of `text`, unescaping it.
fn quoted(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().skip(1);
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '"' => break,
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quoted(r#""/run/tor/control.authcookie" rest"#), "/run/tor/control.authcookie");
        assert_eq!(quoted(r#""a\"b""#), "a\"b");
    }

    /// A fake Tor that speaks just enough of the protocol.
    #[tokio::test]
    async fn talks_to_tor() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let reply = if line.starts_with("PROTOCOLINFO") {
                    "250-PROTOCOLINFO 1\r\n250-AUTH METHODS=NULL\r\n250-VERSION Tor=\"0.4.8\"\r\n250 OK\r\n".to_string()
                } else if line == "AUTHENTICATE" {
                    "650 STATUS_GENERAL NOTICE X\r\n250 OK\r\n".to_string()
                } else if line.starts_with("ADD_ONION NEW") {
                    "250-ServiceID=abc\r\n250-PrivateKey=ED25519-V3:key\r\n250 OK\r\n".to_string()
                } else {
                    "552 Unrecognized\r\n".to_string()
                };
                w.write_all(reply.as_bytes()).await.unwrap();
            }
        });

        let control = TorControl::connect(&format!("tcp://127.0.0.1:{port}"), "").await.unwrap();
        let key = control.add_onion(8080, None).await.unwrap();
        assert_eq!(key, OnionKey { address: "abc.onion".into(), secret_key: "ED25519-V3:key".into() });
        let error = control.del_onion("abc.onion").await.unwrap_err();
        assert!(error.to_string().contains("552"));
    }
}
