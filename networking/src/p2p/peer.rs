// Copyright (c) SimpleStaking and Tezedge Contributors
// SPDX-License-Identifier: MIT

use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use failure::{Error, Fail};
use futures::lock::Mutex;
use riker::actors::*;
use slog::{debug, info, Logger, o, trace, warn};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::time::timeout;

use crypto::crypto_box::{CryptoKey, PrecomputedKey, PublicKey};
use crypto::hash::{CryptoboxPublicKeyHash, HashType};
use crypto::nonce::{self, Nonce, NoncePair};
use tezos_encoding::binary_reader::BinaryReaderError;
use tezos_identity::Identity;
use tezos_messages::p2p::binary_message::{BinaryChunk, BinaryChunkError, BinaryMessage};
use tezos_messages::p2p::encoding::ack::{NackInfo, NackMotive};
use tezos_messages::p2p::encoding::prelude::*;

use crate::p2p::network_channel::NetworkChannelMsg;
use crate::PeerId;

use super::network_channel::{NetworkChannelRef, NetworkChannelTopic, PeerBootstrapFailed, PeerMessageReceived};
use super::stream::{EncryptedMessageReader, EncryptedMessageWriter, MessageStream, StreamError};

const IO_TIMEOUT: Duration = Duration::from_secs(6);
/// There is a 90-second timeout for ping peers with GetCurrentHead
const READ_TIMEOUT_LONG: Duration = Duration::from_secs(120);

static ACTOR_ID_GENERATOR: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Fail)]
pub enum PeerError {
    #[fail(display = "Unsupported protocol - supported_version: {} vs. {}", supported_version, incompatible_versions)]
    UnsupportedProtocol {
        supported_version: String,
        incompatible_versions: String,
    },
    #[fail(display = "Received NACK from remote peer")]
    NackReceived,
    #[fail(display = "Received NACK from remote peer with info: {:?}", nack_info)]
    NackWithMotiveReceived {
        nack_info: NackInfo
    },
    #[fail(display = "Network error: {}, reason: {}", message, error)]
    NetworkError {
        error: Error,
        message: &'static str,
    },
    #[fail(display = "Message serialization error, reason: {}", error)]
    SerializationError {
        error: tezos_encoding::ser::Error
    },
    #[fail(display = "Message deserialization error, reason: {}", error)]
    DeserializationError {
        error: BinaryReaderError
    },
    #[fail(display = "Crypto error, reason: {}", error)]
    CryptoError {
        error: crypto::CryptoError
    },
}

impl From<tezos_encoding::ser::Error> for PeerError {
    fn from(error: tezos_encoding::ser::Error) -> Self {
        PeerError::SerializationError { error }
    }
}

impl From<BinaryReaderError> for PeerError {
    fn from(error: BinaryReaderError) -> Self {
        PeerError::DeserializationError { error }
    }
}

impl From<std::io::Error> for PeerError {
    fn from(error: std::io::Error) -> Self {
        PeerError::NetworkError { error: error.into(), message: "Network error" }
    }
}

impl From<StreamError> for PeerError {
    fn from(error: StreamError) -> Self {
        PeerError::NetworkError { error: error.into(), message: "Stream error" }
    }
}

impl From<BinaryChunkError> for PeerError {
    fn from(error: BinaryChunkError) -> Self {
        PeerError::NetworkError { error: error.into(), message: "Binary chunk error" }
    }
}

impl From<crypto::CryptoError> for PeerError {
    fn from(error: crypto::CryptoError) -> Self {
        PeerError::CryptoError { error }
    }
}

impl From<tokio::time::Elapsed> for PeerError {
    fn from(timeout: tokio::time::Elapsed) -> Self {
        PeerError::NetworkError {
            message: "Connection timeout",
            error: timeout.into(),
        }
    }
}

