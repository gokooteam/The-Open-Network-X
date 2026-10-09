#!/bin/sh
# Set up and check ONX's website hosting from your own device, over SSH.
#
# Everything docs/guides/hosting.md does through the cPanel web pages, this
# does from a terminal (Termux, a laptop, anything with ssh):
#
#   onx-hosting.sh init      create the config file and an SSH key (once)
#   onx-hosting.sh keys      install the key on the three servers (once;
#                            asks for each server's password one last time)
#   onx-hosting.sh status    read-only: DNS, document roots, cron jobs, logs,
#                            and which on-x-scan.com A record to delete
#   onx-hosting.sh deploy    install the deploy cron job on both cPanel hosts
#                            and run it once now
#   onx-hosting.sh vm        read-only: how onxd and nginx are set up on the VM
#
# Credentials: an SSH key (default ~/.ssh/onx_ed25519) and a config file
# (default ~/.config/onx/hosting.env, mode 600). Neither is ever written into
# the repository. No passwords are stored.
#
# On the cPanel hosts it uses cPanel's own command-line API (`uapi`), which
# every cPanel account has in its shell.

set -eu

REPO="gokooteam/The-Open-Network-X"
CONF=${ONX_HOSTING_CONF:-$HOME/.config/onx/hosting.env}
SCAN_ZONE="on-x-scan.com"
LIVE_DOMAIN="on-x.live"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
say() { printf '\n== %s\n' "$*"; }

load_conf() {
  [ -f "$CONF" ] || die "no config at $CONF; run: $0 init"
  # shellcheck disable=SC1090
  . "$CONF"
  for v in KEY LIVE_SSH LIVE_PORT SCAN_SSH SCAN_PORT VM_SSH VM_PORT VM_IP; do
    eval "val=\${$v:-}"
    [ -n "$val" ] || die "$v is empty in $CONF"
  done
  case "$LIVE_SSH$SCAN_SSH" in
    *CPANELUSER*) die "replace CPANELUSER in $CONF with your cPanel username(s)" ;;
  esac
  [ -f "$KEY" ] || die "no SSH key at $KEY; run: $0 init"
}

# remote <user@host> <port> [args...] < script
# Runs the script from stdin with POSIX sh on the remote host.
remote() {
  host=$1; port=$2; shift 2
  ssh -i "$KEY" -p "$port" -o IdentitiesOnly=yes -o BatchMode=yes \
    -o ConnectTimeout=20 "$host" sh -s -- "$@"
}

cmd_init() {
  mkdir -p "$(dirname "$CONF")"
  if [ -f "$CONF" ]; then
    echo "config already exists: $CONF"
  else
    umask 077
    cat > "$CONF" <<'EOF'
# ONX hosting: who to SSH into. Edit the CPANELUSER parts.
# Your cPanel username is on the cPanel home page (right-hand column,
# "Current User"), or in the hosting welcome email.

# SSH key used for all three servers.
KEY="$HOME/.ssh/onx_ed25519"

# cPanel account that hosts on-x.live. Namecheap shared hosting uses SSH
# port 21098. The IP is on-x.live's server.
LIVE_SSH="CPANELUSER@67.223.118.124"
LIVE_PORT=21098

# cPanel account that hosts on-x-scan.com (and its DNS zone). If it is the
# same account as on-x.live, use the same values.
SCAN_SSH="CPANELUSER@198.54.114.221"
SCAN_PORT=21098

# The devnet VM (BitLaunch).
VM_SSH="root@192.241.136.73"
VM_PORT=22
VM_IP="192.241.136.73"
EOF
    chmod 600 "$CONF"
    echo "wrote $CONF: edit it and replace CPANELUSER"
  fi
  # shellcheck disable=SC1090
  . "$CONF"
  if [ -f "$KEY" ]; then
    echo "SSH key already exists: $KEY"
  else
    mkdir -p "$(dirname "$KEY")" && chmod 700 "$(dirname "$KEY")"
    echo "Creating an SSH key. A passphrase protects it if the phone is lost;"
    echo "leaving it empty means no typing on every run. Your choice."
    ssh-keygen -t ed25519 -f "$KEY" -C "onx-hosting"
  fi
  cat <<EOF

Next:
  1. Edit $CONF (your cPanel username).
  2. In each cPanel account, turn SSH on once: "Manage Shell" -> Enable.
  3. Run: $0 keys
EOF
}

