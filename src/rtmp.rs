//! RTMP / RTMPS transport (publisher client) on top of `rml_rtmp` + `rustls`.
//!
//! `MediaPacket.data` must be FLV tag bodies (VIDEODATA / AUDIODATA),
//! i.e. what OBS's `flv-mux.c` already produces for rtmp-stream.

use crate::queue::{Kind, MediaPacket};
use crate::session::Transport;
use bytes::Bytes;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
    StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::{self, Read, Write};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Address family used to connect (like OBS's "IP Address Family").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IpFamily {
    #[default]
    Any,
    V4,
    V6,
}

/// Per-session network options (equivalent to those of OBS's rtmp_output).
#[derive(Clone, Debug, Default)]
pub struct NetOptions {
    pub family: IpFamily,
    /// Local IP the connection goes out from; None = chosen by the OS.
    pub bind_ip: Option<IpAddr>,
    /// Socket send buffer size (SO_SNDBUF) in bytes; None = OS default (autotuned).
    /// Data in the kernel buffer is invisible to the queue's congestion handling.
    pub send_buffer: Option<usize>,
}

impl NetOptions {
    /// Effective family: when a local IP is set, that IP's family wins (like OBS).
    fn effective_family(&self) -> IpFamily {
        match self.bind_ip {
            Some(IpAddr::V4(_)) => IpFamily::V4,
            Some(IpAddr::V6(_)) => IpFamily::V6,
            None => self.family,
        }
    }

    fn accepts(&self, addr: &SocketAddr) -> bool {
        match self.effective_family() {
            IpFamily::Any => true,
            IpFamily::V4 => addr.is_ipv4(),
            IpFamily::V6 => addr.is_ipv6(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RtmpUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub app: String,
    pub stream_key: String,
}

impl RtmpUrl {
    /// `rtmp[s]://host[:port]/app[/...]` + stream key (the key may be given separately,
    /// as in OBS's "Stream key" field, or at the end of the URL).
    pub fn parse(url: &str, key: Option<&str>) -> io::Result<Self> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_string());
        let (tls, rest) = if let Some(r) = url.strip_prefix("rtmps://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("rtmp://") {
            (false, r)
        } else {
            return Err(bad("scheme must be rtmp:// or rtmps://"));
        };
        let (hostport, path) = rest.split_once('/').ok_or_else(|| bad("missing /app"))?;
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad("invalid port"))?),
            None => (hostport.to_string(), if tls { 443 } else { 1935 }),
        };
        if host.is_empty() {
            return Err(bad("empty host"));
        }
        let (app, stream_key) = match key.filter(|k| !k.is_empty()) {
            Some(k) => (path.trim_end_matches('/').to_string(), k.to_string()),
            None => match path.rsplit_once('/') {
                Some((a, k)) if !a.is_empty() && !k.is_empty() => (a.to_string(), k.to_string()),
                _ => return Err(bad("missing stream key")),
            },
        };
        Ok(Self { tls, host, port, app, stream_key })
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    fn sock(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            Stream::Tls(s) => s.read(b),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn tls_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

pub struct RtmpTransport {
    url: RtmpUrl,
    metadata: Option<StreamMetadata>,
    net: NetOptions,
    connect_timeout: Duration,
    /// Hard upper bound on blocking during a send: keeps a dead network from hanging the thread.
    write_timeout: Duration,
    stream: Option<Stream>,
    session: Option<ClientSession>,
    last_drain: Instant,
}

fn other<E: std::fmt::Debug>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{e:?}"))
}

fn at_stage(stage: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{stage}: {error}"))
}

fn publish_status_error(code: &str) -> Option<io::Error> {
    let kind = match code {
        "NetStream.Publish.BadName"
        | "NetStream.Publish.Denied"
        | "NetStream.Publish.Rejected"
        | "NetConnection.Connect.Rejected" => io::ErrorKind::PermissionDenied,
        "NetStream.Publish.Failed"
        | "NetStream.Failed"
        | "NetStream.Unpublish.Success"
        | "NetConnection.Connect.Closed" => io::ErrorKind::ConnectionAborted,
        _ => return None,
    };
    Some(io::Error::new(kind, format!("RTMP server: {code}")))
}

impl RtmpTransport {
    pub fn new(url: RtmpUrl) -> Self {
        Self {
            url,
            metadata: None,
            net: NetOptions::default(),
            connect_timeout: Duration::from_secs(8),
            write_timeout: Duration::from_secs(4),
            stream: None,
            session: None,
            last_drain: Instant::now(),
        }
    }

    pub fn with_timeouts(mut self, connect: Duration, write: Duration) -> Self {
        self.connect_timeout = connect;
        self.write_timeout = write;
        self
    }