/// Commands peer actor to initialize bootstrapping process with a remote peer.
#[derive(Clone, Debug)]
pub struct Bootstrap {
    stream: Arc<Mutex<Option<TcpStream>>>,
    address: SocketAddr,
    incoming: bool,
    disable_mempool: bool,
    private_node: bool,
}

impl Bootstrap {
    pub fn incoming(stream: Arc<Mutex<Option<TcpStream>>>, address: SocketAddr, disable_mempool: bool, private_node: bool) -> Self {
        Bootstrap { stream, address, incoming: true, disable_mempool, private_node }
    }

    pub fn outgoing(stream: TcpStream, address: SocketAddr, disable_mempool: bool, private_node: bool) -> Self {
        Bootstrap { stream: Arc::new(Mutex::new(Some(stream))), address, incoming: false, disable_mempool, private_node }
    }
}

/// Commands peer actor to send a p2p message to a remote peer.
#[derive(Clone, Debug)]
pub struct SendMessage {
    /// Message is wrapped in `Arc` to avoid excessive cloning.
    message: Arc<PeerMessageResponse>
}

impl SendMessage {
    pub fn new(message: Arc<PeerMessageResponse>) -> Self {
        SendMessage { message }
    }
}

#[derive(Clone)]
struct Network {
    /// Message receiver boolean indicating whether
    /// more messages should be received from network
    rx_run: Arc<AtomicBool>,
    /// Message sender
    tx: Arc<Mutex<Option<EncryptedMessageWriter>>>,
    /// Socket address of the peer
    socket_address: SocketAddr,
}

/// Local node info
pub struct Local {
    /// port where remote node can establish new connection
    listener_port: u16,
    /// Our node identity
    identity: Arc<Identity>,
    /// version of network protocol
    version: Arc<NetworkVersion>,
}

impl Local {
    pub fn new(listener_port: u16, identity: Arc<Identity>, network_version: Arc<NetworkVersion>) -> Self {
        Local {
            listener_port,
            identity,
            version: network_version,
        }
    }
}

pub type PeerRef = ActorRef<PeerMsg>;

/// Represents a single p2p peer.
#[actor(Bootstrap, SendMessage)]
pub struct Peer {
    /// All events generated by the peer will end up in this channel
    network_channel: NetworkChannelRef,
    /// Local node info
    local: Arc<Local>,
    /// Network IO
    net: Network,
    /// Tokio task executor
    tokio_executor: Handle,
    /// IP address of the remote peer
    remote_addr: SocketAddr,
}

impl Peer {
    /// Create instance of a peer actor.
    pub fn actor(sys: &impl ActorRefFactory,
                 network_channel: NetworkChannelRef,
                 listener_port: u16,
                 node_identity: Arc<Identity>,
                 version: Arc<NetworkVersion>,
                 tokio_executor: Handle,
                 socket_address: &SocketAddr) -> Result<PeerRef, CreateError>
    {
        let info = Local::new(listener_port, node_identity, version);
        let props = Props::new_args::<Peer, _>((network_channel, Arc::new(info), tokio_executor, *socket_address));
        let actor_id = ACTOR_ID_GENERATOR.fetch_add(1, Ordering::SeqCst);
        sys.actor_of_props(&format!("peer-{}", actor_id), props)
    }
}

impl ActorFactoryArgs<(NetworkChannelRef, Arc<Local>, Handle, SocketAddr)> for Peer {
    fn create_args((event_channel, info, tokio_executor, socket_address): (NetworkChannelRef, Arc<Local>, Handle, SocketAddr)) -> Self {
        Peer {
            network_channel: event_channel,
            local: info,
            net: Network {
                rx_run: Arc::new(AtomicBool::new(false)),
                tx: Arc::new(Mutex::new(None)),
                socket_address,
            },
            tokio_executor,
            remote_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0)),
        }
    }
}

impl Actor for Peer {
    type Msg = PeerMsg;

    fn post_stop(&mut self) {
        self.net.rx_run.store(false, Ordering::Release);
    }

