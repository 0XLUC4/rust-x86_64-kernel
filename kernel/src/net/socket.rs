// =============================================================================
// net/socket.rs — API sockets TCP/UDP pour le kernel (et les syscalls).
//
// Sockets TCP :
//   tcp_connect(addr, port) -> handle
//   tcp_send(handle, data)  -> Result
//   tcp_recv(handle, buf)   -> Result<usize>
//   tcp_close(handle)
//
// Sockets UDP :
//   udp_bind(local_port)    -> handle
//   udp_send(handle, addr, port, data) -> Result
//   udp_recv(handle, buf)   -> Result<(usize, addr, port)>
//
// Chaque opération prend le lock sur le NetStack, fait éventuellement
// un poll(), puis opère sur le socket smoltcp.
//
// Limitations :
//   - Nombre de sockets limité (on pourrait expand le SocketSet)
//   - Pas de select/poll/epoll
//   - Blocking simplifié (busy-wait avec hlt)
// =============================================================================

use alloc::vec;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use smoltcp::time::Instant;

use super::NET;

/// Crée un socket TCP, connecte à (addr, port). Retourne le SocketHandle.
pub fn tcp_connect(addr: Ipv4Address, port: u16) -> Result<SocketHandle, &'static str> {
    let net = NET.get().ok_or("net non init")?;
    let mut stack = net.lock();

    let rx_buf = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tx_buf = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let socket = tcp::Socket::new(rx_buf, tx_buf);

    let handle = stack.sockets.add(socket);

    let _now = Instant::from_millis(crate::time::uptime_ms() as i64);
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(addr), port);

    // Ephemeral local port (49152 + uptime % 16384 pour varier)
    let local_port = 49152 + (crate::time::uptime_ms() % 16384) as u16;

    {
        let crate::net::NetStack { ref mut sockets, ref mut iface, .. } = &mut *stack;
        let sock = sockets.get_mut::<tcp::Socket>(handle);
        sock.connect(iface.context(), endpoint, local_port)
            .map_err(|_| "tcp connect failed")?;
    }

    Ok(handle)
}

/// Envoie des données sur un socket TCP.
pub fn tcp_send(handle: SocketHandle, data: &[u8]) -> Result<usize, &'static str> {
    let net = NET.get().ok_or("net non init")?;
    let mut stack = net.lock();

    // Poll pour avancer l'état
    let now = Instant::from_millis(crate::time::uptime_ms() as i64);
    {
        let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
        let _ = iface.poll(now, device, sockets);
    }

    let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
    if !sock.may_send() {
        return Err("tcp: cannot send");
    }
    sock.send_slice(data).map_err(|_| "tcp send error")
}

/// Reçoit des données depuis un socket TCP. Retourne le nombre d'octets lus.
pub fn tcp_recv(handle: SocketHandle, buf: &mut [u8]) -> Result<usize, &'static str> {
    let net = NET.get().ok_or("net non init")?;
    let mut stack = net.lock();

    let now = Instant::from_millis(crate::time::uptime_ms() as i64);
    {
        let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
        let _ = iface.poll(now, device, sockets);
    }

    let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
    if !sock.may_recv() {
        return Err("tcp: cannot recv");
    }
    match sock.recv_slice(buf) {
        Ok(n) => Ok(n),
        Err(_) => Err("tcp recv error"),
    }
}

/// Ferme un socket TCP.
pub fn tcp_close(handle: SocketHandle) {
    if let Some(net) = NET.get() {
        let mut stack = net.lock();
        let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
        sock.close();
    }
}

/// Crée un socket UDP bindé sur un port local.
pub fn udp_bind(local_port: u16) -> Result<SocketHandle, &'static str> {
    let net = NET.get().ok_or("net non init")?;
    let mut stack = net.lock();

    let rx_meta = udp::PacketMetadata::EMPTY;
    let tx_meta = udp::PacketMetadata::EMPTY;
    let rx_buf = udp::PacketBuffer::new(
        vec![rx_meta; 8],
        vec![0u8; 4096],
    );
    let tx_buf = udp::PacketBuffer::new(
        vec![tx_meta; 8],
        vec![0u8; 4096],
    );
    let mut socket = udp::Socket::new(rx_buf, tx_buf);
    socket.bind(local_port).map_err(|_| "udp bind failed")?;

    let handle = stack.sockets.add(socket);
    Ok(handle)
}

/// Envoie un datagramme UDP.
pub fn udp_send(
    handle: SocketHandle,
    addr: Ipv4Address,
    port: u16,
    data: &[u8],
) -> Result<(), &'static str> {
    let net = NET.get().ok_or("net non init")?;
    let mut stack = net.lock();

    let now = Instant::from_millis(crate::time::uptime_ms() as i64);
    {
        let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
        let _ = iface.poll(now, device, sockets);
    }

    let sock = stack.sockets.get_mut::<udp::Socket>(handle);
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(addr), port);
    sock.send_slice(data, endpoint).map_err(|_| "udp send error")
}

/// Vérifie si le lien réseau est actif.
pub fn link_up() -> bool {
    crate::drivers::e1000::nic()
        .map(|n| n.lock().link_up())
        .unwrap_or(false)
}
