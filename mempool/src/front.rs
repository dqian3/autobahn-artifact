use consensus::messages::Transaction;
use crypto::{Hash, Signature, SignatureService};
use ed25519_dalek::ed25519;


use futures::stream::StreamExt as _;
use log::{debug, warn};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::Sender;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub struct Front {
    address: SocketAddr,
    deliver: Sender<Transaction>,
    signature_service: SignatureService
}

impl Front {
    pub fn new(address: SocketAddr, deliver: Sender<Transaction>, signature_service: SignatureService) -> Self {
        Self { address, deliver, signature_service }
    }

    // For each incoming request, we spawn a new worker responsible to receive
    // messages and replay them through the provided deliver channel.
    pub async fn run(&self) {
        let listener = TcpListener::bind(&self.address)
            .await
            .expect("Failed to bind to TCP port");

        debug!("Listening for client transactions on {}", self.address);
        loop {
            let (socket, peer) = match listener.accept().await {
                Ok(value) => value,
                Err(e) => {
                    warn!("Failed to connect with client: {}", e);
                    continue;
                }
            };
            debug!("Connection established with client {}", peer);
            Self::spawn_worker(socket, peer, self.deliver.clone()).await;
        }
    }

    async fn spawn_worker(socket: TcpStream, peer: SocketAddr, deliver: Sender<Transaction>) {
        tokio::spawn(async move {
            let mut transport = Framed::new(socket, LengthDelimitedCodec::new());
            while let Some(frame) = transport.next().await {
                match frame {
                    Ok(x) => {
                        // Verify client signature here
                        let (msg, sig) = x.split_at(x.len() - 64); 
                        
                        let digest = msg.digest();

                        let signature = ed25519::signature::Signature::from_bytes(sig).expect("Failed to create sig");
                        let key = ed25519_dalek::PublicKey::from_bytes(b"").expect("Failed to load pub key");
                        
                        match key.verify_strict(&digest.0, &signature) {
                            Ok(()) => {
                                deliver.send(x.to_vec()).await.expect("Core channel closed");
                            }
                            Err(e) => {
                                warn!("Failed to verify client transaction {}", e);
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to receive client transaction: {}", e);
                        return;
                    }
                }
            }
            debug!("Connection closed by client {}", peer);
        });
    }
}