    fn recv(&mut self, ctx: &Context<Self::Msg>, msg: Self::Msg, sender: Sender) {
        // Use the respective Receive<T> implementation
        self.receive(ctx, msg, sender);
    }
}

impl Receive<Bootstrap> for Peer {
    type Msg = PeerMsg;

    fn receive(&mut self, ctx: &Context<Self::Msg>, msg: Bootstrap, _sender: Sender) {
        let info = self.local.clone();
        let myself = ctx.myself();
        let system = ctx.system.clone();
        let net = self.net.clone();
        let network_channel = self.network_channel.clone();
        self.remote_addr = msg.address;

        self.tokio_executor.spawn(async move {
            async fn setup_net(net: &Network, tx: EncryptedMessageWriter) {
                net.rx_run.store(true, Ordering::Release);
                *net.tx.lock().await = Some(tx);
            }

            let peer_address = msg.address;
            debug!(system.log(), "Bootstrapping"; "ip" => &peer_address, "peer" => myself.name(), "peer_uri" => myself.uri().to_string());
            match bootstrap(msg, info, &system.log()).await {
                Ok(BootstrapOutput(rx, tx, peer_public_key_hash, peer_id_marker, peer_metadata)) => {
                    // prepare PeerId
                    let peer_id = PeerId::new(myself.clone(), peer_public_key_hash, peer_id_marker, peer_address);
                    let log = {
                        let myself_name = myself.name().to_string();
                        let myself_uri = myself.uri().to_string();
                        system.log().new(slog::o!("peer_id" => peer_id.peer_id_marker.clone(), "peer_ip" => peer_address.to_string(), "peer" => myself_name, "peer_uri" => myself_uri))
                    };
                    debug!(log, "Bootstrap successful"; "peer_metadata" => format!("{:?}", &peer_metadata));

                    // setup encryption writer
                    setup_net(&net, tx).await;

                    // notify that peer was bootstrapped successfully
                    network_channel.tell(Publish {
                        msg: NetworkChannelMsg::PeerBootstrapped(Arc::new(peer_id), Arc::new(peer_metadata)),
                        topic: NetworkChannelTopic::NetworkEvents.into(),
                    }, Some(myself.clone().into()));

                    // begin to process incoming messages in a loop
                    begin_process_incoming(rx, net, myself.clone(), network_channel, log).await;

                    // connection to peer was closed, stop this actor
                    system.stop(myself);
                }
                Err(err) => {
                    warn!(system.log(), "Connection to peer failed"; "reason" => format!("{}", &err), "ip" => &peer_address, "peer" => myself.name(), "peer_uri" => myself.uri().to_string());

                    let potential_peers = match err {
                        PeerError::NackWithMotiveReceived { nack_info } => Some(nack_info.potential_peers_to_connect().clone()),
                        _ => None
                    };

                    // notify that peer failed at bootstrap process
                    network_channel.tell(Publish {
                        msg: NetworkChannelMsg::ProcessFailedBootstrapAddress(
                            PeerBootstrapFailed {
                                address: peer_address,
                                potential_peers_to_connect: potential_peers,
                            }
                        ),
                        topic: NetworkChannelTopic::NetworkCommands.into(),
                    }, Some(myself.clone().into()));

                    system.stop(myself);
                }
            }
        });
    }
}

impl Receive<SendMessage> for Peer {
    type Msg = PeerMsg;

    fn receive(&mut self, ctx: &Context<Self::Msg>, msg: SendMessage, _sender: Sender) {
        let system = ctx.system.clone();
        let myself = ctx.myself();
        let tx = self.net.tx.clone();
        self.tokio_executor.spawn(async move {
            let mut tx_lock = tx.lock().await;
            if let Some(tx) = tx_lock.as_mut() {
                let write_result = timeout(IO_TIMEOUT, tx.write_message(msg.message.as_ref())).await;
                // release mutex as soon as possible
                drop(tx_lock);

                match write_result {
                    Ok(write_result) => {
                        if let Err(e) = write_result {
                            warn!(system.log(), "Failed to send message"; "reason" => e);
                            system.stop(myself);
                        }
                    }
                    Err(_) => {
                        warn!(system.log(), "Failed to send message"; "reason" => "timeout");
                        system.stop(myself);
                    }
                }
            }
        });
    }
}

