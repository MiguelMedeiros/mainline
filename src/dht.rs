//! Dht node.

use std::{net::SocketAddr, thread};

use bytes::Bytes;
use flume::{Receiver, Sender};

use crate::{
    common::{
        hash_immutable,
        messages::{
            AnnouncePeerRequestArguments, GetPeersRequestArguments, GetValueRequestArguments,
            PutImmutableRequestArguments, PutMutableRequestArguments, PutRequestSpecific,
            RequestTypeSpecific,
        },
        Id, MutableItem, PutResult, ResponseSender,
    },
    rpc::Rpc,
    Result,
};

#[derive(Debug, Clone)]
/// Mainlin eDht node.
pub struct Dht(pub(crate) Sender<ActorMessage>);

pub struct Builder {
    settings: DhtSettings,
}

impl Builder {
    /// Create a Dht node.
    pub fn build(&self) -> Result<Dht> {
        Dht::new(self.settings.clone())
    }

    /// Create a full DHT node that accepts requests, and acts as a routing and storage node.
    pub fn server(mut self) -> Self {
        self.settings.read_only = false;
        self
    }

    /// Set bootstrapping nodes
    pub fn bootstrap(mut self, bootstrap: &[String]) -> Self {
        self.settings.bootstrap = Some(bootstrap.to_owned());
        self
    }

    /// Set the port to listen on.
    pub fn port(mut self, port: u16) -> Self {
        self.settings.port = Some(port);
        self
    }
}

#[derive(Debug, Clone)]
/// Dht settings
pub struct DhtSettings {
    pub bootstrap: Option<Vec<String>>,
    pub read_only: bool,
    pub port: Option<u16>,
}

impl Default for DhtSettings {
    fn default() -> Self {
        DhtSettings {
            bootstrap: None,
            read_only: true,
            port: None,
        }
    }
}

impl Dht {
    /// Returns a builder to edit settings before creating a Dht node.
    pub fn builder() -> Builder {
        Builder {
            settings: DhtSettings::default(),
        }
    }

    /// Create a new DHT client with default bootstrap nodes.
    pub fn client() -> Result<Self> {
        Dht::builder().build()
    }

    /// Create a new DHT server that serves as a routing node and accepts storage requests
    /// for peers and other arbitrary data.
    ///
    /// Note: this is only useful if the node has a public IP address and is able to receive
    /// incoming udp packets.
    pub fn server() -> Result<Self> {
        Dht::builder().server().build()
    }

    /// Create a new Dht node.
    ///
    /// Could return an error if it failed to bind to the specified
    /// port or other io errors while binding the udp socket.
    pub fn new(settings: DhtSettings) -> Result<Self> {
        let (sender, receiver) = flume::bounded(32);

        let mut rpc = Rpc::new()?.with_read_only(settings.read_only);

        if let Some(bootstrap) = settings.bootstrap {
            rpc = rpc.with_bootstrap(bootstrap);
        }

        if let Some(port) = settings.port {
            rpc = rpc.with_port(port)?;
        }

        thread::spawn(move || run(rpc, receiver));

        Ok(Dht(sender))
    }

    // === Getters ===

    /// Returns the local address of the udp socket this node is listening on.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        let (sender, receiver) = flume::bounded::<SocketAddr>(1);

        self.0.send(ActorMessage::LocalAddress(sender))?;

