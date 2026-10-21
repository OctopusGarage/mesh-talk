#!/usr/bin/env bash
# Prepare a headless relay as its service user, then activate it interactively
# with administrator rights as a boot-time LaunchDaemon.
set -euo pipefail

usage() {
    echo "usage: $0 prepare --binary PATH --name NAME [--port 47479] [--discovery-port PORT] [--lan-announce-port PORT] [--bridge] [--bridge-peer IP:PORT --bridge-peer-id ID --bridge-proxy-port PORT] [--federation-peer ID@IP:PORT ... | --federation-config PATH | --relay-network PATH --relay-membership PATH [--relay-seed ID@IP:PORT ...]] | activate" >&2
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
    # Reject old prepared files as well as invalid prepare arguments.
    python3 - "$staged_plist" <<'PYPORTS'
import plistlib
import sys

with open(sys.argv[1], "rb") as source:
    args = plistlib.load(source)["ProgramArguments"]
if args.count("--port") != 1 or args[args.index("--port") + 1:args.index("--port") + 2] != ["47479"]:
    raise SystemExit("Managed post offices require TCP 47479")
for flag, port in [("--discovery-port", "47478"), ("--lan-announce-port", "47474")]:
    if args.count(flag) != 1 or args[args.index(flag) + 1:args.index(flag) + 2] != [port]:
        raise SystemExit("Managed post offices require UDP 47478 and LAN UDP 47474")
PYPORTS
    sudo -v
    sudo install -o root -g wheel -m 644 "$staged_plist" "$installed_plist"
    # The previous per-login agent and the new boot daemon cannot own the same ports.
    launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
    sudo launchctl bootout "system/$label" 2>/dev/null || true
    # bootout can return before launchd finishes removing the job. Loading
    # immediately then fails with "Operation already in progress".
    for ((attempt = 0; attempt < 100; attempt++)); do
        if ! sudo launchctl print "system/$label" >/dev/null 2>&1; then
            break
        fi
        sleep 0.2
    done
    if ((attempt == 100)); then
        echo "Timed out waiting for launchd to remove the old post office" >&2
        exit 1
    fi
    sudo launchctl enable "system/$label"
    sudo launchctl bootstrap system "$installed_plist"
    sudo launchctl print "system/$label" | grep -E 'state =|pid =|last exit code ='
    exit 0
fi

source_binary=
name=
port=47479
discovery_port=47478
lan_announce_port=47474
bridge=false
bridge_peer=
bridge_peer_id=
bridge_proxy_port=
federation_peers=()
federation_config=
relay_network=
relay_membership=
relay_seeds=()
while (($#)); do
    case $1 in
        --binary|--name|--port|--discovery-port|--lan-announce-port|--bridge-peer|--bridge-peer-id|--bridge-proxy-port|--federation-peer|--federation-config|--relay-network|--relay-membership|--relay-seed)
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
                --federation-peer) federation_peers+=("$2") ;;
                --federation-config) federation_config=$2 ;;
                --relay-network) relay_network=$2 ;;
                --relay-membership) relay_membership=$2 ;;
                --relay-seed) relay_seeds+=("$2") ;;
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
if [[ $port != 47479 ]]; then
    echo "Managed post offices require TCP 47479" >&2
    exit 2
fi
# Managed services share one role-based discovery contract on every host.
# The raw CLI still supports isolated custom-port test networks.
if [[ $discovery_port != 47478 || $lan_announce_port != 47474 ]]; then
    echo "Managed post offices require UDP 47478 and LAN UDP 47474" >&2
    exit 2
fi
if [[ -n $bridge_peer || -n $bridge_peer_id || -n $bridge_proxy_port ]]; then
    [[ -n $bridge_peer && $bridge_peer_id =~ ^[0-9a-fA-F]{32}$ && $bridge_proxy_port =~ ^[0-9]+$ ]] || usage
    ((bridge_proxy_port >= 1 && bridge_proxy_port <= 65535 && bridge_proxy_port != port)) || usage
    bridge=true
fi
for seed in ${federation_peers[@]+"${federation_peers[@]}"}; do
    [[ $seed =~ ^[0-9a-fA-F]{32}@.+:[0-9]+$ ]] || usage
    bridge=true
done

