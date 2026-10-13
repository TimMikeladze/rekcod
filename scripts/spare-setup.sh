#!/usr/bin/env bash
# Turns this Mac into a one-machine rekcod fleet: controller, node and edge as LaunchAgents
# on its Tailscale address, with only public apps on 127.0.0.1:8081 for cloudflared.
#
#   scripts/spare-setup.sh --db postgres://... --domain example.com
#
# Needs Tailscale up, and automatic login (LaunchAgents start at login). Safe to re-run:
# each agent is replaced, and the node registers again only if it hasn't joined yet.
set -euo pipefail
R=${REKCOD:-rekcod}
db="" domain=""
while [[ $# -gt 0 ]]; do
  case $1 in
    --db) db=$2; shift 2 ;;
    --domain) domain=$2; shift 2 ;;
    *) echo "usage: $0 --db POSTGRES_URL --domain DOMAIN" >&2; exit 2 ;;
  esac
done
[[ -n $db && -n $domain ]] || { echo "usage: $0 --db POSTGRES_URL --domain DOMAIN" >&2; exit 2; }

ts=$(command -v tailscale || echo /Applications/Tailscale.app/Contents/MacOS/Tailscale)
ip=$("$ts" ip -4 2>/dev/null | head -1) || true
[[ -n $ip ]] || { echo "no Tailscale address; run 'tailscale up' first" >&2; exit 1; }
home=${REKCOD_HOME:-$HOME/.rekcod}

"$R" controller serve --db "$db" --listen "$ip:7700" --domain "$domain" --install
for _ in $(seq 60); do [[ -s $home/controller/admin-token ]] && break; sleep 0.5; done
[[ -s $home/controller/admin-token ]] || { echo "controller didn't start; see $home/logs/controller.log" >&2; exit 1; }
admin=$(cat "$home/controller/admin-token")

if [[ -f $home/node.json ]]; then
  "$R" node serve --install
else
  "$R" node join "http://$ip:7700" "$(cat "$home/controller/join-token")" --listen "$ip:7701" --install
fi
"$R" remote add local "http://$ip:7700" --token "$admin"
"$R" edge serve --remote local --listen "$ip:8080" --public-listen 127.0.0.1:8081 --install

name=$("$ts" status --json 2>/dev/null | sed -n 's/.*"DNSName": *"\([^".]*\)\..*/\1/p' | head -1)
cat <<EOF

This Mac is a rekcod fleet on $ip (${name:-its tailnet name}).

On your Mac (on the same tailnet):
  rekcod remote add spare http://${name:-$ip}:7700 --token $admin
  rekcod app deploy --remote spare            # private: http://${name:-$ip}:8080 with Host: <app>.$domain
  rekcod app public <app> --remote spare      # also on the public listener

Public traffic, once $domain's DNS is on Cloudflare:
  cloudflared tunnel login && cloudflared tunnel create rekcod
  cloudflared tunnel route dns rekcod '*.$domain'
  # ~/.cloudflared/config.yml
  #   tunnel: rekcod
  #   credentials-file: $HOME/.cloudflared/<tunnel-id>.json
  #   ingress:
  #     - hostname: "*.$domain"
  #       service: http://127.0.0.1:8081
  #     - service: http_status:404
  sudo cloudflared service install

Logs: $home/logs/{controller,node,edge}.log   Undo: rekcod edge uninstall; rekcod node uninstall; rekcod controller uninstall
EOF
