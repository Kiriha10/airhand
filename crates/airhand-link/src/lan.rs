//! Standalone LAN sessions. The host accepts the controller directly, without registration,
//! rendezvous, forwarding, or a persistent connection to a relay.
//!
//! The existing Noise/SPAKE2 handshake still authenticates both devices and encrypts every
//! session frame. An ephemeral TLS certificate belongs only to this connection; it never
//! changes the relay's pinned certificate or trust store.

use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airhand_proto::handshake::MAX_MSG;
use airhand_proto::signal::{
    ClientHello, ClientMsg, ConnectError, Peer, ResumeToken, ServerHello, ServerMsg, SessionGrant,
    connect_outcome,
};
use airhand_proto::{MAX_SIGNAL_MSG, Role, SIGNAL_VERSION, framing};
use anyhow::{Context, Result, bail};
use quinn::{Connection as PeerConnection, Endpoint, EndpointConfig};
use rand::Rng;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, info};

use crate::Problem;
use crate::direct::tls::Cert;
use crate::host::{HostCmd, Incoming};
use crate::lanes::Router;
use crate::net::{self, Dial, LinkConfig, Signal};
use crate::tasks::OwnedTask;
use crate::wire::{read_frame, read_msg, write_frame, write_msg};

pub const PORT: u16 = 4434;
const ALPN: &[u8] = b"airhand-lan-v1";
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 16;
const MAX_INFO: usize = 1024;

/// Only numeric local addresses are accepted. No DNS or public relay configuration is used.
pub fn parse_address(text: &str) -> Option<SocketAddr> {
    let text = text.trim();
    let addr = text.parse::<SocketAddr>().ok().or_else(|| {
        text.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, PORT))
    })?;
    (addr.port() != 0 && local_ip(addr.ip())).then_some(addr)
}

fn local_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
    }
}

/// Addresses shown in the Windows tray. Binding all interfaces also covers later IP changes.
pub fn addresses(port: u16) -> Vec<String> {
    let mut out: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| iface.is_oper_up())
        .map(|iface| iface.ip().to_canonical())
        .filter(|ip| local_ip(*ip) && !ip.is_loopback())
        .map(|ip| SocketAddr::new(ip, port).to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostInfo {
    /// A handshake binding, obtained locally even if the host never registered with a relay.
    code: u32,
}

#[derive(Clone)]
struct Access {
    code: u32,
    resume: ResumeToken,
}

struct Shared {
    name: String,
    os: String,
    access: Mutex<Access>,
    kick: watch::Sender<airhand_proto::session::ByeReason>,
}

impl Shared {
    fn access(&self) -> Access {
        // The handshake binding is a startup snapshot. Later relay registrations must
        // neither change it nor revoke standalone LAN reconnect tokens.
        self.access.lock().unwrap().clone()
    }

    fn revoke(&self, cmd: HostCmd) {
        self.access.lock().unwrap().resume = rand::rng().random();
        self.kick.send_replace(cmd.reason());
    }
}

/// A listener owned by the host process, with its own sessions and cancellation scope.
pub struct Host {
    endpoint: Endpoint,
    shared: Arc<Shared>,
}

impl Host {
    pub fn bind(
        addr: SocketAddr,
        name: String,
        os: String,
        code: Arc<dyn Fn() -> u32 + Send + Sync>,
    ) -> Result<Self> {
        let cert = Cert::generate()?;
        let socket = crate::udp::bind(addr, true)?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(cert.lan_listener(ALPN)?),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        let access = Access {
            code: code(),
            resume: rand::rng().random(),
        };
        let (kick, _) = watch::channel(airhand_proto::session::ByeReason::Kicked);
        Ok(Self {
            endpoint,
            shared: Arc::new(Shared {
                name,
                os,
                access: Mutex::new(access),
                kick,
            }),
        })
    }

    pub fn bind_default(
        name: String,
        os: String,
        code: Arc<dyn Fn() -> u32 + Send + Sync>,
    ) -> Result<Self> {
        Self::bind(
            (Ipv6Addr::UNSPECIFIED, PORT).into(),
            name.clone(),
            os.clone(),
            code.clone(),
        )
        .or_else(|_| Self::bind(([0, 0, 0, 0], PORT).into(), name, os, code))
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub async fn run<S, F>(self, session: S, mut cmds: mpsc::UnboundedReceiver<HostCmd>)
    where
        S: Fn(Incoming) -> F + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let session = Arc::new(session);
        let mut peers = JoinSet::new();
        loop {
            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if !local_ip(incoming.remote_address().ip()) || peers.len() >= MAX_CONNECTIONS {
                        incoming.refuse();
                        continue;
                    }
                    let shared = self.shared.clone();
                    let session = session.clone();
                    peers.spawn(async move {
                        let result = async {
                            let conn = tokio::time::timeout(STEP_TIMEOUT, incoming).await??;
                            serve(conn, shared, &*session).await
                        }.await;
                        if let Err(e) = result {
                            debug!("LAN connection ended: {e:#}");
                        }
                    });
                }
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => self.shared.revoke(cmd),
                    None => break,
                },
                Some(_) = peers.join_next() => {},
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"LAN host stopped");
    }
}

