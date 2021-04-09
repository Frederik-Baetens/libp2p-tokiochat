// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! A basic chat application demonstrating libp2p with the mDNS and gossipsub protocols
//! using tokio for all asynchronous tasks and I/O. In order for all used libp2p
//! crates to use tokio, it enables tokio-specific features for some crates.
//!
//! The example is run per node as follows:
//!
//! ```sh
//! cargo run --example chat-tokio --features="tcp-tokio mdns-tokio"
//! ```
//!
//! Alternatively, to run with the minimal set of features and crates:
//!
//! ```sh
//!cargo run --example chat-tokio \\
//!    --no-default-features \\
//!    --features="gossipsub mplex noise tcp-tokio mdns-tokio"
//! ```

use libp2p::{
    core::upgrade,
    gossipsub::{
        self, Gossipsub, GossipsubEvent, GossipsubMessage, IdentTopic, MessageAuthenticity,
        MessageId, ValidationMode,
    },
    identity,
    mdns::{Mdns, MdnsEvent},
    mplex,
    noise,
    swarm::SwarmBuilder,
    // `TokioTcpConfig` is available through the `tcp-tokio` feature.
    tcp::TokioTcpConfig,
    Multiaddr,
    NetworkBehaviour,
    PeerId,
    Swarm,
    Transport,
};
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt};

#[derive(Debug)]
pub enum OutEvent {
    GossipsubEvent(GossipsubEvent),
}

impl From<GossipsubEvent> for OutEvent {
    fn from(gossipsubevent: GossipsubEvent) -> Self {
        OutEvent::GossipsubEvent(gossipsubevent)
    }
}

// We create a custom network behaviour that combines floodsub and mDNS.
// The derive generates a delegating `NetworkBehaviour` impl which in turn
// requires the implementations of `NetworkBehaviourEventProcess` for
// the events of each behaviour.
#[behaviour(out_event = "OutEvent", event_process = false)]
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    gossipsub: Gossipsub,
}

/// The `tokio::main` attribute sets up a tokio runtime.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    // Create a random PeerId
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    println!("Local peer id: {:?}", peer_id);

    // Create a keypair for authenticated encryption of the transport.
    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(&id_keys)
        .expect("Signing libp2p-noise static DH keypair failed.");

    // Create a tokio-based TCP transport use noise for authenticated
    // encryption and Mplex for multiplexing of substreams on a TCP stream.
    let transport = TokioTcpConfig::new()
        .nodelay(true)
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(mplex::MplexConfig::new())
        .boxed();

    // Create a Gossipsub topic
    let gossipsub_topic = IdentTopic::new("chat");

    // Create a Swarm to manage peers and events.
    let mut swarm: Swarm<MyBehaviour> = {
        // To content-address message, we can take the hash of message and use it as an ID.
        let message_id_fn = |message: &GossipsubMessage| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            MessageId::from(s.finish().to_string())
        };

        // Set a custom gossipsub
        let gossipsub_config = gossipsub::GossipsubConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
            .validation_mode(ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
            .message_id_fn(message_id_fn) // content-address messages. No two messages of the
            // same content will be propagated.
            .build()
            .expect("Valid config");

        let mut behaviour = MyBehaviour {
            gossipsub: Gossipsub::new(MessageAuthenticity::Signed(id_keys), gossipsub_config)
                .unwrap(),
        };

        behaviour.gossipsub.subscribe(&gossipsub_topic).unwrap();

        SwarmBuilder::new(transport, behaviour, peer_id)
            // We want the connection background tasks to be spawned
            // onto the tokio runtime.
            .executor(Box::new(|fut| {
                tokio::spawn(fut);
            }))
            .build()
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse()?;
        Swarm::dial_addr(&mut swarm, addr)?;
        println!("Dialed {:?}", to_dial)
    }

    // Listen on all interfaces and whatever port the OS assigns
    Swarm::listen_on(&mut swarm, "/ip4/0.0.0.0/tcp/0".parse()?).unwrap();
    for addr in Swarm::listeners(&swarm) {
        dbg!(addr);
    }
    tokio::spawn(run(swarm, gossipsub_topic));
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    Ok(())
}

async fn run(mut swarm: Swarm<MyBehaviour>, gossipsub_topic: IdentTopic) {
    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();
    //let peerid: PeerId = "12D3KooWK379MZgvrnYLYj2zFDuVWRLZbF7JBsrNqrJYJqXvgfzv".parse().unwrap();
    //swarm.gossipsub.add_explicit_peer(&peerid);
    // Kick it off
    let mut listening = false;
    loop {
        if !listening {
            println!("hi");
            for addr in Swarm::listeners(&swarm) {
                println!("Listening on {:?}", addr);
                listening = true;
            }
        }
        let to_publish = {
            tokio::select! {
                line = stdin.next_line() => {
                    let line = line.unwrap().expect("stdin closed");
                    Some((gossipsub_topic.clone(), line))
                }
                event = swarm.next() => {
                    println!("hi");
                    match event {
                        OutEvent::GossipsubEvent(gossipsubevent) => {
                            if let GossipsubEvent::Message { message, message_id, propagation_source } = gossipsubevent {
                                println!("Received: '{:?}' from {:?}", String::from_utf8_lossy(&message.data), message.source);
                                println!("message id was {:?} and propagation_source was {:?}", message_id, propagation_source);
                            }
                            None
                        }
                    }
                }
            }
        };
        if let Some((topic, line)) = to_publish {
            swarm.gossipsub.publish(topic, line.as_bytes()).unwrap();
        }
    }
}
