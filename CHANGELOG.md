# Changelog

## 0.4.5

### Added

- Keep unfinished text drafts encrypted on this device and restore them after
  restarting, including protected-message drafts and their protection mode.
- Send protected text and images with concealed previews and controlled viewing;
  protected content stays out of Favorites.
- Catch up on group @mentions and choose how much recent text history to share with
  newly invited members.
- Edit and view account biographies and creation information, with private
  contact verification kept separate from public profile details.
- Collect messages and personal notes in Favorites, add descriptions, and keep
  eligible attachments independently of ordinary chat history.
- Back up and restore encrypted account data through local files, including
  Favorites and profile information. Google Drive backup is available in builds
  configured for it. Keep the recovery key separately; Google sign-in does not
  replace it.
- Let approved local agents read permitted conversation context and send messages
  through scoped access configured in Settings.
- Capture screenshots on supported Linux X11 desktops when the screenshot portal
  is unavailable, using the optional `xfce4-screenshooter` tool.

### Fixed

- Recover from transient node identity failures without requiring an app restart.
- Improve delayed-message recovery and unread navigation, account restoration,
  and chat responsiveness during profile changes and interrupted sends.

## [0.4.4](https://github.com/OctopusGarage/mesh-talk/compare/v0.4.3...v0.4.4) (2026-10-22)


### Features

* automate nightly updates and the version lifecycle ([#254](https://github.com/OctopusGarage/mesh-talk/issues/254)) ([105f8e3](https://github.com/OctopusGarage/mesh-talk/commit/105f8e3dcb76bf892e66f2f2da6b7a747f83f1bd))
* **chat:** show local delivery details for awaiting messages ([c233563](https://github.com/OctopusGarage/mesh-talk/commit/c233563e2b8bcf86cfe963a3ac6d26112778545b))
* **delivery:** expose bounded runtime attempt observations ([2ba0bec](https://github.com/OctopusGarage/mesh-talk/commit/2ba0becb9133f274ef722671f7ca678ada83e87b))
* improve exact search, delivery recovery and history costs ([6705bb1](https://github.com/OctopusGarage/mesh-talk/commit/6705bb19381e21440d0a17120f81131ecf916640))
* improve profile locking, delivery details, and search APIs ([12f6878](https://github.com/OctopusGarage/mesh-talk/commit/12f68788e5293d4e56f1ff78fddb77b17b211f59))
* **search:** add canonical recent-history references ([72f927d](https://github.com/OctopusGarage/mesh-talk/commit/72f927da4ba091c489fa2389546a53663a1cd016))
* **search:** expose owner-scoped bounded message context ([9f79c16](https://github.com/OctopusGarage/mesh-talk/commit/9f79c16cd343539e7ded56b5068e3392559773f7))


### Bug Fixes

* **auth:** streamline first-use account entry and saved login ([#261](https://github.com/OctopusGarage/mesh-talk/issues/261)) ([331adc9](https://github.com/OctopusGarage/mesh-talk/commit/331adc97541a808d80bf05d3bcc511ab27ae4567))
* **cli:** hold an OS lock for the node data directory ([70c8756](https://github.com/OctopusGarage/mesh-talk/commit/70c8756b31661e7af8c937a3ca15ca574997d881))
* **core:** retry profile publication and advance opaque relay pages ([d782c8d](https://github.com/OctopusGarage/mesh-talk/commit/d782c8de2b5179832714ae584c15edbff97cae30))
* keep chat responsive during sends and profile updates ([#259](https://github.com/OctopusGarage/mesh-talk/issues/259)) ([27dfd00](https://github.com/OctopusGarage/mesh-talk/commit/27dfd00228f61c838791f9bbca7be84ca1ca4e0c))
* **release:** bound legacy history at the verified source baseline ([#256](https://github.com/OctopusGarage/mesh-talk/issues/256)) ([c8de4f8](https://github.com/OctopusGarage/mesh-talk/commit/c8de4f8a1335c7846363a77861a9a46771ca588a))
* **validation:** align profile selectors with source evidence ([0dab724](https://github.com/OctopusGarage/mesh-talk/commit/0dab7243a6c7254fd352b58f212c4c66772ebe79))
* **validation:** include search consumers and delivery browser evidence ([8d40688](https://github.com/OctopusGarage/mesh-talk/commit/8d4068828938490ef3c1309908cdda2aa8b958ac))
* **validation:** require the delivery recovery portable scenario ([20358fc](https://github.com/OctopusGarage/mesh-talk/commit/20358fc8e7cd290f74545d177334e0b75e209965))