        Ok(receiver.recv()?)
    }

    // === Public Methods ===

    /// Shutdown the actor thread loop.
    pub fn shutdown(&self) -> Result<()> {
        let (sender, receiver) = flume::bounded::<()>(1);

        self.0.send(ActorMessage::Shutdown(sender))?;

        Ok(receiver.recv()?)
    }

    // === Peers ===

    /// Get peers for a given infohash.
    ///
    /// Note: each node of the network will only return a _random_ subset (usually 20)
    /// of the total peers it has for a given infohash, so if you are getting responses
    /// from 20 nodes, you can expect up to 400 peers in total, but if there are more
    /// announced peers on that infohash, you are likely to miss some, the logic here
    /// for Bittorrent is that any peer will introduce you to more peers through "peer exchange"
    /// so if you are implementing something different from Bittorrent, you might want
    /// to implement your own logic for gossipping more peers after you discover the first ones.
    pub fn get_peers(&self, info_hash: Id) -> Result<flume::IntoIter<SocketAddr>> {
        // Get requests use unbounded channels to avoid blocking in the run loop.
        // Other requests like put_* and getters don't need that and is ok with
        // bounded channel with 1 capacity since it only ever sends one message back.
        //
        // So, if it is a ResponseMessage<_>, it should be unbounded, otherwise bounded.
        let (sender, receiver) = flume::unbounded::<SocketAddr>();

        let request = RequestTypeSpecific::GetPeers(GetPeersRequestArguments { info_hash });

        self.0.send(ActorMessage::Get(
            info_hash,
            request,
            ResponseSender::Peer(sender),
        ))?;

        Ok(receiver.into_iter())
    }

    /// Announce a peer for a given infohash.
    ///
    /// The peer will be announced on this process IP.
    /// If explicit port is passed, it will be used, otherwise the port will be implicitly
    /// assumed by remote nodes to be the same ase port they recieved the request from.
    pub fn announce_peer(&self, info_hash: Id, port: Option<u16>) -> Result<Id> {
        let (sender, receiver) = flume::bounded::<PutResult>(1);

        let (port, implied_port) = match port {
            Some(port) => (port, None),
            None => (0, Some(true)),
        };

        let request = PutRequestSpecific::AnnouncePeer(AnnouncePeerRequestArguments {
            info_hash,
            port,
            implied_port,
        });

        self.0.send(ActorMessage::Put(info_hash, request, sender))?;

        receiver.recv()?
    }

    // === Immutable data ===

    /// Get an Immutable data by its sha1 hash.
    pub fn get_immutable(&self, target: Id) -> Result<Bytes> {
        let (sender, receiver) = flume::unbounded::<Bytes>();

        let request = RequestTypeSpecific::GetValue(GetValueRequestArguments {
            target,
            seq: None,
            salt: None,
        });

        self.0.send(ActorMessage::Get(
            target,
            request,
            ResponseSender::Immutable(sender),
        ))?;

        Ok(receiver.recv()?)
    }

    /// Put an immutable data to the DHT.
    pub fn put_immutable(&self, value: Bytes) -> Result<Id> {
        let target = Id::from_bytes(hash_immutable(&value)).unwrap();

        let (sender, receiver) = flume::bounded::<PutResult>(1);

        let request = PutRequestSpecific::PutImmutable(PutImmutableRequestArguments {
            target,
            v: value.clone().into(),
        });

        self.0.send(ActorMessage::Put(target, request, sender))?;

        receiver.recv()?
    }

    // === Mutable data ===

    /// Get a mutable data by its public_key and optional salt.
    pub fn get_mutable(
        &self,
        public_key: &[u8; 32],
        salt: Option<Bytes>,
        seq: Option<i64>,
    ) -> Result<flume::IntoIter<MutableItem>> {
        let target = MutableItem::target_from_key(public_key, &salt);

        let (sender, receiver) = flume::unbounded::<MutableItem>();

        let request = RequestTypeSpecific::GetValue(GetValueRequestArguments { target, seq, salt });

        let _ = self.0.send(ActorMessage::Get(
            target,
            request,
            ResponseSender::Mutable(sender),
        ));

        Ok(receiver.into_iter())
    }

    /// Put a mutable data to the DHT.
    pub fn put_mutable(&self, item: MutableItem) -> Result<Id> {
        let (sender, receiver) = flume::bounded::<PutResult>(1);

        let request = PutRequestSpecific::PutMutable(PutMutableRequestArguments {
            target: *item.target(),
            v: item.value().clone().into(),
            k: item.key().to_vec(),
            seq: *item.seq(),
            sig: item.signature().to_vec(),
            salt: item.salt().clone().map(|s| s.to_vec()),
            cas: *item.cas(),
        });

        let _ = self
            .0
            .send(ActorMessage::Put(*item.target(), request, sender));

        receiver.recv()?
    }
}

fn run(mut rpc: Rpc, receiver: Receiver<ActorMessage>) {
    loop {
        if let Ok(actor_message) = receiver.try_recv() {
            match actor_message {
                ActorMessage::Shutdown(sender) => {
                    let _ = sender.send(());
                    drop(receiver);
                    break;
                }
                ActorMessage::LocalAddress(sender) => {
                    let _ = sender.send(rpc.local_addr());
                }
                ActorMessage::Put(target, request, sender) => {
                    rpc.put(target, request, Some(sender));
                }
                ActorMessage::Get(target, request, sender) => {
                    rpc.get(target, request, Some(sender))
                }
            }
        }

        rpc.tick();
    }
}

