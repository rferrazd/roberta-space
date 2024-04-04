// from class 04/02/2024
// updated version
use libp2p::{
    core::upgrade,
    floodsub::{Floodsub, FloodsubEvent, Topic},
    futures::StreamExt,
    identity,
    // mdns broadcast message to all other nodes
    mdns::{Mdns, MdnsEvent},
    mplex,
    noise::{Keypair, NoiseConfig, X25519Spec},
    // swarm manages the cluster of peers 
    swarm::{NetworkBehaviourEventProcess, Swarm, SwarmBuilder},
    tcp::TokioTcpConfig,
    NetworkBehaviour, PeerId, Transport,
};
use log::{error, info}; 
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize}; // package that manages seriralizing and desirializing into diff. data structures. we are desirializing in json. Serializing Rust structure into json
use std::collections::HashSet;
use tokio::{fs, io::AsyncBufReadExt, sync::mpsc};

const STORAGE_FILE_PATH: &str = "./cocktails.json";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;
type Cocktails = Vec<Cocktail>;

static KEYS: Lazy<identity::Keypair> = Lazy::new(|| identity::Keypair::generate_ed25519());
static PEER_ID: Lazy<PeerId> = Lazy::new(|| PeerId::from(KEYS.public()));
static TOPIC: Lazy<Topic> = Lazy::new(|| Topic::new("cocktails"));

#[derive(Debug, Serialize, Deserialize)]
struct Cocktail {
    id: usize,
    name: String,
    ingredients: String,
    instructions: String,
    public: bool,
}

#[derive(Debug, Serialize, Deserialize)]
enum ListMode {
    // keep this the same
    // listing cocktails from all nodes in network
    ALL,
    One(String),
}

#[derive(Debug, Serialize, Deserialize)]
// wrapper
struct ListRequest {
    mode: ListMode,
}

#[derive(Debug, Serialize, Deserialize)]
// wraper
struct ListResponse {
    mode: ListMode,
    data: Cocktails,
    receiver: String,
}

enum EventType {
    // leave the same
    Response(ListResponse),
    Input(String),
}