async fn serve<S, F>(conn: PeerConnection, shared: Arc<Shared>, session: &S) -> Result<()>
where
    S: Fn(Incoming) -> F,
    F: Future<Output = ()>,
{
    let (mut send, mut recv) = tokio::time::timeout(STEP_TIMEOUT, conn.accept_bi()).await??;
    let mut buf = Vec::new();
    let hello: ClientHello = tokio::time::timeout(STEP_TIMEOUT, async {
        if !read_frame(&mut recv, MAX_SIGNAL_MSG, &mut buf).await? {
            bail!("the LAN controller closed during its hello");
        }
        Ok::<_, anyhow::Error>(framing::decode_known(&buf)?)
    })
    .await??;
    write_msg(&mut send, &ServerHello::new(None)).await?;
    if hello.version != SIGNAL_VERSION || hello.role != Role::Controller {
        send.finish()?;
        let _ = tokio::time::timeout(STEP_TIMEOUT, send.stopped()).await;
        bail!("incompatible LAN controller");
    }
    let kick = shared.kick.subscribe();
    let access = shared.access();
    write_frame(
        &mut send,
        &serde_json::to_vec(&HostInfo { code: access.code })?,
    )
    .await?;
    let request: ClientMsg =
        tokio::time::timeout(STEP_TIMEOUT, read_msg(&mut recv, MAX_SIGNAL_MSG, &mut buf))
            .await??
            .context("the LAN controller closed before requesting a session")?;
    let ClientMsg::Connect {
        req_id,
        code,
        resume,
        init,
    } = request
    else {
        bail!("unexpected LAN session request");
    };
    if init.len() > MAX_MSG {
        bail!("oversized LAN handshake");
    }
    // Check the current token after the request arrives: a kick while reading invalidates it too.
    let current = shared.access();
    let denied = if code != access.code || current.code != access.code {
        Some(ConnectError::NotFound)
    } else if resume.is_some_and(|token| !bool::from(token.ct_eq(&current.resume))) {
        Some(ConnectError::Revoked)
    } else {
        None
    };
    if let Some(error) = denied {
        write_msg(&mut send, &ServerMsg::connect_result(req_id, Err(error))).await?;
        send.finish()?;
        let _ = tokio::time::timeout(STEP_TIMEOUT, send.stopped()).await;
        return Ok(());
    }
    let session_id = rand::rng().random::<u64>();
    let grant = SessionGrant {
        session_id,
        host_name: shared.name.clone(),
        host_os: shared.os.clone(),
        resume: current.resume,
        observed: conn.remote_address(),
    };
    let (router, _router) = Router::start_lan(&conn);
    write_msg(&mut send, &ServerMsg::connect_result(req_id, Ok(grant))).await?;
    let inc = Incoming {
        conn: conn.clone(),
        router,
        session_id,
        init,
        code,
        peer: Peer {
            name: hello.name,
            ip: conn.remote_address().ip().to_canonical(),
        },
        kick,
        direct: None,
        file_limit: None,
        lan: true,
    };
    info!(session_id, peer = %conn.remote_address(), "standalone LAN session created");
    tokio::select! {
        _ = session(inc) => {
            // Keep QUIC alive until the controller reads a pairing rejection or Bye and
            // closes its connection. Closing immediately can discard the final stream frame.
            let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
        },
        _ = conn.closed() => {},
    }
    conn.close(0u32.into(), b"LAN session ended");
    Ok(())
}

