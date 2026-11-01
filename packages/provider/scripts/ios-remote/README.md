# Remote Xcode lab PoC

Use a local Mac's Xcode against an iPhone already available in **YARD - Device
Farm**, while keeping the browser screen and controls on USB. Apple development
traffic takes a separate path: Mac → Linux relay → iPhone Wi-Fi. Builds and
signing stay on the Mac. Nothing passes through the coordinator.

This is the first hardware experiment, ported from the sibling `ios-remote`
prototype. All code needed at runtime is in this directory; the sibling checkout
is not needed. These files have not been executed as part of preparing this PoC.

Reserve the phone manually and keep its YARD browser session open throughout.
**The scripts do not check reservations or stop on release.** Stop both relays
before releasing the phone. This experiment tests connectivity and coexistence;
it does not establish reservation isolation. Use one phone and one developer Mac
on a private lab network/VPN. Source-IP filtering cannot distinguish developers
behind the same NAT. Apple trust persists after a relay stops.

## Prepare the two machines

- Linux test host: Python 3.11+, OpenSSH server with remote forwarding allowed,
  a running `avahi-daemon`, system D-Bus and `libsystemd`. Run these scripts on
  the host, outside the provider container, as your normal user. They do not
  need root or own the USB transport. On Debian/Ubuntu the relevant packages
  are `python3`, `openssh-server`, `avahi-daemon` and `libsystemd0`.
- Mac: Apple silicon, Python 3.11+, native Xcode 27 with Device Hub, and SSH/SCP access to Linux.
  No Python packages need installing. Xcode's normal signing and provisioning
  requirements still apply.
- iPhone: iOS 27+, already trusted by the YARD provider over USB, Developer Mode
  enabled, browser screen/input working, and Wi-Fi reachable from Linux. Keep
  USB attached. A fresh **developer Mac** is supported by the proposed pairing
  flow; the phone itself must already be provisioned for YARD.
- The phone-facing Linux interface must receive/publish mDNS on the phone LAN.
  The phone must be able to reach the Linux pairing listener (TCP 55732 in the
  examples). The Mac must reach the Linux development listener on the phone's
  RemotePairing port plus **TCP and UDP** 62000–62300, with replies allowed.
  These are observed ports, not a guaranteed Apple range. The two hops must use
  the same range. The development relay only needs unicast across the VPN.

No system service, routing, firewall, hosts-file or Apple daemon configuration
is changed by the scripts. Only foreground listeners and temporary Bonjour
records are created.

The example addresses below are placeholders:

| Setting | Example | Meaning |
| --- | --- | --- |
| SSH target | `developer@lab-host` | An SSH host or an alias from your SSH config |
| Linux VPN IP | `10.30.0.10` | Assigned to Linux and reachable from the Mac |
| Linux phone-side IP | `10.20.0.10` | Assigned to Linux and reachable from the phone |
| Phone Wi-Fi IP | `10.20.0.5` | Use a DHCP reservation if possible |
| Mac source IP | `10.99.0.42` | Source address Linux actually receives after VPN/NAT |
| Mac LAN interface | `en0` | Use the Mac's active Wi-Fi/Ethernet interface |
| Linux phone-side interface | `eth0` | Interface on the phone's mDNS network |

The Linux addresses can be identical on a simpler network. Verify the phone IP
belongs to the intended YARD phone; Bonjour identifiers are not necessarily its
UDID. Keep the Mac off the phone's LAN during the remote test so direct discovery
cannot produce a false success.

On the **Mac**, point `POC_DIR` at this directory and replace these values. If
using the standalone archive, extract it with
`tar -xzf yard-ios-remote-poc.tar.gz` and point `POC_DIR` at the extracted
`ios-remote` directory. A full YARD checkout is not needed for these scripts.

```sh
POC_DIR=/path/to/yard/packages/provider/scripts/ios-remote
LAB_SSH=developer@lab-host
LAB_RELAY_IP=10.30.0.10
MAC_INTERFACE=en0

ssh "$LAB_SSH" 'install -d -m 700 yard-ios-poc/state'
scp "$POC_DIR/"*.py "$POC_DIR/"*.md "${LAB_SSH}:yard-ios-poc/"
cd "$POC_DIR"
umask 077
mkdir -p state
```

