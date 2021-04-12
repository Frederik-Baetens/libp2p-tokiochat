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
use std::future::Future;
use tokio::io::{self, AsyncBufReadExt};


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
   //let transport = TokioTcpConfig::new()
   //    .nodelay(true)
   //    .upgrade(upgrade::Version::V1)
   //    .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
   //    .multiplex(mplex::MplexConfig::new())
   //    .boxed();
    let transport = libp2p::development_transport(id_keys.clone()).await?;

    // Create a Gossipsub topic
    let gossipsub_topic = IdentTopic::new("chat");

    // Create a Swarm to manage peers and events.
    let mut swarm: Swarm<Gossipsub> = {
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


        // build a gossipsub network behaviour
        let mut gossipsub2: gossipsub::Gossipsub =
            gossipsub::Gossipsub::new(MessageAuthenticity::Signed(id_keys), gossipsub_config)
                .expect("Correct configuration");

        // subscribes to our topic
        gossipsub2.subscribe(&gossipsub_topic).unwrap();

        libp2p::Swarm::new(transport, gossipsub2, peer_id)

        //SwarmBuilder::new(transport, gossipsub2, peer_id)
        //    // We want the connection background tasks to be spawned
        //    // onto the tokio runtime.
        //    .executor(Box::new(|fut| {
        //        tokio::spawn(fut);
        //    }))
        //    .build()
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        println!("{}", to_dial);
        let addr: Multiaddr = to_dial.parse()?;
        swarm.dial_addr(addr)?;
        println!("Dialed {:?}", to_dial)
    }

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?).unwrap();

    use futures::future;
    use futures::Stream;
    use std::task::Poll;
    use std::pin::Pin;
    future::poll_fn(|cx| {
        if swarm.listeners().peekable().peek().is_some() {
            Poll::Ready(None)
        } else {
            Pin::new(&mut swarm).poll_next(cx)
        }
    }).await;
    for addr in swarm.listeners() {
        dbg!(addr);
    }
    tokio::spawn(run(swarm, gossipsub_topic)).await.unwrap();
    Ok(())
}

async fn run(mut swarm: Swarm<Gossipsub>, gossipsub_topic: IdentTopic) {
    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines();
    //let peerid: PeerId = "12D3KooWK379MZgvrnYLYj2zFDuVWRLZbF7JBsrNqrJYJqXvgfzv".parse().unwrap();
    //swarm.gossipsub.add_explicit_peer(&peerid);
    // Kick it off
    let mut listening = false;
    loop {
        if !listening {
            for addr in swarm.listeners() {
                println!("Listening on {:?}", addr);
                listening = true;
            }
        }
        let to_publish = {
            tokio::select! {
                line = stdin.next_line() => {
                    let line = line.unwrap().expect("stdin closed");
                    if line != "" {
                        dbg!(&line);
                        Some((gossipsub_topic.clone(), line))
                    } else {
                        println!("{}", line);
                        None
                    }
                }
                event = swarm.next() => {
                    // All events are handled by the `NetworkBehaviourEventProcess`es.
                    // I.e. the `swarm.next()` future drives the `Swarm` without ever
                    // terminating.
                    match event {
                        GossipsubEvent::Message {
                            propagation_source: peer_id,
                            message_id: id,
                            message,
                        } => {println!(
                            "Got message: {} with id: {} from peer: {:?}",
                            String::from_utf8_lossy(&message.data),
                            id,
                            peer_id
                        );
                        None},
                        _ => {None}
                    }
                }
            }
        };
        if let Some((topic, line)) = to_publish {
            swarm.behaviour_mut().publish(topic.clone(), line.as_bytes()).unwrap();
        }
        println!("heyahee");
    }
}
