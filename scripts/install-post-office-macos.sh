#!/usr/bin/env bash
# Prepare a headless relay as its service user, then activate it interactively
# with administrator rights as a boot-time LaunchDaemon.
set -euo pipefail

usage() {
    echo "usage: $0 prepare --binary PATH --name NAME --port PORT [--discovery-port PORT] [--lan-announce-port PORT] [--bridge] [--bridge-peer IP:PORT --bridge-peer-id ID --bridge-proxy-port PORT] | activate" >&2
    exit 2
}

phase=${1:-}
[[ $phase == prepare || $phase == activate ]] || usage
shift

label=com.octopusgarage.mesh-talk.post-office
home_dir=$HOME
data_dir=$home_dir/.local/share/mesh-talk-post-office
state_dir=$home_dir/.local/state/mesh-talk-post-office
bin_dir=$home_dir/.local/bin
binary=$bin_dir/mesh-talk-node
staged_plist=$state_dir/LaunchDaemon.plist
installed_plist=/Library/LaunchDaemons/$label.plist

if [[ $phase == activate ]]; then
    [[ $# -eq 0 && -f $staged_plist ]] || usage
    plutil -lint "$staged_plist" >/dev/null
    sudo -v
    sudo install -o root -g wheel -m 644 "$staged_plist" "$installed_plist"
    # The previous per-login agent and the new boot daemon cannot own the same ports.
    launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
    sudo launchctl bootout "system/$label" 2>/dev/null || true
    sudo launchctl bootstrap system "$installed_plist"
    sudo launchctl enable "system/$label"
    sudo launchctl kickstart -k "system/$label"
    sudo launchctl print "system/$label" | grep -E 'state =|pid =|last exit code ='
    exit 0
fi

source_binary=
name=
port=
discovery_port=47474
lan_announce_port=
bridge=false
bridge_peer=
bridge_peer_id=
bridge_proxy_port=
while (($#)); do
    case $1 in
        --binary|--name|--port|--discovery-port|--lan-announce-port|--bridge-peer|--bridge-peer-id|--bridge-proxy-port)
            (($# >= 2)) || usage
            case $1 in
                --binary) source_binary=$2 ;;
                --name) name=$2 ;;
                --port) port=$2 ;;
                --discovery-port) discovery_port=$2 ;;
                --lan-announce-port) lan_announce_port=$2 ;;
                --bridge-peer) bridge_peer=$2 ;;
                --bridge-peer-id) bridge_peer_id=$2 ;;
                --bridge-proxy-port) bridge_proxy_port=$2 ;;
            esac
            shift 2 ;;
        --bridge) bridge=true; shift ;;
        *) usage ;;
    esac
done
[[ -f $source_binary && -n $name && $port =~ ^[0-9]+$ && $discovery_port =~ ^[0-9]+$ ]] || usage
((port >= 1 && port <= 65535 && discovery_port >= 1 && discovery_port <= 65535)) || usage
if [[ -n $lan_announce_port ]]; then
    [[ $lan_announce_port =~ ^[0-9]+$ ]] || usage
    ((lan_announce_port >= 1 && lan_announce_port <= 65535)) || usage
fi
if [[ -n $bridge_peer || -n $bridge_peer_id || -n $bridge_proxy_port ]]; then
    [[ -n $bridge_peer && $bridge_peer_id =~ ^[0-9a-fA-F]{32}$ && $bridge_proxy_port =~ ^[0-9]+$ ]] || usage
    ((bridge_proxy_port >= 1 && bridge_proxy_port <= 65535 && bridge_proxy_port != port)) || usage
    bridge=true
fi

umask 077
install -d -m 700 "$data_dir" "$state_dir" "$bin_dir"
if [[ ! -f $data_dir/password ]]; then
    for state in identity.keystore account.keystore relay.log; do
        [[ ! -e $data_dir/$state ]] || { echo "Existing relay state needs its original password file" >&2; exit 1; }
    done
    openssl rand -base64 48 > "$data_dir/password"
fi
chmod 600 "$data_dir/password"
install -m 755 "$source_binary" "$binary.next"
"$binary.next" --help >/dev/null
mv -f "$binary.next" "$binary"

python3 - "$staged_plist" "$home_dir" "$(id -un)" "$name" "$port" "$discovery_port" "$lan_announce_port" "$bridge" "$bridge_peer" "$bridge_peer_id" "$bridge_proxy_port" <<'PY'
import plistlib
import sys

plist, home, user, name, port, discovery_port, lan_announce_port, bridge, peer, peer_id, proxy_port = sys.argv[1:]
args = [
    f"{home}/.local/bin/mesh-talk-node",
    "--keystore", f"{home}/.local/share/mesh-talk-post-office/identity.keystore",
    "--password-file", f"{home}/.local/share/mesh-talk-post-office/password",
    "--name", name,
    "--post-office", "--port", port,
    "--discovery-port", discovery_port,
]
if lan_announce_port:
    args.extend(["--lan-announce-port", lan_announce_port])
if bridge == "true":
    args.append("--bridge")
if peer:
    args.extend(["--bridge-peer", peer, "--bridge-peer-id", peer_id.lower(),
                 "--bridge-proxy-port", proxy_port])
job = {
    "Label": "com.octopusgarage.mesh-talk.post-office",
    "UserName": user,
    "ProgramArguments": args,
    "WorkingDirectory": home,
    "RunAtLoad": True,
    "KeepAlive": True,
    "ThrottleInterval": 10,
    "StandardOutPath": f"{home}/.local/state/mesh-talk-post-office/stdout.log",
    "StandardErrorPath": f"{home}/.local/state/mesh-talk-post-office/stderr.log",
}
with open(plist, "wb") as output:
    plistlib.dump(job, output)
PY
chmod 600 "$staged_plist"
plutil -lint "$staged_plist" >/dev/null
echo "Prepared $binary and $staged_plist. Run '$0 activate' interactively for boot startup."
