# Hosting runbook: on-x.live, on-x-scan.com and the devnet VM

This is the step-by-step for connecting the two websites to the deployment
and monitoring this repository already has ([`site/README.md`](../../site/README.md)
describes that machinery). It is written for someone doing it for the first
time, from a phone, with cPanel open in one tab and an SSH session in the
other.

## The scripted way (recommended)

[`scripts/onx-hosting.sh`](../../scripts/onx-hosting.sh) does steps 2–5
below over SSH, from any terminal (Termux, a laptop). It keeps one SSH key
and a small config file on your device, outside the repository, and stores
no passwords.

```sh
curl -fsSLO https://raw.githubusercontent.com/gokooteam/The-Open-Network-X/main/scripts/onx-hosting.sh
sh onx-hosting.sh init      # makes ~/.config/onx/hosting.env and ~/.ssh/onx_ed25519
#   edit hosting.env: put your cPanel username where it says CPANELUSER
#   in each cPanel account, once: "Manage Shell" -> Enable
sh onx-hosting.sh keys      # asks each server's password one last time
sh onx-hosting.sh status    # read-only: docroots, cron jobs, which DNS record to delete
sh onx-hosting.sh deploy    # installs both cron jobs and deploys right away
sh onx-hosting.sh vm        # read-only: how onxd and nginx run on the VM
```

The one thing it does not do is change DNS: `status` shows which
on-x-scan.com record to delete, and step 1 below is that deletion in cPanel.
On a cPanel host it uses cPanel's command-line API (`uapi`) for the document
roots and the zone, and `crontab` for the jobs.

## What runs where

Measured from outside on 2026-10-09:

| Name | Points at | Server | Serves |
| --- | --- | --- | --- |
| `on-x.live`, `www.on-x.live` | `67.223.118.124` | Namecheap shared hosting (cPanel, LiteSpeed) | an old hand-edited copy of the project site |
| `on-x-scan.com`, `www.on-x-scan.com` | `198.54.114.221` **and** `192.241.136.73` | one Namecheap shared host (cPanel, LiteSpeed) and the devnet VM (nginx) | an old explorer from whichever server answers |
| `data.on-x-scan.com` | `192.241.136.73` | the devnet VM (nginx) | devnet-1's block files under `/devnet-1/` |

DNS for `on-x-scan.com` is edited in **cPanel → Domains → Zone Editor** (its
nameservers are `dns1/dns2.namecheaphosting.com`). DNS for `on-x.live` is
edited in the **Namecheap account dashboard → Domain List → Manage →
Advanced DNS** (its nameservers are `dns1/dns2.registrar-servers.com`), not
in cPanel.

## The decision: which server serves the explorer

`on-x-scan.com` has two A records, so every visitor gets one of two servers
at random. A domain needs exactly one place that answers for it. This
runbook picks **the cPanel host**, for three reasons:

1. It already has a valid certificate for `on-x-scan.com` (cPanel's AutoSSL
   renews it), so there is nothing to configure for HTTPS.
2. Both websites are then deployed the same way: one cPanel cron job each.
3. The VM stays dedicated to the chain. It keeps serving
   `data.on-x-scan.com`, which the explorer already reads cross-origin
   (`Access-Control-Allow-Origin: *` is set there).

The alternative (explorer on the VM) works too but needs an nginx server
block and a certificate covering `on-x-scan.com` and `www`; nothing below
depends on the choice except step 1.

## Step 1: one A record for on-x-scan.com

In the cPanel account that hosts `on-x-scan.com`: **Domains → Zone Editor →
on-x-scan.com → Manage**.

1. Find the `A` records named `on-x-scan.com.`. There are two.
2. **Delete the one whose value is `192.241.136.73`.** Keep `198.54.114.221`.
3. Do **not** touch `data.on-x-scan.com.` (it must stay `192.241.136.73`),
   the `MX` records (mail), or anything else.
4. `www.on-x-scan.com.` is a `CNAME` to the apex, so it follows by itself.
   If there is instead an `A` record for `www` with `192.241.136.73`,
   delete that one too.

Note the record's TTL (seconds). Old answers can be cached for that long, so
the next steps may look half-done until it has passed.

Then **Security → SSL/TLS Status → Run AutoSSL**, so the certificate covers
both `on-x-scan.com` and `www.on-x-scan.com`.

## Step 2: find each document root