/// Kept alive by the controller session, independently of the relay service's router/tasks.
pub(crate) struct Connection {
    pub address: String,
    pub code: u32,
    pub signal: Signal,
    pub router: Arc<Router>,
    _router: OwnedTask<()>,
}

impl Connection {
    pub async fn request(
        &mut self,
        resume: Option<ResumeToken>,
        init: Vec<u8>,
    ) -> Result<SessionGrant, Problem> {
        let result = tokio::time::timeout(STEP_TIMEOUT, async {
            write_msg(
                &mut self.signal.send,
                &ClientMsg::Connect {
                    req_id: 0,
                    code: self.code,
                    resume,
                    init,
                },
            )
            .await?;
            let mut buf = Vec::new();
            let result: ServerMsg = read_msg(&mut self.signal.recv, MAX_SIGNAL_MSG, &mut buf)
                .await?
                .context("the LAN host closed before answering")?;
            let ServerMsg::ConnectResult {
                req_id: 0,
                grant,
                error,
            } = result
            else {
                bail!("unexpected LAN session answer");
            };
            Ok::<_, anyhow::Error>(connect_outcome(grant, error).map_err(Problem::from)?)
        })
        .await;
        match result {
            Ok(Ok(grant)) => Ok(grant),
            Ok(Err(e)) => Err(Problem::from_error(e, |detail| Problem::LanUnreachable {
                detail,
            })),
            Err(_) => Err(Problem::LanUnreachable {
                detail: "the LAN host did not answer".into(),
            }),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.signal
            .conn
            .close(0u32.into(), b"LAN controller stopped");
    }
}

pub(crate) async fn connect(cfg: &LinkConfig, address: &str) -> Result<Connection, Problem> {
    let address = parse_address(address)
        .ok_or(Problem::InvalidLanAddress)?
        .to_string();
    let lan_cfg = LinkConfig::new(&address, "", &cfg.name, &cfg.os).map_err(Problem::failed)?;
    let result = tokio::time::timeout(STEP_TIMEOUT, async {
        let mut signal = net::connect_signal_with(
            &lan_cfg,
            Role::Controller,
            None,
            Dial {
                alpn: ALPN,
                timeout: Duration::from_secs(3),
            },
        )
        .await?;
        let mut buf = Vec::new();
        if !read_frame(&mut signal.recv, MAX_INFO, &mut buf).await? {
            bail!("the LAN host did not send its identity binding");
        }
        let host: HostInfo = serde_json::from_slice(&buf)?;
        if !(100_000..=999_999).contains(&host.code) {
            bail!("invalid LAN identity binding");
        }
        let (router, task) = Router::start_lan(&signal.conn);
        Ok::<_, anyhow::Error>(Connection {
            address: address.clone(),
            code: host.code,
            signal,
            router,
            _router: task,
        })
    })
    .await;
    match result {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => {
            let p = Problem::from_error(e, |detail| Problem::LanUnreachable { detail });
            Err(match p {
                Problem::ServerUnreachable { detail } => Problem::LanUnreachable { detail },
                other => other,
            })
        }
        Err(_) => Err(Problem::LanUnreachable {
            detail: format!("{address} did not respond"),
        }),
    }
}