/// Output values of the successful bootstrap process
pub struct BootstrapOutput(pub EncryptedMessageReader, pub EncryptedMessageWriter, pub CryptoboxPublicKeyHash, pub String, pub MetadataMessage);

pub async fn bootstrap(
    msg: Bootstrap,
    info: Arc<Local>,
    log: &Logger,
) -> Result<BootstrapOutput, PeerError> {
    let (mut msg_rx, mut msg_tx) = {
        let stream = msg.stream.lock().await.take().expect("Someone took ownership of the socket before the Peer");
        let msg_reader: MessageStream = stream.into();
        msg_reader.split()
    };

    let supported_protocol_version = &info.version;

    // send connection message
    let connection_message = ConnectionMessage::new(
        info.listener_port,
        &info.identity.public_key,
        &info.identity.proof_of_work_stamp,
        Nonce::random(),
        vec![supported_protocol_version.as_ref().clone()])?;
    let connection_message_sent = {
        let connection_message_bytes = BinaryChunk::from_content(&connection_message.as_bytes()?)?;
        match timeout(IO_TIMEOUT, msg_tx.write_message(&connection_message_bytes)).await? {
            Ok(_) => connection_message_bytes,
            Err(e) => return Err(PeerError::NetworkError { error: e.into(), message: "Failed to transfer connection message" })
        }
    };

    // receive connection message
    let received_connection_message_bytes = match timeout(IO_TIMEOUT, msg_rx.read_message()).await? {
        Ok(msg) => msg,
        Err(e) => return Err(PeerError::NetworkError { error: e.into(), message: "No response to connection message was received" })
    };

    let connection_message = ConnectionMessage::from_bytes(received_connection_message_bytes.content())?;

    // generate local and remote nonce
    let NoncePair { local: nonce_local, remote: nonce_remote } = generate_nonces(&connection_message_sent, &received_connection_message_bytes, msg.incoming);

    // create PublicKey from received bytes from remote peer
    let peer_public_key = PublicKey::from_bytes(connection_message.public_key())?;

    // pre-compute encryption key
    let precomputed_key = PrecomputedKey::precompute(&peer_public_key, &info.identity.secret_key);

    // generate public key hash for PublicKey, which will be used as a peer_id
    let peer_public_key_hash = peer_public_key.public_key_hash();
    let peer_id_marker = HashType::CryptoboxPublicKeyHash.hash_to_b58check(&peer_public_key_hash);
    let log = log.new(o!("peer_id" => peer_id_marker.clone()));

    // from now on all messages will be encrypted
    let mut msg_tx = EncryptedMessageWriter::new(
        msg_tx,
        precomputed_key.clone(),
        nonce_local,
        log.clone(),
    );
    let mut msg_rx = EncryptedMessageReader::new(
        msg_rx,
        precomputed_key,
        nonce_remote,
        log.clone(),
    );

    let connecting_to_self = peer_public_key == info.identity.public_key;
    if connecting_to_self {
        debug!(log, "Detected self connection");
        // treat as if nack was received
        return Err(PeerError::NackWithMotiveReceived { nack_info: NackInfo::new(NackMotive::AlreadyConnected, &[]) });
    }

    // send metadata
    let metadata = MetadataMessage::new(msg.disable_mempool, msg.private_node);
    timeout(IO_TIMEOUT, msg_tx.write_message(&metadata)).await??;

    // receive metadata
    let metadata_received = timeout(IO_TIMEOUT, msg_rx.read_message::<MetadataMessage>()).await??;
    debug!(log, "Received remote peer metadata"; "disable_mempool" => metadata_received.disable_mempool(), "private_node" => metadata_received.private_node());

    let protocol_not_supported = !connection_message.versions().iter().any(|version| supported_protocol_version.supports(version));
    if protocol_not_supported {
        // send nack
        timeout(IO_TIMEOUT, msg_tx.write_message(&AckMessage::NackV0)).await??;

        return Err(
            PeerError::UnsupportedProtocol {
                supported_version: format!("{:?}", &supported_protocol_version),
                incompatible_versions: format!("{:?}", &connection_message.versions()),
            }
        );
    }

    // send ack
    timeout(IO_TIMEOUT, msg_tx.write_message(&AckMessage::Ack)).await??;

    // receive ack
    let ack_received = timeout(IO_TIMEOUT, msg_rx.read_message()).await??;

    match ack_received {
        AckMessage::Ack => {
            debug!(log, "Received ACK");
            Ok(BootstrapOutput(msg_rx, msg_tx, peer_public_key_hash, peer_id_marker, metadata_received))
        }
        AckMessage::NackV0 => {
            debug!(log, "Received NACK");
            Err(PeerError::NackReceived)
        }
        AckMessage::Nack(nack_info) => {
            debug!(log, "Received NACK with info: {:?}", nack_info);
            Err(PeerError::NackWithMotiveReceived { nack_info })
        }
    }
}