cmd_keys() {
  load_conf
  command -v ssh-copy-id >/dev/null 2>&1 || die "ssh-copy-id is missing (apt install openssh-client)"
  seen=""
  for t in "$LIVE_SSH $LIVE_PORT" "$SCAN_SSH $SCAN_PORT" "$VM_SSH $VM_PORT"; do
    case " $seen " in *" $t "*) continue ;; esac
    seen="$seen $t"
    set -- $t
    say "$1 (port $2): enter that server's password one last time"
    ssh-copy-id -i "$KEY.pub" -p "$2" "$1"
  done
  say "testing key logins (no passwords from here on)"
  for t in "$LIVE_SSH $LIVE_PORT" "$SCAN_SSH $SCAN_PORT" "$VM_SSH $VM_PORT"; do
    set -- $t
    if echo 'echo ok' | remote "$1" "$2" >/dev/null 2>&1; then echo "ok    $1"; else echo "FAIL  $1"; fi
  done
}

# Shell function shared by the cPanel-side scripts: the document root of a
# domain in this cPanel account.
REMOTE_LIB='
docroot_of() {
  uapi --output=json DomainInfo single_domain_data domain="$1" |
    perl -MJSON::PP -0777 -ne '"'"'my $d = decode_json($_); print $d->{result}{data}{documentroot} // ""'"'"'
}
'

# Runs on a cPanel host: install the deploy cron job for one site and run it.
REMOTE_DEPLOY="$REMOTE_LIB"'
set -eu
src=$1; domain=$2; repo=$3
docroot=$(docroot_of "$domain" || true)
[ -n "$docroot" ] || { echo "cannot find the document root of $domain in this cPanel account"; exit 1; }
echo "document root of $domain: $docroot"
mkdir -p "$HOME/onx"
curl -fsS --max-time 60 -o "$HOME/onx/site-pull-deploy.sh.new" "https://raw.githubusercontent.com/$repo/main/scripts/site-pull-deploy.sh"
mv -f "$HOME/onx/site-pull-deploy.sh.new" "$HOME/onx/site-pull-deploy.sh"
tag="# onx-deploy $domain"
line="*/10 * * * * /bin/sh $HOME/onx/site-pull-deploy.sh $src $docroot/index.html >> $HOME/onx-deploy.log 2>&1 $tag"
{ crontab -l 2>/dev/null | grep -vF "$tag" || true; echo "$line"; } > "$HOME/.onx-cron.$$"
crontab "$HOME/.onx-cron.$$"; rm -f "$HOME/.onx-cron.$$"
echo "cron job installed (every 10 minutes)"
if [ -f "$docroot/index.php" ]; then
  echo "WARNING: $docroot/index.php exists and is served instead of index.html."
  echo "         To serve the ONX page: mv $docroot/index.php $docroot/index.php.old"
fi
echo "running it once now:"
if /bin/sh "$HOME/onx/site-pull-deploy.sh" "$src" "$docroot/index.html"; then
  echo "ok: $docroot/index.html is the newest checked commit on main"
else
  echo "not deployed this time (see the message above); cron retries every 10 minutes"
fi
'