In each cPanel account: **Domains → Domains**. The table shows the
*Document Root* for each domain (for the account's main domain this is
usually `public_html`; for an addon domain it is a folder of its own). Write
both down. Below they are `DOCROOT_LIVE` and `DOCROOT_SCAN`, relative to the
home directory.

Open each document root in **Files → File Manager** and look for an
`index.php`. If one exists, the server shows it instead of `index.html`;
rename it to `index.php.old`.

## Step 3: one cron job per site

In each cPanel account: **Advanced → Cron Jobs → Add New Cron Job**. Set
*Common Settings* to "Once Per Ten Minutes" (`*/10 * * * *`) and paste one
line as the command, with your document root filled in.

For on-x.live:

```sh
[ -f "$HOME/onx/site-pull-deploy.sh" ] || { mkdir -p "$HOME/onx" && curl -fsS -o "$HOME/onx/site-pull-deploy.sh" https://raw.githubusercontent.com/gokooteam/The-Open-Network-X/main/scripts/site-pull-deploy.sh; }; /bin/sh "$HOME/onx/site-pull-deploy.sh" site/index.html "$HOME/DOCROOT_LIVE/index.html" >> "$HOME/onx-deploy.log" 2>&1
```

For on-x-scan.com:

```sh
[ -f "$HOME/onx/site-pull-deploy.sh" ] || { mkdir -p "$HOME/onx" && curl -fsS -o "$HOME/onx/site-pull-deploy.sh" https://raw.githubusercontent.com/gokooteam/The-Open-Network-X/main/scripts/site-pull-deploy.sh; }; /bin/sh "$HOME/onx/site-pull-deploy.sh" explorer/index.html "$HOME/DOCROOT_SCAN/index.html" >> "$HOME/onx-deploy.log" 2>&1
```

The part before `;` downloads
[`scripts/site-pull-deploy.sh`](../../scripts/site-pull-deploy.sh) the first
time only; the rest runs it. If both domains are in the same cPanel account,
the script is downloaded once and both jobs share it.

Within ten minutes, open `onx-deploy.log` in the home directory with File
Manager. A line `deployed commit <sha>` means it worked. After that the
script logs nothing until `main` changes. A line saying the check `has not
passed` is the script doing its job: it waits until `main`'s `site-and-docs`
check is green.

## Step 4: check from outside

From any machine with a checkout:

```sh
python3 scripts/site.py live --attempts 6
```

Every URL should say it matches. The
[`Site monitor`](../../.github/workflows/site-monitor.yml) workflow runs the
same thing every six hours and closes its `site-drift` issue once both sites
match; trigger it by hand from the Actions tab to see it straight away.

## Step 5 (optional): tidy the VM

Once DNS no longer sends `on-x-scan.com` to the VM, its nginx block for
that name is unused. On the VM:

```sh
grep -rn "server_name" /etc/nginx/sites-enabled/
```

If a block names `on-x-scan.com` (and not only `data.on-x-scan.com`), it can
be removed from `sites-enabled`, then `nginx -t && systemctl reload nginx`.
Leave the `data.on-x-scan.com` block alone.

## The devnet VM

What is known from outside and from the login banner:

- Public IPv4 `192.241.136.73`, private `10.10.0.92`, IPv6 in
  `2604:a880:400::/48`; rented through BitLaunch, hostname `to-x`.
- Ubuntu 26.04.1 LTS, about 57 GB of disk, nginx 1.28.3.
- Serves `https://data.on-x-scan.com/devnet-1/`: `genesis.boc` and, as of
  2026-10-09, `block-00000001.blk` only. That is expected: `onxd` produces a
  block only when a message arrives
  ([`local-network-launch.md`](local-network-launch.md)).

Not recorded anywhere in this repository yet: how `onxd` is started (systemd
unit or by hand), where its config and data directory are, and how block
files reach the nginx web root. Running this on the VM shows all of it:

```sh
systemctl list-units --type=service --all | grep -i onx
ps aux | grep -i [o]nxd
grep -rn "root\|server_name" /etc/nginx/sites-enabled/
ls -la /etc/letsencrypt/live/ 2>/dev/null
crontab -l
```

Record the answers here, so the next person (and M5, which adds a follower
node on a second machine) starts from facts.

**Before rebooting** for pending updates (`*** System restart required ***`
in the banner): find out from the commands above whether `onxd` starts by
itself. If it does not, it has to be started again by hand after the reboot.