On **Linux**, in a terminal that will run the relay:

```sh
cd ~/yard-ios-poc
PHONE_IP=10.20.0.5
PHONE_INTERFACE=eth0
LAB_PHONE_SIDE_IP=10.20.0.10
LAB_RELAY_IP=10.30.0.10
MAC_SOURCE_IP=10.99.0.42
umask 077
```

`state/` and Python bytecode are ignored by git. Capture files contain private
Bonjour metadata, including authentication tags, but no Apple pairing private
keys. Scripts write them atomically with owner-only permissions. Transfer only
the specified files and avoid posting their contents with results.

## Pair a developer Mac that has never trusted this phone

Skip this section only for the initial already-paired connectivity check. A
conclusive fresh-pairing test needs a Mac with no trust history for the phone.
Do not reset all phone trust: that would also risk YARD's provider connection.

On the **Mac**, open Device Hub → Add Device → Pair Nearby Device. Choose the
iPhone/iPad category and leave the waiting dialog open. Then capture its live
advertisement and copy it to Linux:

```sh
python3 pairing.py capture --record state/host.json --interface "$MAC_INTERFACE"
scp -p state/host.json "${LAB_SSH}:yard-ios-poc/state/host.json"
```

If several Macs advertise, rerun with `--instance` and the instance belonging to
this Mac. Capture again whenever you reopen the dialog: its native port and
discovery data can change.

In that same **Mac** terminal, start the return path and leave it running:

```sh
MAC_LAN_IP="$(ipconfig getifaddr "$MAC_INTERFACE")"
MAC_PAIR_PORT="$(python3 -c 'import json; print(json.load(open("state/host.json"))["port"])')"
ssh -N -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -R "127.0.0.1:45732:${MAC_LAN_IP}:${MAC_PAIR_PORT}" "$LAB_SSH"
```

The phone initiates pairing toward Linux. SSH carries it back to the real native
Mac listener. It is not a replacement pairing identity owned by Linux. The SSH
listener stays on Linux loopback; no `GatewayPorts` change is required. Do not
probe the native Mac pairing listener with `nc` or an empty TCP connection: that
can consume or disturb a pairing attempt.

On **Linux**, publish the Mac on the phone LAN and leave this running:

```sh
python3 -u pairing.py serve --record state/host.json \
  --bind "$LAB_PHONE_SIDE_IP" --allow "$PHONE_IP" \
  --interface "$PHONE_INTERFACE" --forward 45732 --listen 55732 --timeout 600
```

In the **YARD browser**, open the phone's Settings → Privacy & Security →
Developer Mode and follow its computer-pairing flow. Select the Mac whose name
ends in `via YARD - Device Farm`, enter the PIN shown in native Device Hub, and
approve any consent prompts. Exact screens may differ by iOS build. Keep the
Mac dialog open throughout. The test fails if a required screen is hidden or
rejects browser input and needs someone at the phone.

After Device Hub completes pairing, press **Ctrl-C on Linux** to withdraw the
pairing advertisement, then **Ctrl-C on the Mac** to stop the SSH return path.
Pairing times out after ten minutes by default. A PIN prompt or a cached device
entry alone does not establish success; perform a live request below.

## Connect native Xcode through Linux

On **Linux**, capture the phone's current advertisement after pairing:

```sh
python3 pairing.py capture-device --record state/phone.json \
  --iphone "$PHONE_IP" --interface "$PHONE_INTERFACE" --timeout 8
```

If multiple instances resolve to that phone IP, the script lists them and stops.
Repeat with `--instance INSTANCE` for the intended current instance. An old
capture may become stale after pairing, a network change or a reboot.

Start the development relay on **Linux**, keeping it in the foreground:

```sh
python3 -u bridge.py relay --config state/phone.json \
  --bind "$LAB_RELAY_IP" --allow "$MAC_SOURCE_IP" \
  --ports 62000-62300 --timeout 1800
```

In a **new Mac terminal**, set the same variables, copy the fresh capture and
start the local advertisement and relays:

```sh
cd /path/to/ios-remote
LAB_SSH=developer@lab-host
LAB_RELAY_IP=10.30.0.10
MAC_INTERFACE=en0
scp -p "${LAB_SSH}:yard-ios-poc/state/phone.json" state/phone.json
python3 -u bridge.py connect --config state/phone.json \
  --via "$LAB_RELAY_IP" --interface "$MAC_INTERFACE" \
  --ports 62000-62300 --timeout 1800
```