#[derive(NetworkBehaviour)]
struct CocktailBehaviour {
    floodsub: Floodsub,
    mdns: Mdns,
    #[behaviour(ignore)]
    response_sender: mpsc::UnboundedSender<ListResponse>,
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for CocktailBehaviour {
    fn inject_event(&mut self, event: FloodsubEvent) {
        match event {
            FloodsubEvent::Message(msg) => {
                if let Ok(resp) = serde_json::from_slice::<ListResponse>(&msg.data) {
                    if resp.receiver == PEER_ID.to_string() {
                        info!("Response from {}:", msg.source);
                        resp.data.iter().for_each(|r| info!("{:?}", r));
                    }
                } else if let Ok(req) = serde_json::from_slice::<ListRequest>(&msg.data) {
                    match req.mode {
                        ListMode::ALL => {
                            info!("Received ALL req: {:?} from {:?}", req, msg.source);
                            respond_with_public_cocktails(
                                self.response_sender.clone(),
                                msg.source.to_string(),
                            );
                        }
                        ListMode::One(ref peer_id) => {
                            if peer_id == &PEER_ID.to_string() {
                                info!("Received req: {:?} from {:?}", req, msg.source);
                                respond_with_public_cocktails(
                                    self.response_sender.clone(),
                                    msg.source.to_string(),
                                );
                            }
                        }
                    }
                }
            }
            _ => (),
        }
    }
}

fn respond_with_public_cocktails(sender: mpsc::UnboundedSender<ListResponse>, receiver: String) {
    tokio::spawn(async move {
        match read_local_cocktails().await {
            Ok(cocktails) => {
                let resp = ListResponse {
                    mode: ListMode::ALL,
                    receiver,
                    data: cocktails.into_iter().filter(|r| r.public).collect(),
                };
                if let Err(e) = sender.send(resp) {
                    error!("error sending response via channel, {}", e);
                }
            }
            Err(e) => error!("error fetching local cocktails to answer ALL request, {}", e),
        }
    });
}

impl NetworkBehaviourEventProcess<MdnsEvent> for CocktailBehaviour {
    fn inject_event(&mut self, event: MdnsEvent) {
        match event {
            MdnsEvent::Discovered(discovered_list) => {
                for (peer, _addr) in discovered_list {
                    self.floodsub.add_node_to_partial_view(peer);
                }
            }
            MdnsEvent::Expired(expired_list) => {
                for (peer, _addr) in expired_list {
                    if !self.mdns.has_node(&peer) {
                        self.floodsub.remove_node_from_partial_view(&peer);
                    }
                }
            }
        }
    }
}

async fn create_new_cocktail(name: &str, ingredients: &str, instructions: &str) -> Result<()> {
    let mut local_cocktails = read_local_cocktails().await?;
    let new_id = match local_cocktails.iter().max_by_key(|r| r.id) {
        Some(v) => v.id + 1,
        None => 0,
    };
    local_cocktails.push(Cocktail {
        id: new_id,
        name: name.to_owned(),
        ingredients: ingredients.to_owned(),
        instructions: instructions.to_owned(),
        public: false,
    });
    write_local_cocktails(&local_cocktails).await?;

    info!("Created cocktail:");
    info!("Name: {}", name);
    info!("Ingredients: {}", ingredients);
    info!("Instructions:: {}", instructions);

    Ok(())
}

async fn publish_cocktail(id: usize) -> Result<()> {
    let mut local_cocktails = read_local_cocktails().await?;
    local_cocktails
        .iter_mut()
        .filter(|r| r.id == id)
        .for_each(|r| r.public = true);
    write_local_cocktails(&local_cocktails).await?;
    Ok(())
}

async fn read_local_cocktails() -> Result<Cocktails> {
    let content = fs::read(STORAGE_FILE_PATH).await?;
    let result = serde_json::from_slice(&content)?;
    Ok(result)
}

async fn write_local_cocktails(cocktails: &Cocktails) -> Result<()> {
    let json = serde_json::to_string(&cocktails)?;
    fs::write(STORAGE_FILE_PATH, &json).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    // main will create a new peer id for node in network, and hooks it up with rest of the swarm
    pretty_env_logger::init();

    info!("Peer Id: {}", PEER_ID.clone());
    let (response_sender, mut response_rcv) = mpsc::unbounded_channel();

    let auth_keys = Keypair::<X25519Spec>::new()
        .into_authentic(&KEYS)
        .expect("can create auth keys");

    let transp = TokioTcpConfig::new()
        .upgrade(upgrade::Version::V1)
        .authenticate(NoiseConfig::xx(auth_keys).into_authenticated()) // XX Handshake pattern, IX exists as well and IK - only XX currently provides interop with other libp2p impls
        .multiplex(mplex::MplexConfig::new())
        .boxed();

    let mut behaviour = CocktailBehaviour {
        floodsub: Floodsub::new(PEER_ID.clone()),
        mdns: Mdns::new(Default::default())
            .await
            .expect("can create mdns"),
        response_sender,
    };

    behaviour.floodsub.subscribe(TOPIC.clone());

    let mut swarm = SwarmBuilder::new(transp, behaviour, PEER_ID.clone())
        .executor(Box::new(|fut| {
            tokio::spawn(fut);
        }))
        .build();

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    
    // Swarm = fleet on nodes in network
    Swarm::listen_on(
        &mut swarm,
        // ip4 address: ip4/0.0.0.0 
        // 0.0.0.0 = bind to all interfaces in our machine, ethernet, wifi, etc. -> IT WILL LISTEN TO EVERYTHING. LISTEN TO ALL NETWORK INTERFACES

        "/ip4/0.0.0.0/tcp/0"
            .parse()
            .expect("can get a local socket"),
    )
    .expect("swarm can be started");

    loop {
        // Listen for events in the swarm
        let evt = {
            tokio::select! {
                line = stdin.next_line() => Some(EventType::Input(line.expect("can get line").expect("can read line from stdin"))),
                response = response_rcv.recv() => Some(EventType::Response(response.expect("response exists"))),
                event = swarm.select_next_some() => {
                    info!("Unhandled Swarm Event: {:?}", event);
                    None
                },
            }
        };

        if let Some(event) = evt {
            match event {
                EventType::Response(resp) => {
                    let json = serde_json::to_string(&resp).expect("can jsonify response");
                    swarm
                        .behaviour_mut()
                        .floodsub
                        .publish(TOPIC.clone(), json.as_bytes());
                }
                EventType::Input(line) => match line.as_str() {
                    //list all peers
                    "list peers" => handle_list_peers(&mut swarm).await,
                    cmd if cmd.starts_with("list cocktails") => handle_list_cocktails(cmd, &mut swarm).await,
                    cmd if cmd.starts_with("create cocktail") => handle_create_cocktail(cmd).await,
                    cmd if cmd.starts_with("publish cocktail") => handle_publish_cocktail(cmd).await,
                    _ => error!("unknown command"),
                },
            }
        }
    }
}

async fn handle_list_peers(swarm: &mut Swarm<CocktailBehaviour>) {
    info!("Discovered Peers:");
    let nodes = swarm.behaviour().mdns.discovered_nodes();
    let mut unique_peers = HashSet::new();
    for peer in nodes {
        unique_peers.insert(peer);
    }
    unique_peers.iter().for_each(|p| info!("{}", p));
}

async fn handle_list_cocktails(cmd: &str, swarm: &mut Swarm<CocktailBehaviour>) {
    let rest = cmd.strip_prefix("list coktails "); // include space then strip it
    match rest {
        Some("all") => {
            let req = ListRequest {
                mode: ListMode::ALL,
            };
            let json = serde_json::to_string(&req).expect("can jsonify request");
            swarm
                .behaviour_mut()
                .floodsub
                .publish(TOPIC.clone(), json.as_bytes());
        }
        Some(cocktails_peer_id) => {
            let req = ListRequest {
                mode: ListMode::One(cocktails_peer_id.to_owned()),
            };
            let json = serde_json::to_string(&req).expect("can jsonify request");
            swarm
                .behaviour_mut()
                .floodsub
                .publish(TOPIC.clone(), json.as_bytes());
        }
        None => {
            match read_local_cocktails().await {
                Ok(v) => {
                    info!("Local Cocktails ({})", v.len());
                    v.iter().for_each(|r| info!("{:?}", r));
                }
                Err(e) => error!("error fetching local cocktails: {}", e),
            };
        }
    };
}

async fn handle_create_cocktail(cmd: &str) {
    if let Some(rest) = cmd.strip_prefix("create r") {
        let elements: Vec<&str> = rest.split("|").collect();
        if elements.len() < 3 {
            info!("too few arguments - Format: name|ingredients|instructions");
        } else {
            let name = elements.get(0).expect("name is there");
            let ingredients = elements.get(1).expect("ingredients is there");
            let instructions = elements.get(2).expect("instructions is there");
            if let Err(e) = create_new_cocktail(name, ingredients, instructions).await {
                error!("error creating cocktail: {}", e);
            };
        }
    }
}

async fn handle_publish_cocktail(cmd: &str) {
    if let Some(rest) = cmd.strip_prefix("publish r") {
        match rest.trim().parse::<usize>() {
            Ok(id) => {
                if let Err(e) = publish_cocktail(id).await {
                    info!("error publishing cocktail with id {}, {}", id, e)
                } else {
                    info!("Published Cocktail with id: {}", id);
                }
            }
            Err(e) => error!("invalid id: {}, {}", rest.trim(), e),
        };
    }
}
