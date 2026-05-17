#!/usr/bin/env bash
set -u
cd "$(dirname "$0")/.."

pkill -9 -f qemu-system-x86_64 2>/dev/null || true
sleep 2
rm -f /tmp/serial.log /tmp/qemu.pid

qemu-system-x86_64 \
  -cdrom build/os-x86_64.iso \
  -serial file:/tmp/serial.log \
  -m 128M -machine pc,hpet=off \
  -netdev 'user,id=net0,hostfwd=tcp::2091-:8080' \
  -device e1000,netdev=net0 \
  -vga std -display none \
  -daemonize -pidfile /tmp/qemu.pid

pid=$(cat /tmp/qemu.pid)
echo "[host] qemu pid=$pid, waiting 10s for DHCP..."
sleep 10

for ep in /api/uptime /api/mem /api/net /api/procs '/api/dns?host=example.com'; do
  echo "===  GET $ep  ==="
  curl -s --max-time 8 "http://127.0.0.1:2091$ep" || echo "(curl failed)"
  echo
  sleep 2
done

echo ""
echo "--- serial log (all http+panic+fault lines) ---"
grep -E '\[http\]|panic|PANIC|fault|fault!' /tmp/serial.log || true

echo ""
echo "--- serial log (last 20 lines) ---"
tail -20 /tmp/serial.log