pub enum ActorMessage {
    LocalAddress(Sender<SocketAddr>),

    Put(Id, PutRequestSpecific, Sender<PutResult>),
    Get(Id, RequestTypeSpecific, ResponseSender),
    Shutdown(Sender<()>),
}

/// Create a testnet of Dht nodes to run tests against instead of the real mainline network.
#[derive(Debug)]
pub struct Testnet {
    pub bootstrap: Vec<String>,
    pub nodes: Vec<Dht>,
}

impl Testnet {
    pub fn new(count: usize) -> Self {
        let mut nodes: Vec<Dht> = vec![];
        let mut bootstrap = vec![];

        for i in 0..count {
            if i == 0 {
                let node = Dht::builder().server().bootstrap(&[]).build().unwrap();

                let addr = node.local_addr().unwrap();
                bootstrap.push(format!("127.0.0.1:{}", addr.port()));

                nodes.push(node)
            } else {
                let node = Dht::builder()
                    .server()
                    .bootstrap(&bootstrap)
                    .build()
                    .unwrap();
                nodes.push(node)
            }
        }

        Self { bootstrap, nodes }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use ed25519_dalek::SigningKey;

    use super::*;

    #[test]
    fn shutdown() {
        let dht = Dht::client().unwrap();

        dht.local_addr().unwrap();

        let a = dht.clone();

        dht.shutdown().unwrap();

        let local_addr = a.local_addr();
        assert!(local_addr.is_err());
    }

    #[test]
    fn bind_twice() {
        let a = Dht::client().unwrap();
        let result = Dht::builder()
            .port(a.local_addr().unwrap().port())
            .server()
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn announce_get_peer() {
        let testnet = Testnet::new(10);

        let a = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();
        let b = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();

        let info_hash = Id::random();

        a.announce_peer(info_hash, Some(45555))
            .expect("failed to announce");

        let peer = b.get_peers(info_hash).unwrap().next().expect("No peers");

        assert_eq!(peer.port(), 45555);
    }

    #[test]
    fn put_get_immutable() {
        let testnet = Testnet::new(10);

        let a = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();
        let b = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();

        let value: Bytes = "Hello World!".into();
        let expected_target = Id::from_str("e5f96f6f38320f0f33959cb4d3d656452117aadb").unwrap();

        let target = a.put_immutable(value.clone()).unwrap();
        assert_eq!(target, expected_target);

        let response = b.get_immutable(target).unwrap();
        assert_eq!(response, value);
    }

    #[test]
    fn put_get_mutable() {
        let testnet = Testnet::new(10);

        let a = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();
        let b = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();

        let signer = SigningKey::from_bytes(&[
            56, 171, 62, 85, 105, 58, 155, 209, 189, 8, 59, 109, 137, 84, 84, 201, 221, 115, 7,
            228, 127, 70, 4, 204, 182, 64, 77, 98, 92, 215, 27, 103,
        ]);

        let seq = 1000;
        let value: Bytes = "Hello World!".into();

        let item = MutableItem::new(signer.clone(), value, seq, None);

        a.put_mutable(item.clone()).unwrap();

        let response = b
            .get_mutable(signer.verifying_key().as_bytes(), None, None)
            .unwrap()
            .next()
            .expect("No mutable values");

        assert_eq!(&response, &item);
    }

    #[test]
    fn put_get_mutable_no_more_recent_value() {
        let testnet = Testnet::new(10);

        let a = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();
        let b = Dht::builder()
            .bootstrap(&testnet.bootstrap)
            .build()
            .unwrap();

        let signer = SigningKey::from_bytes(&[
            56, 171, 62, 85, 105, 58, 155, 209, 189, 8, 59, 109, 137, 84, 84, 201, 221, 115, 7,
            228, 127, 70, 4, 204, 182, 64, 77, 98, 92, 215, 27, 103,
        ]);

        let seq = 1000;
        let value: Bytes = "Hello World!".into();

        let item = MutableItem::new(signer.clone(), value, seq, None);

        a.put_mutable(item.clone()).unwrap();

        let response = b
            .get_mutable(signer.verifying_key().as_bytes(), None, Some(seq))
            .unwrap()
            .next();

        assert!(&response.is_none());
    }
}
