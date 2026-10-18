#!/usr/bin/env bash
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
	printf '%s\n' 'SSH loopback fixture requires macOS' >&2
	exit 0
fi

sshd=${SSHD:-/usr/sbin/sshd}
ssh_keygen=${SSH_KEYGEN:-/usr/bin/ssh-keygen}
ssh_agent=${SSH_AGENT:-/usr/bin/ssh-agent}
ssh_add=${SSH_ADD:-/usr/bin/ssh-add}

for tool in "$sshd" "$ssh_keygen" "$ssh_agent" "$ssh_add"; do
	[ -x "$tool" ] || { printf 'missing SSH fixture tool: %s\n' "$tool" >&2; exit 1; }
done

root=$(mktemp -d)
agent_env=''
pid=''
cleanup() {
	[ -n "$pid" ] && kill "$pid" 2>/dev/null || true
	[ -n "$agent_env" ] && eval "$agent_env" >/dev/null 2>&1 || true
	rm -rf "$root"
}
trap cleanup EXIT

mkdir -p "$root/.ssh"
chmod 700 "$root/.ssh"
"$ssh_keygen" -q -t ed25519 -N '' -f "$root/client" -C omp-ssh-test
"$ssh_keygen" -q -t ed25519 -N '' -f "$root/host" -C omp-ssh-test-host
cp "$root/client.pub" "$root/.ssh/authorized_keys"
chmod 600 "$root/.ssh/authorized_keys" "$root/client"
chmod 644 "$root/client.pub" "$root/host.pub"

agent_env=$($ssh_agent -s)
eval "$agent_env" >/dev/null
"$ssh_add" "$root/client" >/dev/null

port=$(python3 - <<'PY'
import socket
with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    print(s.getsockname()[1])
PY
)

cat >"$root/sshd_config" <<EOF
HostKey $root/host
PidFile $root/sshd.pid
Port $port
ListenAddress 127.0.0.1
AuthorizedKeysFile $root/.ssh/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
AllowUsers ${USER}
LogLevel ERROR
EOF

"$sshd" -f "$root/sshd_config" -E "$root/sshd.log"
for _ in $(seq 1 50); do
	[ -e "$root/sshd.pid" ] && break
	sleep 0.1
done
pid=$(cat "$root/sshd.pid")
fingerprint=$("$ssh_keygen" -lf "$root/host.pub" -E sha256 | awk '{print $2}')

hosts="$root/hosts.toml"
cat >"$hosts" <<EOF
[hosts.loopback]
address = "127.0.0.1"
port = $port
user = "${USER}"
host_key = "$fingerprint"
auth = { type = "agent" }
timeout_secs = 30
EOF

OMP_SSH_TEST_HOSTS="$hosts" "$@"
