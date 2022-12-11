use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::sink::SinkExt;

use rand::{prelude::SliceRandom, rngs::OsRng, Rng};
use snarkvm::{
    console::account::address::Address,
    prelude::{FromBytes, Network, Testnet3},
    synthesizer::Block,
};
use snarkos_account::Account;
use snarkos_node_messages::{
    ChallengeRequest, ChallengeResponse, Data, MessageCodec, NodeType, Ping, Pong, PuzzleRequest,
    PuzzleResponse,
};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc,
        mpsc::{Receiver, Sender},
        Mutex,
    },
    task,
    time::{sleep, timeout},
};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use crate::prover::{Prover, ProverEvent, Record};

type Message = snarkos_node_messages::Message<Testnet3>;

pub struct Client {
    pub address: Address<Testnet3>,
    pub servers: Vec<String>,
    sender: Arc<Sender<Message>>,
    receiver: Arc<Mutex<Receiver<Message>>>,
    worker: String,
}

impl Client {
    pub fn init(address: Address<Testnet3>, servers: Vec<String>, worker: String) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel(1024);
        Arc::new(Self {
            address,
            servers,
            sender: Arc::new(sender),
            receiver: Arc::new(Mutex::new(receiver)),
            worker,
        })
    }


    pub fn address(&self) -> Address<Testnet3> {
        self.address.clone()
    }

    pub fn get_worker(&self) -> String {
        self.worker.clone()
    }
    pub fn sender(&self) -> Arc<Sender<Message>> {
        self.sender.clone()
    }

    pub fn receiver(&self) -> Arc<Mutex<Receiver<Message>>> {
        self.receiver.clone()
    }
}