    pub fn with_metadata(mut self, metadata: Option<StreamMetadata>) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    fn connect_tcp(&self, addr: &SocketAddr) -> io::Result<TcpStream> {
        if self.net.bind_ip.is_none() && self.net.send_buffer.is_none() {
            return TcpStream::connect_timeout(addr, self.connect_timeout);
        }
        let sock = Socket::new(Domain::for_address(*addr), Type::STREAM, Some(Protocol::TCP))?;
        if let Some(size) = self.net.send_buffer {
            sock.set_send_buffer_size(size)?;
        }
        if let Some(ip) = self.net.bind_ip {
            sock.bind(&SocketAddr::new(ip, 0).into()).map_err(|e| at_stage(&format!("bind IP {ip}"), e))?;
        }
        sock.connect_timeout(&(*addr).into(), self.connect_timeout)?;
        Ok(sock.into())
    }

    fn open_stream(&self) -> io::Result<Stream> {
        let addrs: Vec<SocketAddr> = (self.url.host.as_str(), self.url.port).to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "DNS returned no results"));
        }
        let candidates: Vec<SocketAddr> = addrs.into_iter().filter(|a| self.net.accepts(a)).collect();
        if candidates.is_empty() {
            let msg = format!("server has no {:?} addresses", self.net.effective_family());
            return Err(io::Error::new(io::ErrorKind::NotFound, msg));
        }
        // Try each address (e.g. IPv6 then IPv4) until one connects.
        let mut last = None;
        let mut tcp = None;
        for addr in &candidates {
            match self.connect_tcp(addr) {
                Ok(t) => {
                    tcp = Some(t);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let Some(tcp) = tcp else {
            return Err(last.unwrap_or_else(|| io::Error::other("no addresses")));
        };
        tcp.set_nodelay(true)?;
        tcp.set_write_timeout(Some(self.write_timeout))?;
        tcp.set_read_timeout(Some(self.connect_timeout))?;
        if self.url.tls {
            let name = ServerName::try_from(self.url.host.clone()).map_err(other)?;
            let mut conn = ClientConnection::new(tls_config(), name).map_err(other)?;
            conn.complete_io(&mut &tcp)
                .map_err(|e| at_stage("TLS handshake", e))?;
            Ok(Stream::Tls(Box::new(StreamOwned::new(conn, tcp))))
        } else {
            Ok(Stream::Plain(tcp))
        }
    }

    fn write_results(stream: &mut Stream, results: Vec<ClientSessionResult>) -> io::Result<Vec<ClientSessionEvent>> {
        let mut events = Vec::new();
        for r in results {
            match r {
                ClientSessionResult::OutboundResponse(p) => stream.write_all(&p.bytes)?,
                ClientSessionResult::RaisedEvent(e) => events.push(e),
                _ => {}
            }
        }
        stream.flush()?;
        Ok(events)
    }

    fn handle_input(
        stream: &mut Stream,
        sess: &mut ClientSession,
        data: &[u8],
    ) -> io::Result<Vec<ClientSessionEvent>> {
        if data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server closed the connection"));
        }
        let res = sess.handle_input(data).map_err(other)?;
        Self::write_results(stream, res)
    }

    fn handshake(stream: &mut Stream) -> io::Result<Vec<u8>> {
        let mut hs = Handshake::new(PeerType::Client);
        stream.write_all(&hs.generate_outbound_p0_and_p1().map_err(other)?)?;
        loop {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "handshake interrupted"));
            }
            match hs.process_bytes(&buf[..n]).map_err(other)? {
                HandshakeProcessResult::InProgress { response_bytes } => stream.write_all(&response_bytes)?,
                HandshakeProcessResult::Completed { response_bytes, remaining_bytes } => {
                    stream.write_all(&response_bytes)?;
                    return Ok(remaining_bytes);
                }
            }
        }
    }
}

