// =============================================================================
// net/mod.rs — couche réseau minimale autour de smoltcp.
//
// smoltcp est un stack TCP/IP no_std en Rust pur. On l'interface avec
// notre driver e1000 via un `Device` smoltcp custom.
//
// Pour l'instant : IP statique (configurable depuis le shell), ARP, ICMP
// (ping réponse), et sockets TCP/UDP.
//
// Architecture :
//   - `E1000Device` : implémente smoltcp::phy::Device (send/recv raw Ethernet)
//   - `NetStack`    : possède l'Interface smoltcp + gère le polling
//   - polling       : appelé périodiquement depuis le timer ou manuellement
//
// Usage typique :
//   net::init()                       — crée l'interface, IP par défaut 10.0.2.15/24
//   net::poll()                       — traite les paquets en attente
//   net::tcp_connect(addr, port)      — ouvre une connexion TCP
//   net::udp_send(addr, port, data)   — envoie un datagramme UDP
// =============================================================================

pub mod socket;
pub mod http;
pub mod dns;

use alloc::{vec, vec::Vec};
use spin::{Mutex, Once};

use smoltcp::iface::{Config, Interface, SocketSet, SocketHandle};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::dhcpv4;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use smoltcp::time::Instant;

use crate::drivers::e1000;

/// Wrapper smoltcp autour de notre e1000.
pub struct E1000Device;

impl Device for E1000Device {
    type RxToken<'a> = E1000RxToken;
    type TxToken<'a> = E1000TxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let nic = e1000::nic()?;
        let data = nic.lock().recv()?;
        Some((E1000RxToken(data), E1000TxToken))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(E1000TxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(1);
        caps
    }
}

pub struct E1000RxToken(Vec<u8>);
pub struct E1000TxToken;

impl phy::RxToken for E1000RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where F: FnOnce(&mut [u8]) -> R
    {
        f(&mut self.0)
    }
}

impl phy::TxToken for E1000TxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where F: FnOnce(&mut [u8]) -> R
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        if let Some(nic) = e1000::nic() {
            let _ = nic.lock().send(&buf);
        }
        result
    }
}

/// State du stack réseau.
pub struct NetStack {
    pub iface: Interface,
    pub device: E1000Device,
    pub sockets: SocketSet<'static>,
    pub dhcp_handle: Option<SocketHandle>,
    pub dhcp_configured: bool,
    pub dns_server: Option<Ipv4Address>,
    pub gateway:    Option<Ipv4Address>,
}

static NET: Once<Mutex<NetStack>> = Once::new();

/// Initialise le stack réseau. Requiert que e1000::init() ait été appelé.
pub fn init() {
    let mac = match e1000::mac_address() {
        Some(m) => m,
        None => {
            crate::println!("[net] pas de NIC — stack réseau non initialisé");
            return;
        }
    };

    let ethernet_addr = EthernetAddress(mac);

    let mut config = Config::new(ethernet_addr.into());
    // Pas de random seed no_std simple — on utilise une constante
    config.random_seed = 0xDEAD_BEEF_CAFE_1234;

    let mut device = E1000Device;
    let now = Instant::from_millis(crate::time::uptime_ms() as i64);

    let iface = Interface::new(config, &mut device, now);
    // Pas d'IP au départ — DHCP va la fournir via la task `net_poll_task`.

    let mut sockets = SocketSet::new(vec![]);
    let dhcp_socket = dhcpv4::Socket::new();
    let dhcp_handle = sockets.add(dhcp_socket);

    let link = e1000::nic().map(|n| n.lock().link_up()).unwrap_or(false);
    crate::serial_println!("[net] interface up mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} link={} — DHCP en cours...",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], link);
    crate::println!("[net] interface up — DHCP en cours...");

    NET.call_once(|| Mutex::new(NetStack {
        iface,
        device,
        sockets,
        dhcp_handle: Some(dhcp_handle),
        dhcp_configured: false,
        dns_server: None,
        gateway: None,
    }));
}

/// Polling : traite les paquets RX/TX + DHCP state machine.
pub fn poll() {
    if let Some(net) = NET.get() {
        let mut stack = net.lock();
        let now = Instant::from_millis(crate::time::uptime_ms() as i64);
        let NetStack {
            ref mut iface, ref mut device, ref mut sockets,
            dhcp_handle, dhcp_configured,
            dns_server, gateway, ..
        } = &mut *stack;
        let _ = iface.poll(now, device, sockets);

        // DHCP state machine
        if let Some(handle) = *dhcp_handle {
            let event = sockets.get_mut::<dhcpv4::Socket>(handle).poll();
            if let Some(event) = event {
                match event {
                    dhcpv4::Event::Deconfigured => {
                        crate::serial_println!("[dhcp] déconfiguré");
                        iface.update_ip_addrs(|a| a.clear());
                        iface.routes_mut().remove_default_ipv4_route();
                        *dhcp_configured = false;
                        *dns_server = None;
                        *gateway = None;
                    }
                    dhcpv4::Event::Configured(cfg) => {
                        crate::println!("[dhcp] bail OK : IP={} mask=/{}",
                            cfg.address.address(), cfg.address.prefix_len());
                        crate::serial_println!("[dhcp] bail OK : IP={} mask=/{}",
                            cfg.address.address(), cfg.address.prefix_len());
                        iface.update_ip_addrs(|addrs| {
                            addrs.clear();
                            let _ = addrs.push(IpCidr::Ipv4(cfg.address));
                        });
                        if let Some(router) = cfg.router {
                            crate::println!("[dhcp] gateway = {}", router);
                            crate::serial_println!("[dhcp] gateway = {}", router);
                            let _ = iface.routes_mut().add_default_ipv4_route(router);
                            *gateway = Some(router);
                        }
                        if let Some(&dns) = cfg.dns_servers.first() {
                            crate::println!("[dhcp] dns     = {}", dns);
                            crate::serial_println!("[dhcp] dns     = {}", dns);
                            *dns_server = Some(dns);
                        }
                        *dhcp_configured = true;
                    }
                }
            }
        }
    }
}

pub fn dhcp_configured() -> bool {
    NET.get().map(|n| n.lock().dhcp_configured).unwrap_or(false)
}

pub fn dns_server() -> Option<Ipv4Address> {
    NET.get().and_then(|n| n.lock().dns_server)
}

pub fn gateway() -> Option<Ipv4Address> {
    NET.get().and_then(|n| n.lock().gateway)
}

pub fn stack() -> Option<&'static Mutex<NetStack>> {
    NET.get()
}

/// Retourne l'adresse IP configurée (première IPv4).
pub fn ip_address() -> Option<Ipv4Address> {
    let net = NET.get()?;
    let stack = net.lock();
    for cidr in stack.iface.ip_addrs() {
        if let IpAddress::Ipv4(v4) = cidr.address() {
            return Some(v4);
        }
    }
    None
}
