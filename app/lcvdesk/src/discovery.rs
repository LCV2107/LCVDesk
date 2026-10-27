//! Descubrimiento de equipos LCVDesk en la red local (UDP multicast).
//!
//! Cada equipo anuncia su presencia en el grupo multicast `239.255.47.71:47071` cada 2 s y escucha
//! los anuncios de los demás para construir la lista de "En la red". El multicast no cruza subredes
//! (los routers no lo reenvían por defecto): a otras subredes se llega escribiendo la IP a mano.

use lcv_proto::{
    Beacon, DISCOVERY_ADDR, DISCOVERY_PORT, DiscoveryPacket, decode_discovery, encode_discovery,
};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Tiempo sin anuncios tras el cual un equipo se considera desconectado.
const PEER_TTL: Duration = Duration::from_secs(6);
const ANNOUNCE_EVERY: Duration = Duration::from_secs(2);

/// Un equipo encontrado en la red (vista para la interfaz).
#[derive(Clone)]
pub struct Found {
    pub id: String,
    pub name: String,
    /// Dirección de conexión `ip:puerto`.
    pub addr: String,
    pub busy: bool,
}

struct Peer {
    beacon: Beacon,
    ip: Ipv4Addr,
    last_seen: Instant,
}

#[derive(Clone)]
pub struct Discovery {
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    discoverable: Arc<AtomicBool>,
    my_id: String,
}

impl Discovery {
    /// Arranca los hilos de anuncio y escucha. `busy` lo actualiza el host al tener una sesión.
    pub fn start(my_id: String, my_name: String, tcp_port: u16, busy: Arc<AtomicBool>) -> Self {
        let peers: Arc<Mutex<HashMap<String, Peer>>> = Arc::new(Mutex::new(HashMap::new()));
        let discoverable = Arc::new(AtomicBool::new(true));
        let group = Ipv4Addr::from(DISCOVERY_ADDR);

        let socket = match bind_multicast(group) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                warn!("descubrimiento desactivado (no se pudo abrir el socket multicast): {e}");
                return Self { peers, discoverable, my_id };
            }
        };
        info!("descubrimiento en {group}:{DISCOVERY_PORT}");
        let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));

        let dest = SocketAddr::V4(SocketAddrV4::new(group, DISCOVERY_PORT));
        let version = env!("CARGO_PKG_VERSION").to_string();
        let make_beacon = {
            let (id, name) = (my_id.clone(), my_name.clone());
            move |busy_now: bool| Beacon {
                id: id.clone(),
                name: name.clone(),
                port: tcp_port,
                version: version.clone(),
                busy: busy_now,
            }
        };

        // Hilo de anuncio: una baliza cada 2 s (si estamos visibles).
        {
            let (sock, disc, busy, beacon) = (socket.clone(), discoverable.clone(), busy.clone(), make_beacon.clone());
            std::thread::Builder::new()
                .name("discovery-send".into())
                .spawn(move || {
                    // Al arrancar preguntamos para no esperar al primer anuncio ajeno.
                    let _ = sock.send_to(&encode_discovery(&DiscoveryPacket::Query), dest);
                    loop {
                        if disc.load(Ordering::Relaxed) {
                            let pkt = DiscoveryPacket::Announce(beacon(busy.load(Ordering::Relaxed)));
                            let _ = sock.send_to(&encode_discovery(&pkt), dest);
                        }
                        std::thread::sleep(ANNOUNCE_EVERY);
                    }
                })
                .ok();
        }

        // Hilo de escucha: actualiza la tabla y responde a las consultas.
        {
            let (sock, peers, disc, beacon, own_id) =
                (socket.clone(), peers.clone(), discoverable.clone(), make_beacon.clone(), my_id.clone());
            std::thread::Builder::new()
                .name("discovery-recv".into())
                .spawn(move || {
                    let mut buf = [0u8; 512];
                    loop {
                        match sock.recv_from(&mut buf) {
                            Ok((n, SocketAddr::V4(src))) => match decode_discovery(&buf[..n]) {
                                Some(DiscoveryPacket::Announce(b)) if b.id != own_id => {
                                    let mut map = peers.lock().unwrap_or_else(|e| e.into_inner());
                                    map.insert(b.id.clone(), Peer { beacon: b, ip: *src.ip(), last_seen: Instant::now() });
                                }
                                Some(DiscoveryPacket::Query) if disc.load(Ordering::Relaxed) => {
                                    let pkt = DiscoveryPacket::Announce(beacon(false));
                                    let _ = sock.send_to(&encode_discovery(&pkt), SocketAddr::V4(src));
                                }
                                _ => {}
                            },
                            Ok(_) => {}
                            Err(ref e)
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    || e.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => std::thread::sleep(Duration::from_millis(200)),
                        }
                        // Purga de equipos que llevan demasiado sin anunciarse.
                        peers.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, p| p.last_seen.elapsed() < PEER_TTL);
                    }
                })
                .ok();
        }

        Self { peers, discoverable, my_id }
    }

    /// Equipos vistos recientemente, ordenados por nombre.
    pub fn list(&self) -> Vec<Found> {
        let map = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<Found> = map
            .values()
            .filter(|p| p.last_seen.elapsed() < PEER_TTL && p.beacon.id != self.my_id)
            .map(|p| Found {
                id: p.beacon.id.clone(),
                name: p.beacon.name.clone(),
                addr: format!("{}:{}", p.ip, p.beacon.port),
                busy: p.beacon.busy,
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    pub fn is_discoverable(&self) -> bool {
        self.discoverable.load(Ordering::Relaxed)
    }

    pub fn set_discoverable(&self, on: bool) {
        self.discoverable.store(on, Ordering::Relaxed);
    }
}

/// Abre un socket UDP unido al grupo multicast en el puerto de descubrimiento.
fn bind_multicast(group: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))?;
    socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
    let _ = socket.set_multicast_loop_v4(true);
    Ok(socket)
}
