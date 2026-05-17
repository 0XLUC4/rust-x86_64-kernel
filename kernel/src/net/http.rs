// =============================================================================
// net/http.rs — serveur HTTP minimal sur TCP:8080.
//
// Accepte une connexion, lit la request (jusqu'à "\r\n\r\n"), renvoie
// une réponse 200 avec du HTML statique (dashboard kernel).
//
// Lancé comme une task async au boot : `net::http::serve()`.
// =============================================================================

use alloc::vec;
use alloc::string::String;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::time::Instant;

use super::NET;

const LISTEN_PORT: u16 = 8080;

/// Ajoute un socket TCP en listen sur :8080 et pompe async.
pub async fn serve() {
    // Attend que le stack soit up + DHCP configuré (ou 5s max)
    let start = crate::time::uptime_ms();
    while crate::net::NET.get().is_none() {
        crate::time::sleep::sleep_ms(200).await;
        if crate::time::uptime_ms() - start > 5000 { return; }
    }

    let handle = match create_listening_socket() {
        Some(h) => h,
        None => {
            crate::serial_println!("[http] init failed");
            return;
        }
    };
    crate::println!("[http] serveur sur :{} (essaie curl {{ip}}:{} depuis l'host)",
        LISTEN_PORT, LISTEN_PORT);

    loop {
        crate::time::sleep::sleep_ms(100).await;
        tick(handle);
    }
}

fn create_listening_socket() -> Option<SocketHandle> {
    let net = NET.get()?;
    let mut stack = net.lock();
    let rx = tcp::SocketBuffer::new(vec![0u8; 2048]);
    let tx = tcp::SocketBuffer::new(vec![0u8; 8192]);
    let mut socket = tcp::Socket::new(rx, tx);
    socket.listen(LISTEN_PORT).ok()?;
    Some(stack.sockets.add(socket))
}

fn tick(handle: SocketHandle) {
    let Some(net) = NET.get() else { return; };
    let mut stack = net.lock();
    let now = Instant::from_millis(crate::time::uptime_ms() as i64);
    let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
    let _ = iface.poll(now, device, sockets);

    let sock = sockets.get_mut::<tcp::Socket>(handle);

    // Si la socket a été fermée, on la re-listen
    if sock.state() == tcp::State::Closed {
        let _ = sock.listen(LISTEN_PORT);
        return;
    }

    // Lecture : on cumule jusqu'à la fin des headers
    if sock.can_recv() {
        let mut req_bytes = [0u8; 1024];
        if let Ok(n) = sock.recv_slice(&mut req_bytes) {
            if n > 0 {
                let req = core::str::from_utf8(&req_bytes[..n]).unwrap_or("");
                crate::serial_println!("[http] request ({} bytes) : {:.60}", n, req);

                let body = render_dashboard();
                let resp = alloc::format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = sock.send_slice(resp.as_bytes());
                sock.close();
            }
        }
    }
}

fn render_dashboard() -> String {
    let up = crate::time::format_uptime();
    let ticks = crate::time::ticks();
    let (used, total) = crate::memory::frame_allocator::FRAME_ALLOCATOR
        .lock().as_ref().map(|a| a.stats()).unwrap_or((0,0));
    let mib_used = used * 4 / 1024;
    let mib_total = total * 4 / 1024;
    let n_procs = crate::task::process::list().len();
    let n_tasks = crate::task::executor::task_count();
    let syscalls = crate::arch::x86_64::percpu::syscall_count();

    alloc::format!(
r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Rust Kernel</title>
<style>
body {{ background:#0a0a14; color:#e8e8f0; font-family: ui-monospace, SFMono-Regular, monospace; padding:2em; }}
h1 {{ color:#8be9fd; border-bottom:2px solid #6272a4; padding-bottom:.3em; }}
.grid {{ display:grid; grid-template-columns: max-content auto; gap:.3em 1.5em; }}
.k {{ color:#50fa7b; }}
.v {{ color:#f1fa8c; }}
.footer {{ margin-top:3em; color:#6272a4; font-size:.8em; }}
</style></head><body>
<h1>Rust Kernel v0.6 dashboard</h1>
<p>Bienvenue — cette page est servie en live par le kernel (ring 0, driver
e1000, pile TCP/IP smoltcp, HTTP fait maison). Pas de user-space encore.</p>
<div class="grid">
<span class="k">uptime</span><span class="v">{up}  ({ticks} ticks @ 100Hz)</span>
<span class="k">memory</span><span class="v">{mib_used} / {mib_total} MiB used</span>
<span class="k">tasks</span><span class="v">{n_tasks} async, {n_procs} user procs</span>
<span class="k">syscalls</span><span class="v">{syscalls} total</span>
</div>
<p class="footer">Phase IV: framebuffer, preempt ring 3, DHCP, HTTP.</p>
</body></html>"#
    )
}