impl Transport for RtmpTransport {
    fn connect(&mut self) -> io::Result<()> {
        let mut stream = self.open_stream()?;
        let leftover = Self::handshake(&mut stream).map_err(|e| at_stage("RTMP handshake", e))?;

        let mut conf = ClientSessionConfig::new();
        // Same as OBS/librtmp: full tcUrl (including YouTube's ?backup=1) and the FMLE flashVer.
        conf.tc_url = Some(format!(
            "{}://{}:{}/{}",
            if self.url.tls { "rtmps" } else { "rtmp" },
            self.url.host,
            self.url.port,
            self.url.app
        ));
        conf.flash_version = "FMLE/3.0 (compatible; FMSc/1.0)".to_string();
        let (mut sess, init) = ClientSession::new(conf).map_err(other)?;
        Self::write_results(&mut stream, init)?;
        let req = sess.request_connection(self.url.app.clone()).map_err(other)?;
        Self::write_results(&mut stream, vec![req])?;

        let mut pending = if leftover.is_empty() {
            Vec::new()
        } else {
            let r = sess.handle_input(&leftover).map_err(other)?;
            Self::write_results(&mut stream, r)?
        };

        let deadline = Instant::now() + self.connect_timeout * 2;
        let mut publishing_requested = false;
        let mut publish_accepted = false;
        loop {
            for ev in pending.drain(..) {
                match ev {
                    ClientSessionEvent::ConnectionRequestAccepted if !publishing_requested => {
                        let r = sess
                            .request_publishing(self.url.stream_key.clone(), PublishRequestType::Live)
                            .map_err(other)?;
                        Self::write_results(&mut stream, vec![r])?;
                        publishing_requested = true;
                    }
                    ClientSessionEvent::ConnectionRequestRejected { description } => {
                        return Err(io::Error::new(io::ErrorKind::PermissionDenied, description));
                    }
                    ClientSessionEvent::UnhandleableOnStatusCode { code } => {
                        if let Some(error) = publish_status_error(&code) {
                            return Err(error);
                        }
                    }
                    ClientSessionEvent::PublishRequestAccepted => {
                        publish_accepted = true;
                    }
                    _ => {}
                }
            }
            if publish_accepted {
                if let Some(metadata) = &self.metadata {
                    let result = sess.publish_metadata(metadata).map_err(other)?;
                    Self::write_results(&mut stream, vec![result])
                        .map_err(|e| at_stage("RTMP onMetaData", e))?;
                }
                // Live: from here on, reads must not block sending.
                stream.sock().set_read_timeout(Some(Duration::from_millis(1)))?;
                self.stream = Some(stream);
                self.session = Some(sess);
                self.last_drain = Instant::now();
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "RTMP negotiation timed out"));
            }
            let stage = if publishing_requested { "RTMP publish" } else { "RTMP connect" };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).map_err(|e| at_stage(stage, e))?;
            pending = Self::handle_input(&mut stream, &mut sess, &buf[..n])
                .map_err(|e| at_stage(stage, e))?;
        }
    }

    fn send(&mut self, pkt: &MediaPacket) -> io::Result<()> {
        let (stream, sess) = match (self.stream.as_mut(), self.session.as_mut()) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        };
        let ts = RtmpTimestamp::new(pkt.dts_ms);
        // P-frames (non-key, non-header) are droppable at the chunk-stream level.
        let droppable = pkt.kind == Kind::Video && !pkt.keyframe && !pkt.is_header;
        let data: Bytes = pkt.data.clone();
        let r = match pkt.kind {
            Kind::Video => sess.publish_video_data(data, ts, droppable),
            Kind::Audio => sess.publish_audio_data(data, ts, false),
        }
        .map_err(other)?;
        Self::write_results(stream, vec![r]).map_err(|e| at_stage("RTMP media send", e))?;
        self.poll()
    }

    fn poll(&mut self) -> io::Result<()> {
        // Drain server acks / pings / closes roughly every 500 ms.
        if self.last_drain.elapsed() < Duration::from_millis(500) {
            return Ok(());
        }
        self.last_drain = Instant::now();
        let (Some(stream), Some(sess)) = (&mut self.stream, &mut self.session) else {
            return Ok(());
        };
        let mut buf = [0u8; 4096];
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Ok(());
            }
            Err(e) => return Err(at_stage("RTMP status read", e)),
        };
        let events = Self::handle_input(stream, sess, &buf[..n])
            .map_err(|e| at_stage("RTMP status processing", e))?;
        for event in events {
            if let ClientSessionEvent::UnhandleableOnStatusCode { code } = event {
                if let Some(error) = publish_status_error(&code) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.sock().shutdown(std::net::Shutdown::Both);
        }
        self.session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let u = RtmpUrl::parse("rtmps://live.example.com/app", Some("abc")).unwrap();
        assert!(u.tls && u.port == 443 && u.app == "app" && u.stream_key == "abc");
        let u = RtmpUrl::parse("rtmp://h:1936/live/key123", None).unwrap();
        assert!(!u.tls && u.port == 1936 && u.app == "live" && u.stream_key == "key123");
        let u = RtmpUrl::parse("rtmps://b.rtmps.youtube.com/live2?backup=1", Some("abcd")).unwrap();
        assert_eq!((u.app.as_str(), u.stream_key.as_str(), u.port), ("live2?backup=1", "abcd", 443));
        assert!(RtmpUrl::parse("http://x/y", None).is_err());
        assert!(RtmpUrl::parse("rtmp://x/app", None).is_err());
    }
}