pub fn report(prover: Arc<Prover>, client: Arc<Client>) {
    let receiver = prover.record_receiver();
    task::spawn(async move {
        let receiver = &mut *receiver.lock().await;
        loop {
            tokio::select! {
                Some(message) = receiver.recv() => {
                    let http_client = reqwest::Client::new();
                    let resp = http_client
                        .post(r#"https://record.aleopro.com/record"#)
                        .json(&Record {
                            address: Some(client.address().to_string()),
                            worker: Some(client.get_worker()),
                            total_proofs: message.total_proofs,
                            proof_rate: message.proof_rate,
                            timestamp: message.timestamp
                        })
                        .send()
                        .await;

                         match resp {
                        Ok(_) => {
                            info!("record data success");
                        }
                        Err(_) => {
                            error!("record data failed");
                            continue;
                        }
                    };
                }
            }
        }
    });
}

pub fn start(prover: Arc<Prover>, client: Arc<Client>) {
    task::spawn(async move {
        let receiver = client.receiver();
        let genesis_header = *Block::<Testnet3>::from_bytes_le(Testnet3::genesis_bytes())
            .unwrap()
            .header();
        let connected = Arc::new(AtomicBool::new(false));
        let client_sender = client.sender();

        let connected_req = connected.clone();
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(Testnet3::ANCHOR_TIME as u64)).await;
                if connected_req.load(Ordering::SeqCst) {
                    if let Err(e) = client_sender
                        .send(Message::PuzzleRequest(PuzzleRequest {}))
                        .await
                    {
                        error!("Failed to send puzzle request: {}", e);
                    }
                }
            }
        });

        info!("Created coinbase puzzle request task");

        let rng = &mut OsRng;

        let random_account = Account::new(rng).unwrap();

        loop {
            info!("Connecting to server...");
            let server = client.servers.choose(rng).unwrap();
            match timeout(Duration::from_secs(5), TcpStream::connect(server)).await {
                Ok(socket) => match socket {
                    Ok(socket) => {
                        info!("Connected to {}", server);
                        let mut framed = Framed::new(socket, MessageCodec::default());
                        let challenge_request = Message::ChallengeRequest(ChallengeRequest {
                            version: Message::VERSION,
                            listener_port: 4140,
                            node_type: NodeType::Prover,
                            address: random_account.address(),
                            nonce: rng.gen(),
                        });
                        if let Err(e) = framed.send(challenge_request).await {
                            error!("Error sending challenge request: {}", e);
                        } else {
                            debug!("Sent challenge request");
                        }
                        let receiver = &mut *receiver.lock().await;
                        loop {
                            tokio::select! {
                                Some(message) = receiver.recv() => {
                                    let m = message.clone();
                                    let name = m.name();
                                    info!("Sending {} to beacon", name);
                                    if let Err(e) = framed.send(message).await {
                                        error!("Error sending {}: {:?}", name, e);
                                    }
                                }
                                result = framed.next() => match result {
                                    Some(Ok(message)) => {
                                        debug!("Received {} from beacon", message.name());
                                        match message {
                                            Message::ChallengeRequest(ChallengeRequest {
                                                version,
                                                listener_port: _,
                                                node_type,
                                                address: _,
                                                nonce,
                                            }) => {
                                                if version < Message::VERSION {
                                                    error!("Peer is running an older version of the protocol");
                                                    sleep(Duration::from_secs(5)).await;
                                                    break;
                                                }
                                                if node_type != NodeType::Beacon && node_type != NodeType::Validator {
                                                    error!("Peer is not a beacon or validator");
                                                    sleep(Duration::from_secs(5)).await;
                                                    break;
                                                }
                                                let response = Message::ChallengeResponse(ChallengeResponse {
                                                    genesis_header,
                                                    signature: Data::Object(random_account.sign_bytes(&nonce.to_le_bytes(), rng).unwrap()),
                                                });
                                                if let Err(e) = framed.send(response).await {
                                                    error!("Error sending challenge response: {:?}", e);
                                                } else {
                                                    debug!("Sent challenge response");
                                                }
                                            }
                                            Message::ChallengeResponse(message) => {
                                                match message.genesis_header == genesis_header {
                                                    true => {
                                                        // Send the first `Ping` message to the peer.
                                                        let message = Message::Ping(Ping {
                                                            version: Message::VERSION,
                                                            node_type: NodeType::Prover,
                                                            block_locators: None,
                                                        });
                                                        if let Err(e) = framed.send(message).await {
                                                            error!("Error sending ping: {:?}", e);
                                                        } else {
                                                            debug!("Sent ping");
                                                        }
                                                    }
                                                    false => {
                                                        error!("Peer has a different genesis block");
                                                        sleep(Duration::from_secs(5)).await;
                                                        break;
                                                    }
                                                }
                                            }
                                            Message::Ping(_) => {
                                                let pong = Message::Pong(Pong { is_fork: None });
                                                if let Err(e) = framed.send(pong).await {
                                                    error!("Error sending pong: {:?}", e);
                                                } else {
                                                    debug!("Sent pong");
                                                }
                                                let message = Message::Ping(Ping {
                                                    version: Message::VERSION,
                                                    node_type: NodeType::Prover,
                                                    block_locators: None,
                                                });
                                                if let Err(e) = framed.send(message).await {
                                                    error!("Error sending ping: {:?}", e);
                                                } else {
                                                    debug!("Sent ping");
                                                }
                                            }
                                            Message::Pong(_) => {
                                                let was_connected = connected.load(Ordering::SeqCst);
                                                connected.store(true, Ordering::SeqCst);
                                                if !was_connected {
                                                    if let Err(e) = framed.send(Message::PuzzleRequest(PuzzleRequest {})).await {
                                                        error!("Failed to send puzzle request: {}", e);
                                                    }
                                                }
                                            }
                                            Message::PuzzleResponse(PuzzleResponse {
                                                epoch_challenge, block_header
                                            }) => {
                                                let block_header = match block_header.deserialize().await {
                                                    Ok(block_header) => block_header,
                                                    Err(error) => {
                                                        error!("Error deserializing block header: {:?}", error);
                                                        sleep(Duration::from_secs(5)).await;
                                                        break;
                                                    }
                                                };
                                                if let Err(e) = prover.sender().send(ProverEvent::NewTarget(block_header.proof_target())).await {
                                                    error!("Error sending new target to prover: {}", e);
                                                } else {
                                                    debug!("Sent new target to prover");
                                                }
                                                if let Err(e) = prover.sender().send(ProverEvent::NewWork(epoch_challenge.epoch_number(), epoch_challenge, client.address())).await {
                                                    error!("Error sending new work to prover: {}", e);
                                                } else {
                                                    debug!("Sent new work to prover");
                                                }
                                            }
                                            Message::Disconnect(message) => {
                                                error!("Peer disconnected: {:?}", message.reason);
                                                sleep(Duration::from_secs(5)).await;
                                                break;
                                            }
                                            _ => {
                                                debug!("Unhandled message: {}", message.name());
                                            }
                                        }
                                    }
                                    Some(Err(e)) => {
                                        warn!("Failed to read the message: {:?}", e);
                                    }
                                    None => {
                                        error!("Disconnected from beacon");
                                        connected.store(false, Ordering::SeqCst);
                                        sleep(Duration::from_secs(5)).await;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to connect to beacon: {}", e);
                        sleep(Duration::from_secs(5)).await;
                    }
                },
                Err(_) => {
                    error!("Failed to connect to beacon: Timed out");
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
}
