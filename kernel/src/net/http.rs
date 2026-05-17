// =============================================================================
// net/http.rs — serveur HTTP minimal sur TCP:8080.
//
// Routes :
//   GET /                  -> dashboard HTML (uptime, mem, procs, tasks)
//   GET /api/uptime        -> JSON  {"uptime_ms": N, "ticks": N}
//   GET /api/mem           -> JSON  {"used_kib": N, "total_kib": N}
//   GET /api/procs         -> JSON  [{"pid":N,"name":"…","state":"…"}, …]
//   GET /api/net           -> JSON  {"ip":"…","gw":"…","dns":"…","dhcp":bool}
//   GET /api/dns?host=NAME -> JSON  {"host":"…","a":"x.y.z.w"}  ou  {"error":"…"}
//   tout autre             -> 404 text/plain
//
// Lancé comme une task async au boot : `net::http::serve()`.
// =============================================================================

use alloc::vec;
use alloc::string::String;
use spin::Mutex;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::time::Instant;

use super::NET;

const LISTEN_PORT: u16 = 8080;

/// Handle vivant du listener courant ; on le remplace après chaque réponse
/// pour éviter le TIME_WAIT bloquant sur un socket unique.
static CURRENT: Mutex<Option<SocketHandle>> = Mutex::new(None);

pub async fn serve() {
    let start = crate::time::uptime_ms();
    while crate::net::NET.get().is_none() {
        crate::time::sleep::sleep_ms(200).await;
        if crate::time::uptime_ms() - start > 5000 { return; }
    }

    install_listener();
    crate::println!("[http] listening :{}", LISTEN_PORT);

    loop {
        crate::time::sleep::sleep_ms(100).await;
        tick();
    }
}

/// Crée un socket TCP en listen, l'ajoute au SocketSet, stocke le handle.
fn install_listener() {
    let Some(net) = NET.get() else { return; };
    let mut stack = net.lock();
    let rx = tcp::SocketBuffer::new(vec![0u8; 2048]);
    let tx = tcp::SocketBuffer::new(vec![0u8; 16384]);
    let mut socket = tcp::Socket::new(rx, tx);
    if socket.listen(LISTEN_PORT).is_ok() {
        let h = stack.sockets.add(socket);
        *CURRENT.lock() = Some(h);
    }
}

fn tick() {
    // Étape 1 : sous lock NET, poll + drain RX dans un buffer local, lire l'état.
    let (state, request_opt, handle) = {
        let Some(net) = NET.get() else { return; };
        let mut stack = net.lock();
        let now = Instant::from_millis(crate::time::uptime_ms() as i64);
        {
            let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
            let _ = iface.poll(now, device, sockets);
        }
        let handle = match *CURRENT.lock() { Some(h) => h, None => return };
        let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
        let state = sock.state();
        let req = if sock.can_recv() {
            let mut tmp = [0u8; 1024];
            match sock.recv_slice(&mut tmp) {
                Ok(n) if n > 0 => Some(alloc::vec::Vec::from(&tmp[..n])),
                _ => None,
            }
        } else { None };
        (state, req, handle)
    };

    // Étape 2 : recycle ou close — sous lock NET court.
    if state == tcp::State::Closed {
        if let Some(net) = NET.get() {
            let mut stack = net.lock();
            stack.sockets.remove(handle);
        }
        *CURRENT.lock() = None;
        install_listener();
        return;
    }
    if state == tcp::State::CloseWait {
        if let Some(net) = NET.get() {
            let mut stack = net.lock();
            let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
            sock.close();
        }
        return;
    }

    // Étape 3 : SANS lock NET, on route + génère la réponse. Les handlers
    // (api_net, api_dns) reprennent le lock NET de leur côté.
    let Some(bytes) = request_opt else { return; };
    let req = core::str::from_utf8(&bytes).unwrap_or("");
    crate::serial_println!("[http] {} bytes : {:.60}", bytes.len(), req);
    let (method, path) = parse_request_line(req);
    let resp = if method != "GET" {
        response_text(405, "Method Not Allowed", "Only GET\n")
    } else {
        route(path)
    };

    // Étape 4 : re-lock NET et envoie la réponse.
    if let Some(net) = NET.get() {
        let mut stack = net.lock();
        let sock = stack.sockets.get_mut::<tcp::Socket>(handle);
        let sent = sock.send_slice(resp.as_bytes()).unwrap_or(0);
        crate::serial_println!("[http] respond {} B sent", sent);
    }
}

fn parse_request_line(req: &str) -> (&str, &str) {
    let first = req.lines().next().unwrap_or("");
    let mut it = first.split_ascii_whitespace();
    let method = it.next().unwrap_or("");
    let path   = it.next().unwrap_or("/");
    (method, path)
}

fn route(path: &str) -> String {
    // Sépare path et query string.
    let (p, q) = match path.find('?') {
        Some(i) => (&path[..i], &path[i+1..]),
        None    => (path, ""),
    };
    match p {
        "/" | "/index.html" => response_html(200, "OK", &render_dashboard()),
        "/api/uptime"       => response_json(200, &api_uptime()),
        "/api/mem"          => response_json(200, &api_mem()),
        "/api/procs"        => response_json(200, &api_procs()),
        "/api/net"          => response_json(200, &api_net()),
        "/api/dns"          => response_json(200, &api_dns(q)),
        _                   => response_text(404, "Not Found",
            "404 — disponibles: / /api/{uptime,mem,procs,net,dns}\n"),
    }
}