The Mac advertises a separate proxy hostname against its own LAN IPv4 address.
This preserves the native interface-scoping workaround and avoids cached phone
IPv6 addresses bypassing the relay. Local listeners accept only this Mac's LAN
source address. Keep this terminal running; network/interface changes require a
restart. Both development processes stop after 30 minutes by default.

In another **Mac** terminal, allow discovery to settle, then run:

```sh
xcrun devicectl list devices --timeout 15
DEVICE_ID='replace with the device identifier from the list'
xcrun devicectl device info apps --device "$DEVICE_ID" --timeout 25
```

Start with Device Hub's own screen viewer closed. In local Xcode, select this
physical phone as the run destination, build/run a small development-signed app,
hit a breakpoint, inspect a value and resume. Keep using the YARD browser. Then
repeat with normal Device Hub viewing enabled to check whether Apple's second
screen session disrupts YARD's media or input.

Stop **both development processes with Ctrl-C before releasing the reservation**.
Closing them removes their advertisements/listeners and cancels active TCP/UDP
flows. It does not uninstall the app or unpair the Mac. A hard process kill is
cleaned up by the OS, but native Xcode may retain a stale visible device entry.
With the phone unreachable directly, a fresh live app-list request after stopping
the relays must fail. Release in YARD alone will not stop this standalone PoC.

## Record the result and recover

Copy [RESULTS.example.md](RESULTS.example.md) to `state/results.md` and record the
versions and each pass/fail. Discovery, pairing, live app listing, install/launch,
breakpoints and browser coexistence are separate milestones. Record whether the
phone was previously paired with the developer Mac. Do not call an already-paired
run proof of fresh trust.

- No Avahi service: provision/start the host's Avahi and system D-Bus first.
  The scripts do not install or start them. A Docker-only mDNS setup is not
  sufficient for these host processes.
- Multiple Bonjour instances: choose `--instance`; do not guess by device-list
  order. Refresh the capture after a pairing or network change.
- Pairing stalls: keep the native dialog open, recapture after reopening it,
  recopy `host.json`, and recreate the SSH return path. Check Linux's phone-facing
  IP and the captured Mac LAN IP. A stale native port was a failure in the
  original prototype. The helper never probes that listener.
- Native PIN flow completes but the phone stays unavailable: first stop/restart
  the normal relays with fresh `phone.json`. The original prototype sometimes
  needed the following **manual, last-resort** Mac recovery. It interrupts this
  user's other Apple development connections; the scripts never do it:

  ```sh
  pkill -u "$(id -u)" -x remotepairingd
  pkill -u "$(id -u)" -x CoreDeviceService
  ```

- App listing works but install/debug fails: inspect the native Xcode error,
  signing/provisioning and actual tunnel ports. Both processes need the same
  `--ports` range and corresponding TCP/UDP firewall access. The observed range
  is not proof that every Xcode service uses it.
- A bind/file-descriptor error fails the whole startup and closes resources
  already opened. Stop a conflicting bridge or adjust the observed port range.
  One phone per relay listen address is the supported PoC scope.
- `Rejected TCP source` on Linux: use that observed source for `--allow` after
  confirming it is your Mac's VPN/NAT address. An address displayed by the VPN
  client or the SSH hostname's route may differ from this relay's route.
- The phone leaves Wi-Fi: rejoin the lab network, confirm its IP, recapture and
  restart. This is independent of the USB browser session. If browser input or
  streaming stops when Xcode connects, record the exact action that caused it.

## Checks you can run

These checks use the standard library and local loopback sockets/mocks. They do
not contact a phone or start native Apple development services:

```sh
cd /path/to/ios-remote
python3 -B -m unittest -v test_bridge test_pairing
```

On the **Linux test host**, opt into the Avahi integration check separately. It
opens an empty temporary entry group on the real system D-Bus, then closes it:

```sh
YARD_IOS_TEST_AVAHI=1 python3 -B -m unittest -v test_pairing.AvahiTests
```

The implementation was inspected without running these checks, the relays,
package installations or hardware commands, as requested.
