# Remote Xcode PoC results

- Date:
- Linux OS / Python version:
- Mac model / macOS / Xcode / Python versions:
- iPhone model / iOS build:
- Previously paired with this developer Mac: yes / no
- YARD browser screen/input working before the test: yes / no
- Mac cannot directly reach the phone and is off its LAN: yes / no
- Port range / phone-facing and Mac-facing interface names:

Use PASS, FAIL or NOT TESTED for each item. Record exact native error messages,
without Bonjour authentication tags, private captures or pairing keys.

| Check | Result | Notes |
| --- | --- | --- |
| Native Mac pairing service appears on phone through Linux | | |
| All phone-side prompts completed using only YARD browser input | | |
| Fresh Mac pairing completes in Device Hub | | |
| Live `devicectl device info apps` succeeds through both relays | | |
| Local Xcode installs and launches a development-signed app | | |
| Breakpoint hits, variable can be inspected, execution resumes | | |
| Browser video/input work during pairing | | |
| Browser video/input work during install, launch and debug | | |
| Coexistence works with Device Hub's viewer open | | |
| Stopping both relays disconnects Xcode and live app listing fails | | |
| Fresh metadata/relay restart restores connectivity | | |

Reservations are held/released manually. Automatic release revocation, user
authentication and multi-device operation are outside this standalone PoC.
