# Changelog

## [0.4.5](https://github.com/OctopusGarage/mesh-talk/compare/v0.4.4...v0.4.5) (2026-10-23)


### Features

* add message Favorites with searchable notes ([254be2e](https://github.com/OctopusGarage/mesh-talk/commit/254be2e48ad7aee4145b2c37e64870b0959ec7e8))
* identify presence sources and adapt relay selection ([#270](https://github.com/OctopusGarage/mesh-talk/issues/270)) ([a2698a5](https://github.com/OctopusGarage/mesh-talk/commit/a2698a58d2b5390f0984b564e8b6700b4dec85ee))
* integrate messaging, profiles, recovery and local agent access ([#278](https://github.com/OctopusGarage/mesh-talk/issues/278)) ([1bb6483](https://github.com/OctopusGarage/mesh-talk/commit/1bb6483d3e5c9db093ca3f5760970460c6653b01))
* make lightweight desktop packages the default ([#267](https://github.com/OctopusGarage/mesh-talk/issues/267)) ([222388f](https://github.com/OctopusGarage/mesh-talk/commit/222388fa8e0f7405bb10295db3c569021a75089d))


### Bug Fixes

* **chat:** send staged screenshots after capture loses focus ([#266](https://github.com/OctopusGarage/mesh-talk/issues/266)) ([7c88ee7](https://github.com/OctopusGarage/mesh-talk/commit/7c88ee7756de9f1fa4b0bcaefd425937552953ec))
* **release:** harden draft preparation and state reads ([#263](https://github.com/OctopusGarage/mesh-talk/issues/263)) ([cf91e4c](https://github.com/OctopusGarage/mesh-talk/commit/cf91e4ce6a56d8c5ae83ffe83563a31ddd97aeb5))
* **ui:** guide users through system notification permission ([#262](https://github.com/OctopusGarage/mesh-talk/issues/262)) ([6e7cde3](https://github.com/OctopusGarage/mesh-talk/commit/6e7cde3e7329875b4efa356ca009ae023549d2df))

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
