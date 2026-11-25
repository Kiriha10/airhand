//! Real QUIC/Noise sessions with no relay. The platform records requests and emits a
//! synthetic video packet; these tests verify transport/authentication, not hardware codecs.

use std::sync::Arc;
use std::time::Duration;

use airhand_link::controller::{
    Command, ConnectOptions, ControllerService, Frontend, SessionEvent, SessionInfo, SessionUi,
    UiClosed,
};
use airhand_link::host::{
    Acks, Gate, Paired, Pairing, PairingState, PairingStore, Platform, Request, Senders, Start,
};
use airhand_link::lan::{Host, parse_address};
use airhand_link::secure::{Identity, PairingAttempt, hex};
use airhand_link::tasks::OwnedTask;
use airhand_link::{LinkConfig, Problem, ServerState};
use airhand_proto::handshake::PublicKey;
use airhand_proto::media::{self, VideoHeader};
use airhand_proto::session::{Codec, DisplayInfo, HostHello, HostMsg, InputEvent};
use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(12);
const PAIRING: &str = "624913";

struct Store;
impl PairingStore for Store {
    fn changed(&self, _: &PairingState) {}
}

struct TestGate {
    identity: Identity,
    pairing: Pairing,
}
impl Gate for TestGate {
    fn identity(&self) -> Identity {
        self.identity.clone()
    }
    fn is_known(&self, key: &PublicKey) -> bool {
        self.pairing.is_known(key)
    }
    fn begin_pairing(&self) -> Option<Box<dyn PairingAttempt + '_>> {
        self.pairing.begin()
    }
}

struct NoAcks;
impl Acks for NoAcks {
    fn ack(&self, _: u8, _: u32, _: u64) {}
}

struct TestPlatform {
    senders: Senders,
    requests: mpsc::UnboundedSender<Request>,
}
impl Platform for TestPlatform {
    type Event = ();
    fn acks(&self) -> Arc<dyn Acks> {
        Arc::new(NoAcks)
    }
    async fn start(&mut self, _: Start) -> Result<()> {
        self.senders.out.send(&HostMsg::Hello(HostHello {
            hostname: "LAN Windows".into(),
            os: "Windows".into(),
            version: "test".into(),
            displays: vec![DisplayInfo {
                index: 0,
                name: "test".into(),
                x: 0,
                y: 0,
                width: 8,
                height: 8,
                primary: true,
            }],
            current_display: 0,
            codecs: vec![Codec::H264],
            platform: airhand_proto::session::Platform::Windows,
            touch: false,
            actions: vec![],
            shown: vec![0],
            streams: 1,
        }))?;
        let header = VideoHeader {
            stream: 0,
            seq: 1,
            codec: Codec::H264,
            key: true,
            width: 8,
            height: 8,
            capture_us: 0,
            reference: None,
        }
        .encode();
        self.senders.video[0]
            .send(media::frame(&header, &[&[10, 20, 30]]).into())
            .await?;
        Ok(())
    }
    async fn request(&mut self, request: Request) -> Result<()> {
        let _ = self.requests.send(request);
        Ok(())
    }
    async fn next_event(&mut self) {
        std::future::pending().await
    }
    async fn event(&mut self, _: ()) -> Result<()> {
        Ok(())
    }
    async fn stop(self) {}
}

struct Ui {
    events: mpsc::UnboundedSender<SessionEvent>,
    packets: mpsc::UnboundedSender<Vec<u8>>,
}
impl SessionUi for Ui {
    fn event(&self, event: SessionEvent) -> Result<(), UiClosed> {
        self.events.send(event).map_err(|_| UiClosed)
    }
    fn packet(&self, packet: Vec<u8>) -> Result<(), UiClosed> {
        self.packets.send(packet).map_err(|_| UiClosed)
    }
}

struct Harness {
    _host: OwnedTask<()>,
    ctl: ControllerService,
    addr: String,
    key: String,
    ui: Arc<Ui>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
    packets: mpsc::UnboundedReceiver<Vec<u8>>,
    requests: mpsc::UnboundedReceiver<Request>,
    server: mpsc::UnboundedReceiver<ServerState>,
    commands: mpsc::UnboundedSender<airhand_link::host::HostCmd>,
}