// -----------------------------------------------------------------------------
// Builders
// -----------------------------------------------------------------------------

fn response_html(code: u16, reason: &str, body: &str) -> String {
    alloc::format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        code=code, reason=reason, len=body.len(), body=body)
}

fn response_json(code: u16, body: &str) -> String {
    alloc::format!(
        "HTTP/1.1 {code} OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        code=code, len=body.len(), body=body)
}

fn response_text(code: u16, reason: &str, body: &str) -> String {
    alloc::format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        code=code, reason=reason, len=body.len(), body=body)
}

// -----------------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------------

fn api_uptime() -> String {
    alloc::format!(
        r#"{{"uptime_ms":{},"ticks":{}}}"#,
        crate::time::uptime_ms(), crate::time::ticks(),
    )
}

fn api_mem() -> String {
    let (used, total) = crate::memory::frame_allocator::FRAME_ALLOCATOR
        .lock().as_ref().map(|a| a.stats()).unwrap_or((0,0));
    alloc::format!(
        r#"{{"used_kib":{},"total_kib":{}}}"#,
        used * 4, total * 4,
    )
}

fn api_procs() -> String {
    let procs = crate::task::process::list();
    let mut s = String::from("[");
    for (i, (pid, parent, name, state)) in procs.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&alloc::format!(
            r#"{{"pid":{},"parent":{},"name":"{}","state":"{:?}"}}"#,
            pid, parent, escape(name), state,
        ));
    }
    s.push(']');
    s
}

fn api_net() -> String {
    let ip = crate::net::ip_address()
        .map(|a| alloc::format!("{}", a))
        .unwrap_or_else(|| String::from("0.0.0.0"));
    let gw = crate::net::gateway()
        .map(|a| alloc::format!("{}", a))
        .unwrap_or_else(|| String::from("0.0.0.0"));
    let dns = crate::net::dns_server()
        .map(|a| alloc::format!("{}", a))
        .unwrap_or_else(|| String::from("0.0.0.0"));
    let dhcp = crate::net::dhcp_configured();
    alloc::format!(
        r#"{{"ip":"{}","gw":"{}","dns":"{}","dhcp":{}}}"#,
        ip, gw, dns, dhcp,
    )
}

fn api_dns(query: &str) -> String {
    let host = query_param(query, "host").unwrap_or("");
    if host.is_empty() {
        return String::from(r#"{"error":"missing host param"}"#);
    }
    match crate::net::dns::resolve_a(host) {
        Some(addr) => alloc::format!(
            r#"{{"host":"{}","a":"{}"}}"#, escape(host), addr),
        None => alloc::format!(
            r#"{{"host":"{}","error":"resolve failed"}}"#, escape(host)),
    }
}

fn query_param<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    for pair in q.split('&') {
        let (k, v) = match pair.find('=') {
            Some(i) => (&pair[..i], &pair[i+1..]),
            None    => (pair, ""),
        };
        if k == key { return Some(v); }
    }
    None
}

/// Échappe les caractères JSON minimum (guillemet, backslash).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// -----------------------------------------------------------------------------
// HTML dashboard (page /)
// -----------------------------------------------------------------------------

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
    let ip = crate::net::ip_address()
        .map(|a| alloc::format!("{}", a)).unwrap_or_else(|| String::from("?"));

    alloc::format!(
r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Rust Kernel</title>
<style>
body {{ background:#0a0a14; color:#e8e8f0; font-family: ui-monospace, SFMono-Regular, monospace; padding:2em; max-width:60em; margin:auto; }}
h1 {{ color:#8be9fd; border-bottom:2px solid #6272a4; padding-bottom:.3em; }}
h2 {{ color:#bd93f9; margin-top:1.5em; }}
.grid {{ display:grid; grid-template-columns: max-content auto; gap:.3em 1.5em; }}
.k {{ color:#50fa7b; }}
.v {{ color:#f1fa8c; }}
code {{ background:#1a1a2a; padding:.1em .4em; border-radius:.2em; }}
.footer {{ margin-top:3em; color:#6272a4; font-size:.8em; }}
ul.api li {{ margin:.2em 0; }}
</style></head><body>
<h1>Rust Kernel — live dashboard</h1>
<p>Cette page est servie en ring 0 par le kernel via driver e1000 + smoltcp.</p>
<div class="grid">
<span class="k">uptime</span><span class="v">{up}  ({ticks} ticks @ 100Hz)</span>
<span class="k">memory</span><span class="v">{mib_used} / {mib_total} MiB used</span>
<span class="k">tasks</span><span class="v">{n_tasks} async, {n_procs} user procs</span>
<span class="k">syscalls</span><span class="v">{syscalls} total</span>
<span class="k">ip</span><span class="v">{ip}</span>
</div>
<h2>API JSON</h2>
<ul class="api">
<li><code>GET /api/uptime</code> — uptime_ms + ticks</li>
<li><code>GET /api/mem</code> — frame allocator stats</li>
<li><code>GET /api/procs</code> — user process list</li>
<li><code>GET /api/net</code> — IP / gateway / DNS / DHCP state</li>
<li><code>GET /api/dns?host=NAME</code> — A-record lookup (DNS server vient du DHCP)</li>
</ul>
<p class="footer">Rust no_std · multiboot2 · ring 3 · CoW fork · FAT32 · TCP/IP</p>
</body></html>"#
    )
}
