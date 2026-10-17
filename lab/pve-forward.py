#!/usr/bin/env python3
"""pve-forward — tiny 127.0.0.1 -> host TCP forwarder.

Run this in your NORMAL Mac terminal (the one that CAN reach the Proxmox host).
Claude Code's sandboxed Bash only shares 127.0.0.1 with your shell and cannot
see the VMware host-only network, so this bridges them:

    python3 lab/pve-forward.py                       # PROD: 127.0.0.1:8006 -> 192.168.8.207:8006 (default)
    python3 lab/pve-forward.py 8006 192.168.4.165 8006   # DEV/lab: PC-004-165
    python3 lab/pve-forward.py 8006 192.168.4.157 8006   # DEV/lab: PC-004-157 (hives 240-243)

Environments (see lab/logbook/METHOD.md §1): PROD = 192.168.8.207 (node `pve`, token
`ai-agent@pve!vscode`); DEV/lab = 192.168.4.165 / .157 (token `dev_coa@pve!claude-token`).
Default target below is PROD; pass the DEV IP as arg 2 to bridge the lab instead.

Leave it running (Ctrl-C to stop). Raw TCP passthrough — Proxmox's TLS
terminates at the real host, so `curl -k` / pve.py (PVE_HOST=127.0.0.1) work
unchanged. Bind extra ports the same way if you ever want a second path.
"""
import sys, socket, threading

LPORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8006
RHOST = sys.argv[2] if len(sys.argv) > 2 else "192.168.8.207"
RPORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8006


def _pipe(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def _handle(client):
    try:
        upstream = socket.create_connection((RHOST, RPORT), timeout=10)
    except OSError as e:
        sys.stderr.write("  upstream %s:%d failed: %s\n" % (RHOST, RPORT, e))
        client.close()
        return
    threading.Thread(target=_pipe, args=(client, upstream), daemon=True).start()
    threading.Thread(target=_pipe, args=(upstream, client), daemon=True).start()


def main():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", LPORT))
    srv.listen(64)
    print("forwarding 127.0.0.1:%d -> %s:%d   (leave running; Ctrl-C to stop)" % (LPORT, RHOST, RPORT))
    try:
        while True:
            client, _ = srv.accept()
            threading.Thread(target=_handle, args=(client,), daemon=True).start()
    except KeyboardInterrupt:
        print("\nstopped")


if __name__ == "__main__":
    main()