impl Harness {
    fn new(known: bool, relay: &str) -> Self {
        Self::with_code(known, relay, Arc::new(|| 582314))
    }

    fn with_code(known: bool, relay: &str, code: Arc<dyn Fn() -> u32 + Send + Sync>) -> Self {
        let me = Identity::generate();
        let host_id = Identity::generate();
        let key = hex(host_id.public());
        let gate = Arc::new(TestGate {
            identity: host_id,
            pairing: Pairing::new(
                Some(PairingState {
                    code: PAIRING.into(),
                    locked: false,
                    paired: if known {
                        vec![Paired {
                            key: hex(me.public()),
                            name: "test controller".into(),
                            since: 0,
                        }]
                    } else {
                        vec![]
                    },
                }),
                Store,
            ),
        });
        let host = Host::bind(
            ([127, 0, 0, 1], 0).into(),
            "LAN Windows".into(),
            "Windows".into(),
            code,
        )
        .unwrap();
        let addr = host.local_addr().unwrap().to_string();
        let (commands, cmd_rx) = mpsc::unbounded_channel();
        let (request_tx, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(host.run(
            move |inc| {
                assert!(inc.lan);
                let gate = gate.clone();
                let requests = request_tx.clone();
                async move {
                    let _ = airhand_link::host::serve(inc, &*gate, 2, 2, |_, senders| {
                        TestPlatform { senders, requests }
                    })
                    .await;
                }
            },
            cmd_rx,
        ));
        let (event_tx, events) = mpsc::unbounded_channel();
        let (packet_tx, packets) = mpsc::unbounded_channel();
        let ui = Arc::new(Ui {
            events: event_tx,
            packets: packet_tx,
        });
        let (server_tx, server) = mpsc::unbounded_channel();
        let ctl = ControllerService::start(
            Arc::new(LinkConfig::new(relay, "", "test controller", "test").unwrap()),
            tokio::runtime::Handle::current(),
            Arc::new(move |s| {
                let _ = server_tx.send(s);
            }),
            Arc::new(|_| {}),
            me,
        );
        Self {
            _host: OwnedTask(task),
            ctl,
            addr,
            key,
            ui,
            events,
            packets,
            requests,
            server,
            commands,
        }
    }

    async fn connect(
        &self,
        pairing: Option<&str>,
        resume: Option<[u8; 16]>,
    ) -> Result<SessionInfo, Problem> {
        timeout(
            WAIT,
            self.ctl.connect(
                "",
                ConnectOptions {
                    lan_addr: Some(self.addr.clone()),
                    pairing: pairing.map(str::to_string),
                    resume,
                    ..Default::default()
                },
                self.ui.clone(),
                Frontend::Browser,
            ),
        )
        .await
        .expect("LAN connect timed out")
    }

    async fn hello(&mut self) {
        timeout(WAIT, async {
            loop {
                match self.events.recv().await.unwrap() {
                    SessionEvent::Hello { .. } => return,
                    SessionEvent::Closed { reason, .. } => panic!("closed before hello: {reason}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    }

    async fn input(&mut self, sid: u32, text: &str) {
        self.ctl
            .command(Command::Input {
                sid,
                events: vec![InputEvent::Text { text: text.into() }],
            })
            .unwrap();
        timeout(WAIT, async {
            while let Some(request) = self.requests.recv().await {
                if let Request::Input(events) = request
                    && events
                        .iter()
                        .any(|e| matches!(e, InputEvent::Text { text: t } if t == text))
                {
                    return;
                }
            }
            panic!("LAN input channel closed");
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn connects_and_transfers_video_and_input_without_any_server_configuration() {
    let mut h = Harness::new(true, "");
    let info = h.connect(None, None).await.unwrap();
    assert_eq!(info.host_key, h.key);
    assert_eq!(airhand_proto::parse_code(&info.code), Some(582314));
    assert_eq!(info.lan_addr.as_deref(), Some(h.addr.as_str()));
    h.hello().await;
    let packet = timeout(WAIT, h.packets.recv()).await.unwrap().unwrap();
    assert!(packet.ends_with(&[10, 20, 30]));
    h.input(info.sid, "local input").await;
    timeout(WAIT, async {
        loop {
            if let SessionEvent::Stats { frames, direct, .. } = h.events.recv().await.unwrap()
                && frames > 0
            {
                assert!(direct);
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn relay_connection_failure_does_not_end_the_lan_session() {
    let mut h = Harness::new(true, "127.0.0.1:1");
    let info = h.connect(None, None).await.unwrap();
    h.hello().await;
    timeout(WAIT, async {
        while let Some(state) = h.server.recv().await {
            if matches!(state, ServerState::Disconnected { .. }) {
                return;
            }
        }
        panic!("no relay failure reported");
    })
    .await
    .unwrap();
    h.input(info.sid, "still connected after relay failure")
        .await;
}

#[tokio::test]
async fn pairing_is_required_and_wrong_pairing_is_rejected_on_lan() {
    let mut h = Harness::new(false, "");
    assert_eq!(
        h.connect(None, None).await.unwrap_err(),
        Problem::PairingRequired
    );
    assert_eq!(
        h.connect(Some("000001"), None).await.unwrap_err(),
        Problem::PairingFailed
    );
    let info = h.connect(Some(PAIRING), None).await.unwrap();
    assert_eq!(info.host_key, h.key);
    h.hello().await;
    h.input(info.sid, "paired LAN input").await;
}

#[tokio::test]
async fn kicking_lan_controllers_revokes_their_reconnect_tokens() {
    let mut h = Harness::new(true, "");
    let info = h.connect(None, None).await.unwrap();
    h.hello().await;
    h.commands
        .send(airhand_link::host::HostCmd::KickAll)
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let SessionEvent::Closed { reason, retry } = h.events.recv().await.unwrap() {
                assert_eq!(reason, Problem::Kicked);
                assert!(!retry);
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        h.connect(None, Some(info.resume)).await.unwrap_err(),
        Problem::Kicked
    );
    h.connect(None, None).await.unwrap();
}

#[tokio::test]
async fn a_lan_peer_cannot_replace_the_saved_host_identity() {
    let h = Harness::new(true, "");
    let wrong = Identity::generate();
    let error = timeout(
        WAIT,
        h.ctl.connect(
            "",
            ConnectOptions {
                lan_addr: Some(h.addr.clone()),
                host_key: Some(hex(wrong.public())),
                ..Default::default()
            },
            h.ui.clone(),
            Frontend::Browser,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, Problem::HostKeyChanged);
}

#[test]
fn only_numeric_local_addresses_are_accepted() {
    assert_eq!(
        parse_address(" 192.168.1.20 ").unwrap().to_string(),
        "192.168.1.20:4434"
    );
    for address in [
        "10.0.0.2:12345",
        "172.16.1.2",
        "169.254.10.20",
        "[fd00::2]:4434",
        "127.0.0.1",
    ] {
        assert!(parse_address(address).is_some(), "{address}");
    }
    for address in [
        "example.com:4434",
        "8.8.8.8:4434",
        "0.0.0.0",
        "224.0.0.1",
        "192.168.1.20:0",
        "192.168.1.2/path",
    ] {
        assert!(parse_address(address).is_none(), "{address}");
    }
}

#[tokio::test]
async fn relay_control_code_changes_do_not_change_the_lan_binding_or_resume_token() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let code = Arc::new(AtomicU32::new(582314));
    let source = code.clone();
    let mut h = Harness::with_code(true, "", Arc::new(move || source.load(Ordering::SeqCst)));
    let first = h.connect(None, None).await.unwrap();
    h.hello().await;
    code.store(654321, Ordering::SeqCst);
    h.input(first.sid, "existing LAN session survives a new relay code")
        .await;
    let next = h.connect(None, Some(first.resume)).await.unwrap();
    assert_eq!(airhand_proto::parse_code(&next.code), Some(582314));
    assert_eq!(next.resume, first.resume);
}
