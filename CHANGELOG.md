# Changelog

## [0.4.0](https://github.com/Dinip/yard/compare/v0.3.1...v0.4.0) (2026-09-05)


### Features

* add managed app preloads ([#54](https://github.com/Dinip/yard/issues/54)) ([9219173](https://github.com/Dinip/yard/commit/92191736e93fa8bad5a98314a27fcc97eaf53bf1))


### Bug Fixes

* **web:** normalize session button styles ([#53](https://github.com/Dinip/yard/issues/53)) ([6e590ae](https://github.com/Dinip/yard/commit/6e590aeec1527bcb3197e6c35db343fe80bbc0cf))

## [0.3.1](https://github.com/Dinip/yard/compare/v0.3.0...v0.3.1) (2026-09-04)


### Bug Fixes

* **provider:** list iOS apps over the streaming feature ([#48](https://github.com/Dinip/yard/issues/48)) ([1e50542](https://github.com/Dinip/yard/commit/1e505427cff4a96708b0ff21339b12f0f5a89706))

## [0.3.0](https://github.com/Dinip/yard/compare/v0.2.0...v0.3.0) (2026-08-31)


### Features

* **web:** give admins volume, lock and reboot controls ([#51](https://github.com/Dinip/yard/issues/51)) ([cc173ac](https://github.com/Dinip/yard/commit/cc173ac11e513b0b2c3c3f19bfb10f38a36dce06))


### Bug Fixes

* **provider:** stop treating a quiet iOS encoder as a stall ([#49](https://github.com/Dinip/yard/issues/49)) ([04f359a](https://github.com/Dinip/yard/commit/04f359a0e823ff4bf6fd4931dc655c02a9490ce9))

## [0.2.0](https://github.com/Dinip/yard/compare/v0.1.2...v0.2.0) (2026-08-28)


### ⚠ BREAKING CHANGES

* **provider:** provider environment variables lose the `YARD_` prefix. `YARD_PROVIDER_TOKEN` becomes `PROVIDER_TOKEN`, `YARD_LOG` becomes `PROVIDER_LOG_LEVEL`, and `YARD_CONFIG` becomes `PROVIDER_CONFIG`. Rename these variables before pulling the new provider image.

### Features

* **provider:** isolate usbmuxd in a sidecar ([#46](https://github.com/Dinip/yard/issues/46)) ([6ae4f28](https://github.com/Dinip/yard/commit/6ae4f28b22635edbd8773b45183e49206ec2eafb))
* **web:** highlight the devices you hold in the list ([#43](https://github.com/Dinip/yard/issues/43)) ([d3c0d5f](https://github.com/Dinip/yard/commit/d3c0d5f3eef08a8cebc32881219b17d17bb55c4a))
* **web:** put the reserve action in the middle of a free device's page ([#45](https://github.com/Dinip/yard/issues/45)) ([925b094](https://github.com/Dinip/yard/commit/925b094339c925b88fb1fdd93dde3157caf2ed38))


### Bug Fixes

* **web:** release lost iOS edge swipes ([#47](https://github.com/Dinip/yard/issues/47)) ([5bfe36d](https://github.com/Dinip/yard/commit/5bfe36df998a8023b513c2064c1c5616373e260a))

## [0.1.2](https://github.com/Dinip/yard/compare/v0.1.1...v0.1.2) (2026-08-26)


### Bug Fixes

* **gateway:** stop a provider's `ready` clearing a reserved device's `busy` ([#42](https://github.com/Dinip/yard/issues/42)) ([ada7c85](https://github.com/Dinip/yard/commit/ada7c85d0c50aee0d271ecf1599da7b5e10e5eaf))


### Documentation

* **readme:** show the UI with screenshots ([#38](https://github.com/Dinip/yard/issues/38)) ([5a2e279](https://github.com/Dinip/yard/commit/5a2e27970861579cf76321a89414c879541f35bf))

## [0.1.1](https://github.com/Dinip/yard/compare/v0.1.0...v0.1.1) (2026-08-22)


### Bug Fixes

* **provider:** exec the correct binary name in the entrypoint ([#36](https://github.com/Dinip/yard/issues/36)) ([f5a1ca9](https://github.com/Dinip/yard/commit/f5a1ca98c40ce11ba2af075cadedcf2a34365180))

## 0.1.0 (2026-08-22)


### Features

* **audit:** filter the audit log, and stop the action list drifting ([#6](https://github.com/Dinip/yard/issues/6)) ([5205b49](https://github.com/Dinip/yard/commit/5205b49887a6ba2adcb1218419d1bea0d121c493))
* **auth:** make the first account an admin ([#14](https://github.com/Dinip/yard/issues/14)) ([4fb2fb3](https://github.com/Dinip/yard/commit/4fb2fb3a207914f7c505461ecdadab9a1b1f2890))
* **cleanup:** reset a device between users ([#16](https://github.com/Dinip/yard/issues/16)) ([79f35ab](https://github.com/Dinip/yard/commit/79f35ab75f6638e57516c66939355392d62c5eca))
* **devices:** browse and download device files, and record the screen ([#9](https://github.com/Dinip/yard/issues/9)) ([7fecada](https://github.com/Dinip/yard/commit/7fecada12b648b406adeedd2651397d3f5ed5df4))
* **devices:** report device identity and surface adb connect ([#2](https://github.com/Dinip/yard/issues/2)) ([6ae7ded](https://github.com/Dinip/yard/commit/6ae7deda2cdc78b82fb9a4d8ac35a1a88f971ec9))
* **popout:** screen-first layout and one stream at a time ([#3](https://github.com/Dinip/yard/issues/3)) ([05e99cc](https://github.com/Dinip/yard/commit/05e99cc362ad29172a6171306f4a937b9137be9b))
* **provider:** expose device metrics for prometheus ([#10](https://github.com/Dinip/yard/issues/10)) ([6c45ab8](https://github.com/Dinip/yard/commit/6c45ab8f7879feaabd83237b52d065bd6c619d73))
* **provider:** mount the developer disk image automatically ([#19](https://github.com/Dinip/yard/issues/19)) ([bfbd62e](https://github.com/Dinip/yard/commit/bfbd62e1e59eea6557937f8f6a833ca96c0cb10b))
* **provider:** park idle device screens ([#30](https://github.com/Dinip/yard/issues/30)) ([727e73b](https://github.com/Dinip/yard/commit/727e73b7baaed537f45cfae19efdfb9cd36ba9b4))
* **provider:** terminate adb authentication at the provider ([#25](https://github.com/Dinip/yard/issues/25)) ([0599880](https://github.com/Dinip/yard/commit/0599880539720dbb35cca798367a18ed4c198718))
* **rotation:** follow a device's rotation end to end ([#1](https://github.com/Dinip/yard/issues/1)) ([40e633e](https://github.com/Dinip/yard/commit/40e633e936043b1ba1dd36b7c64eb1d365bdf9a2))
* **sessions:** ask to join a session, plus two iOS corrections ([#7](https://github.com/Dinip/yard/issues/7)) ([ff823b0](https://github.com/Dinip/yard/commit/ff823b0edc4158e3003fbb53fd3ad97ac4003515))
* **sessions:** govern reservations with idle policy, kick disclosure and admin join ([#5](https://github.com/Dinip/yard/issues/5)) ([fd7488b](https://github.com/Dinip/yard/commit/fd7488b2f1265e9e1591ed1fb649ccf9f4cfcadf))
* **web:** add a light theme and a theme toggle ([#22](https://github.com/Dinip/yard/issues/22)) ([bd3561e](https://github.com/Dinip/yard/commit/bd3561e7a90f1666582197b297a1e8154d9b3a42))
* **web:** move adb key prompts into dialogs ([#27](https://github.com/Dinip/yard/issues/27)) ([b2ace9a](https://github.com/Dinip/yard/commit/b2ace9a4b7611ceff7b37cee8ad13a84968fbcb5))
* **web:** portrait-first device page, hardware buttons, left nav rail ([#11](https://github.com/Dinip/yard/issues/11)) ([7df5096](https://github.com/Dinip/yard/commit/7df5096ddb390f35958d0a66af4d556bf2d9ea2c))
* **web:** show the build version and a GitHub link ([#23](https://github.com/Dinip/yard/issues/23)) ([e8bb3b3](https://github.com/Dinip/yard/commit/e8bb3b33fce77e2b708a3a68071ee7a73aacf915))


### Bug Fixes

* **ci:** lowercase the registry reference ([8a5a908](https://github.com/Dinip/yard/commit/8a5a908ce3037bd865f382dcd8a290ce93c02ec7))
* **ci:** stop the formatter and the tests undoing each other ([a9f6d64](https://github.com/Dinip/yard/commit/a9f6d64a60d8e3512eb79b273f77b7f28d3c26fc))
* **ci:** strip the sha256 prefix from digest artifact names ([1a0548a](https://github.com/Dinip/yard/commit/1a0548a935076fb754993f450d4a195bce92c2bb))
* **provider:** close adb connections when a device is released ([#26](https://github.com/Dinip/yard/issues/26)) ([65730e2](https://github.com/Dinip/yard/commit/65730e2adfe453724a08a6e1ce2586b7ad02b2fb))
* **provider:** keep a device healthy and exposed across an adbd restart ([#24](https://github.com/Dinip/yard/issues/24)) ([117dcb9](https://github.com/Dinip/yard/commit/117dcb99d18fcc6502c26b04ebdcb9125d1ea04d))
* **provider:** reachable session plane and adb remote debugging ([#13](https://github.com/Dinip/yard/issues/13)) ([ada8877](https://github.com/Dinip/yard/commit/ada88771b5d607bfb96939220bb17e8f4087d66b))
* **provider:** start an adb server in the container ([#12](https://github.com/Dinip/yard/issues/12)) ([994e0f3](https://github.com/Dinip/yard/commit/994e0f373bae03e82382ae6a43f018b0a0ead759))
* **provider:** stop asking about an adb key the holder denied ([#29](https://github.com/Dinip/yard/issues/29)) ([77c4251](https://github.com/Dinip/yard/commit/77c42517be10e7b36b07152c8d0155e2e6954572))
* **reservations:** correct and quieten the idle timeout ([#15](https://github.com/Dinip/yard/issues/15)) ([bcd6450](https://github.com/Dinip/yard/commit/bcd6450c7d93b549e5689468abd68c3aa9eff882))
* **web:** follow the device orientation in the popout controls ([#18](https://github.com/Dinip/yard/issues/18)) ([3d535d3](https://github.com/Dinip/yard/commit/3d535d3c6001c0fc3c18672d058f74f5a50bf84e))
* **web:** make the whole device card open the device ([#28](https://github.com/Dinip/yard/issues/28)) ([cf46cb7](https://github.com/Dinip/yard/commit/cf46cb76933720c8fef932bfd4d76d4ef5cef4e0))
* **web:** truncate long device names in device cards ([#17](https://github.com/Dinip/yard/issues/17)) ([057c4e8](https://github.com/Dinip/yard/commit/057c4e863b67e0d7e5678365667ebefdebbc5976))


### Documentation

* **claude:** require conventional commits and doc upkeep ([#8](https://github.com/Dinip/yard/issues/8)) ([31494c2](https://github.com/Dinip/yard/commit/31494c2a377e70c577130e608c1236161c4f7003))
* sharpen the branch naming and PR conventions ([#32](https://github.com/Dinip/yard/issues/32)) ([ab9bdd1](https://github.com/Dinip/yard/commit/ab9bdd1a3c89b5959136a9fe9bc0149e9dc4be1f))