# Runs on the host with the on-x-scan.com zone. Read-only: lists the A
# records and marks the apex/www ones that point at the VM.
REMOTE_DNS='
set -eu
zone=$1; ip=$2
uapi --output=json DNS parse_zone zone="$zone" | perl -MJSON::PP -MMIME::Base64 -0777 -e '"'"'
  my ($zone, $ip) = @ARGV;
  my $d = decode_json(scalar <STDIN>);
  die "parse_zone failed: " . join("; ", @{$d->{result}{errors} || []}) . "\n" unless $d->{result}{status};
  my $n = 0;
  for my $r (@{$d->{result}{data} || []}) {
    next unless ($r->{type} // "") eq "record" && ($r->{record_type} // "") eq "A";
    my $name = decode_base64($r->{dname_b64} // "");
    my $v = decode_base64(($r->{data_b64} || [])->[0] // "");
    $name =~ s/\.$//;
    $name = $zone if $name eq "" || $name eq "@";
    $name .= ".$zone" unless $name eq $zone || $name =~ /\.\Q$zone\E$/;
    my $hit = $v eq $ip && ($name eq $zone || $name eq "www.$zone");
    $n++ if $hit;
    printf "  %s A %-24s %s\n", ($hit ? "DELETE" : "keep  "), $name, $v;
  }
  print $n
    ? "\nDelete the DELETE line(s) in cPanel: Domains -> Zone Editor -> $zone -> Manage.\nThen: Security -> SSL/TLS Status -> Run AutoSSL.\n"
    : "\nOK: no apex or www A record points at $ip.\n";
'"'"' "$zone" "$ip"
'

REMOTE_STATUS="$REMOTE_LIB"'
domain=$1
echo "document root of $domain: $(docroot_of "$domain" 2>/dev/null)"
echo "cron:"; crontab -l 2>/dev/null | grep "onx-deploy" | sed "s/^/  /" || echo "  (no onx-deploy job)"
echo "last deploy log lines:"; tail -n 5 "$HOME/onx-deploy.log" 2>/dev/null | sed "s/^/  /" || echo "  (no log yet)"
'

REMOTE_VM='
echo "-- host"; hostname; uptime
[ -f /var/run/reboot-required ] && echo "reboot required: yes" || echo "reboot required: no"
echo "-- onxd services"; systemctl list-units --type=service --all --no-pager 2>/dev/null | grep -i onx || echo "(no systemd unit named onx*)"
systemctl list-unit-files --no-pager 2>/dev/null | grep -i onx || true
echo "-- onxd processes"; ps -eo pid,user,etime,args | grep -i "[o]nxd" || echo "(onxd is not running)"
echo "-- nginx sites"; grep -rn "server_name\|root \|alias " /etc/nginx/sites-enabled/ 2>/dev/null || echo "(no nginx sites-enabled)"
echo "-- certificates"; ls /etc/letsencrypt/live/ 2>/dev/null || echo "(no letsencrypt certificates)"
echo "-- root crontab"; crontab -l 2>/dev/null || echo "(empty)"
echo "-- disk"; df -h / | tail -n 1
'

cmd_status() {
  load_conf
  say "DNS (what the world sees)"
  for h in on-x.live www.on-x.live on-x-scan.com www.on-x-scan.com data.on-x-scan.com; do
    if command -v getent >/dev/null 2>&1; then
      printf '  %-20s %s\n' "$h" "$(getent ahostsv4 "$h" | awk '{print $1}' | sort -u | tr '\n' ' ')"
    fi
  done
  say "on-x.live host ($LIVE_SSH)"
  printf '%s' "$REMOTE_STATUS" | remote "$LIVE_SSH" "$LIVE_PORT" "$LIVE_DOMAIN"
  say "on-x-scan.com host ($SCAN_SSH)"
  printf '%s' "$REMOTE_STATUS" | remote "$SCAN_SSH" "$SCAN_PORT" "$SCAN_ZONE"
  say "on-x-scan.com A records"
  printf '%s' "$REMOTE_DNS" | remote "$SCAN_SSH" "$SCAN_PORT" "$SCAN_ZONE" "$VM_IP"
}

cmd_deploy() {
  load_conf
  say "on-x.live ($LIVE_SSH)"
  printf '%s' "$REMOTE_DEPLOY" | remote "$LIVE_SSH" "$LIVE_PORT" site/index.html "$LIVE_DOMAIN" "$REPO"
  say "on-x-scan.com ($SCAN_SSH)"
  printf '%s' "$REMOTE_DEPLOY" | remote "$SCAN_SSH" "$SCAN_PORT" explorer/index.html "$SCAN_ZONE" "$REPO"
}

cmd_vm() {
  load_conf
  say "devnet VM ($VM_SSH)"
  printf '%s' "$REMOTE_VM" | remote "$VM_SSH" "$VM_PORT"
}

case "${1:-}" in
  init) cmd_init ;;
  keys) cmd_keys ;;
  status) cmd_status ;;
  deploy) cmd_deploy ;;
  vm) cmd_vm ;;
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