/// Generate nonces (sent and recv encoding must be with length bytes also)
///
/// local_nonce is used for writing crypto messages to other peers
/// remote_nonce is used for reading crypto messages from other peers
fn generate_nonces(sent_msg: &BinaryChunk, recv_msg: &BinaryChunk, incoming: bool) -> NoncePair {
    nonce::generate_nonces(sent_msg.raw(), recv_msg.raw(), incoming)
}

/// Start to process incoming data
async fn begin_process_incoming(mut rx: EncryptedMessageReader, net: Network, myself: PeerRef, event_channel: NetworkChannelRef, log: Logger) {
    info!(log, "Starting to accept messages");

    while net.rx_run.load(Ordering::Acquire) {
        match timeout(READ_TIMEOUT_LONG, rx.read_message::<PeerMessageResponse>()).await {
            Ok(res) => match res {
                Ok(msg) => {
                    let should_broadcast_message = net.rx_run.load(Ordering::Acquire);
                    if should_broadcast_message {
                        trace!(log, "Message parsed successfully"; "msg" => format!("{:?}", &msg));
                        event_channel.tell(
                            Publish {
                                msg: PeerMessageReceived {
                                    peer: myself.clone(),
                                    message: Arc::new(msg),
                                }.into(),
                                topic: NetworkChannelTopic::NetworkEvents.into(),
                            }, Some(myself.clone().into()));
                    }
                }
                Err(e) => {
                    if let StreamError::DeserializationError { error: BinaryReaderError::UnsupportedTag { tag } } = e {
                        warn!(log, "Messages with unsupported tags are ignored"; "tag" => tag);
                    } else {
                        warn!(log, "Failed to read peer message"; "reason" => e);
                        break;
                    }
                }
            }
            Err(_) => {
                warn!(log, "Peer message read timed out"; "secs" => READ_TIMEOUT_LONG.as_secs());
                break;
            }
        }
    }

    debug!(log, "Shutting down peer connection");
    let mut tx_lock = net.tx.lock().await;
    if let Some(tx) = tx_lock.take() {
        let socket = rx.unsplit(tx);
        match socket.shutdown(Shutdown::Both) {
            Ok(()) => debug!(log, "Connection shutdown successful"; "socket" => format!("{:?}", socket)),
            Err(err) => debug!(log, "Failed to shutdown connection"; "err" => format!("{:?}", err), "socket" => format!("{:?}", socket)),
        }
    }

    info!(log, "Stopped to accept messages");
}