if [[ -n $federation_config ]]; then
    [[ -z ${federation_peers[*]-} ]] || { echo "--federation-config conflicts with --federation-peer" >&2; exit 2; }
    [[ -f $federation_config ]] || { echo "Federation config must be an existing public manifest" >&2; exit 2; }
    federation_config=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$federation_config")
    # The staged node owns schema, endpoint, and bound validation. Do this before
    # creating directories, passwords, or replacing the installed binary.
    source_binary=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$source_binary")
    "$source_binary" federation --file "$federation_config" list >/dev/null
    bridge=true
fi

if [[ -n $relay_network || -n $relay_membership || -n ${relay_seeds[*]-} ]]; then
    [[ -n $relay_network && -n $relay_membership && -z $federation_config && -z ${federation_peers[*]-} && -z $bridge_peer ]] || { echo "Network mode needs policy and membership and conflicts with federation pins or a bridge gateway" >&2; exit 2; }
    for seed in ${relay_seeds[@]+"${relay_seeds[@]}"}; do
        [[ $seed =~ ^[0-9a-fA-F]{32}@.+:[0-9]+$ ]] || usage
    done
    relay_network=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$relay_network")
    relay_membership=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$relay_membership")
    "$source_binary" network inspect --policy "$relay_network" --membership "$relay_membership" >/dev/null
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
# Local-network rules identify the executable separately from its mesh identity.
# Linker-generated identifiers can change between builds; retain the installed
# identifier for locally signed updates, and use a stable one on first install.
signing_id=com.octopusgarage.mesh-talk.node
if [[ -f $binary ]]; then
    existing_signature=$(codesign --display --verbose=2 "$binary" 2>&1) || existing_signature=
    existing_id=$(printf '%s\n' "$existing_signature" | sed -n 's/^Identifier=//p')
    [[ -z $existing_id ]] || signing_id=$existing_id
fi
install -m 755 "$source_binary" "$binary.next"
staged_signature=$(codesign --display --verbose=2 "$binary.next" 2>&1) || staged_signature=
if ! grep -q '^Authority=' <<< "$staged_signature"; then
    codesign --force --sign - --identifier "$signing_id" "$binary.next"
fi
# Preserve publisher-issued signatures, and never replace a working executable
# if either signing or signature verification fails.
codesign --verify --strict "$binary.next"
"$binary.next" --help >/dev/null
mv -f "$binary.next" "$binary"
if [[ -n $federation_config ]]; then
    # Keep public trust separate from private identity, and alive after staging cleanup.
    persistent_config=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$data_dir/federation.conf")
    if [[ $federation_config != "$persistent_config" ]]; then
        install -m 600 "$federation_config" "$data_dir/federation.conf.next"
        mv -f "$data_dir/federation.conf.next" "$data_dir/federation.conf"
    fi
    federation_config=$persistent_config
fi

for network_file in network.policy relay.membership; do
    if [[ -n $relay_network ]]; then
        source_file=$relay_network
        [[ $network_file != relay.membership ]] || source_file=$relay_membership
        persistent_file=$data_dir/$network_file
        if [[ $source_file != "$persistent_file" ]]; then
            install -m 600 "$source_file" "$persistent_file.next"
            mv -f "$persistent_file.next" "$persistent_file"
        fi
    fi
done
if [[ -n $relay_network ]]; then
    relay_network=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$data_dir/network.policy")
    relay_membership=$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$data_dir/relay.membership")
fi

python3 - "$staged_plist" "$home_dir" "$(id -un)" "$name" "$port" "$discovery_port" "$lan_announce_port" "$bridge" "$bridge_peer" "$bridge_peer_id" "$bridge_proxy_port" "$federation_config" "$relay_network" "$relay_membership" "${#relay_seeds[@]}" ${relay_seeds[@]+"${relay_seeds[@]}"} ${federation_peers[@]+"${federation_peers[@]}"} <<'PY'
import plistlib
import sys

plist, home, user, name, port, discovery_port, lan_announce_port, bridge, peer, peer_id, proxy_port, federation_config, relay_network, relay_membership, seed_count, *seeds = sys.argv[1:]
relay_seeds, federation_peers = seeds[:int(seed_count)], seeds[int(seed_count):]
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
if relay_network:
    args.extend(["--relay-network", relay_network, "--relay-membership", relay_membership])
for seed in relay_seeds:
    args.extend(["--relay-seed", seed])
if federation_config:
    args.extend(["--federation-config", federation_config])
for seed in federation_peers:
    args.extend(["--federation-peer", seed])
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
