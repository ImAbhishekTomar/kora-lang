# Changelog

## [0.1.0](https://github.com/ImAbhishekTomar/kora-lang/compare/v0.0.2...v0.1.0) (2026-08-29)


### Features

* add expense receipt classifier example ([ac24dd3](https://github.com/ImAbhishekTomar/kora-lang/commit/ac24dd3985a8b3c3c903348452e50d8ef8f94fca))
* add secure package dependencies ([37ad37b](https://github.com/ImAbhishekTomar/kora-lang/commit/37ad37b832722f1b8abeca00266398fd281aefff))
* **cli:** add, remove, and update dependencies ([375eac1](https://github.com/ImAbhishekTomar/kora-lang/commit/375eac15472b6bb685ffa63db11ef7ad82ab54b7))
* **cli:** check grants and show them in `kora tree` ([ef9fbc2](https://github.com/ImAbhishekTomar/kora-lang/commit/ef9fbc2db9c13522dd890ccabac9aba4100b4d78))
* **cli:** kora vendor and kora audit --deps ([ad0a44a](https://github.com/ImAbhishekTomar/kora-lang/commit/ad0a44ac03f3aa6a4e3e66a346737ac1ba265198))
* **cli:** report unused dependencies and add `kora tree` ([f17b7e9](https://github.com/ImAbhishekTomar/kora-lang/commit/f17b7e9b9e7b5b98da5759aa90aced992f29cdee))
* **dap:** `kora dap`, a Debug Adapter Protocol server ([cc1f681](https://github.com/ImAbhishekTomar/kora-lang/commit/cc1f68182a21ce75dcf77b64149fcdbaaeea0c80))
* images are values, so a program can look at a receipt ([f863b9b](https://github.com/ImAbhishekTomar/kora-lang/commit/f863b9b394958018fc115e70cca4bfe84d88cc9f))
* **lang:** import other .ko files with `use "path" as name` ([3ffe40d](https://github.com/ImAbhishekTomar/kora-lang/commit/3ffe40d911a2860130cadf3e894cbba4cc07d0d2))
* **pkg:** add git dependencies, content hashing, and the lockfile ([bcb9522](https://github.com/ImAbhishekTomar/kora-lang/commit/bcb9522bea712da8d5ffc7bd1c73e3150577a39f))
* **pkg:** add the kora-pkg crate with manifest parsing ([2b3cccc](https://github.com/ImAbhishekTomar/kora-lang/commit/2b3cccce0f168c605392ab04617ecf176389637b))
* **pkg:** declare and resolve per-package capability grants ([7bb8a03](https://github.com/ImAbhishekTomar/kora-lang/commit/7bb8a038e7f17dc649b74f79cfc8041e7ebef048))
* **pkg:** fetch git dependencies and verify them against the lockfile ([35d89a3](https://github.com/ImAbhishekTomar/kora-lang/commit/35d89a32d5cc3651763ca4e929b4e79b259ac2cb))
* **pkg:** record what each commit contained, in an append-only log ([c22ea18](https://github.com/ImAbhishekTomar/kora-lang/commit/c22ea18107e5d0fa7e702a2779ab147539909790))
* **pkg:** resolve packages by reachability from runtime and test roots ([108891b](https://github.com/ImAbhishekTomar/kora-lang/commit/108891b32ac115bc4431e1a6a55168ce393e3465))
* **runtime:** a debugger hook with frames, breakpoints, and stepping ([ad77c9d](https://github.com/ImAbhishekTomar/kora-lang/commit/ad77c9de95a246ab2b0ac4e76a2899db9b9afc8d))
* **runtime:** enforce capability grants ([6192697](https://github.com/ImAbhishekTomar/kora-lang/commit/6192697d06e05982ff370b2deb1924bfdde8a086))
* **runtime:** give each package its own type namespace ([787115c](https://github.com/ImAbhishekTomar/kora-lang/commit/787115cfccf22d3a036a2266572b3986ca3e9b5b))
* **runtime:** load path dependencies ([1bdb76b](https://github.com/ImAbhishekTomar/kora-lang/commit/1bdb76b1b2719ff704a8c103bf6a9de56c0c6a9f))
* **syntax:** list the lines that carry a statement ([6f52d57](https://github.com/ImAbhishekTomar/kora-lang/commit/6f52d57a918522e1644112e2e8163d4be0cfc4a0))
* **syntax:** parse `use pkg <name> as <alias>` ([d8c2c9f](https://github.com/ImAbhishekTomar/kora-lang/commit/d8c2c9f08c88055a3c6c2205f560a247a251841a))
* **vscode:** debug .ko files with F5 ([b5ac040](https://github.com/ImAbhishekTomar/kora-lang/commit/b5ac040bf19835060d44bffe2b3442071356859a))


### Fixes

* **ci:** a template expression must not depend on an optional output ([7d50d62](https://github.com/ImAbhishekTomar/kora-lang/commit/7d50d621b1070f95e808b97f92f99348c0f5f093))
* **ci:** release notes must cover everything since the last release ([33b44bd](https://github.com/ImAbhishekTomar/kora-lang/commit/33b44bd212967dd68d11ef0e73c9218552a12c4a))
* **ci:** release-please cannot use the rust release type here ([99d0ba3](https://github.com/ImAbhishekTomar/kora-lang/commit/99d0ba3af0042c21e4f34ddc24a6c42e03bdff23))
* keep Windows package caches inside .kora ([481db97](https://github.com/ImAbhishekTomar/kora-lang/commit/481db977bfa1b71e7d78577b5cd3da32bbd2b8bf))
* make package fetches portable and update site dependencies ([a94c26f](https://github.com/ImAbhishekTomar/kora-lang/commit/a94c26fe01fb6a98cb7f216f5451825bcd462b3d))
* **pkg:** three bugs in dependency resolution ([9e5521b](https://github.com/ImAbhishekTomar/kora-lang/commit/9e5521bd5164b0ce306d4cc01158779e1db92228))
* use scoped npm package name ([278b624](https://github.com/ImAbhishekTomar/kora-lang/commit/278b6240836fbfa5b72e05c69deaca6e2dddd67c))
* **vscode:** activate on a debug request, not only on opening a .ko file ([94b6cda](https://github.com/ImAbhishekTomar/kora-lang/commit/94b6cda3ca7767eebf95ebcdb10d6bd6f2c70db8))


### Performance

* measure the interpreter instead of guessing about it ([95cee77](https://github.com/ImAbhishekTomar/kora-lang/commit/95cee77f79336f967b88334c0973e5766ca32f91))


### Documentation

* add AGENTS.md, the checklist a language change has to satisfy ([af584fb](https://github.com/ImAbhishekTomar/kora-lang/commit/af584fb6eea9e0e1eb801c2ab18c5ed857d6884a))
* add developer release notes pages ([31e2a78](https://github.com/ImAbhishekTomar/kora-lang/commit/31e2a78d829b81229ef0721b48cba45d48248f85))
* document debugging ([31e9419](https://github.com/ImAbhishekTomar/kora-lang/commit/31e941908431eec3e2da6e87be6aebd7c13a05f2))
* document file modules, with a runnable example ([4657031](https://github.com/ImAbhishekTomar/kora-lang/commit/4657031a4954e5d4af24f40a811fab01ea7c1fc8))
* document installation and release versions ([1e6c26f](https://github.com/ImAbhishekTomar/kora-lang/commit/1e6c26fef446c4983d3685863d110c5fca009f94))
* document package dependencies across public guides ([afadca0](https://github.com/ImAbhishekTomar/kora-lang/commit/afadca08bc6440ca020efb353eef08612a0a8eca))
* fix search and update feature status ([199865d](https://github.com/ImAbhishekTomar/kora-lang/commit/199865d98b03ad49cf93b1e6af4ea6dc0ec39bc4))
* include site source for deployment ([a93540b](https://github.com/ImAbhishekTomar/kora-lang/commit/a93540b8bd94abf2aad7d5d31dd628d08e2f07aa))
* move the site into site/ and check it like the rest of the docs ([c364d92](https://github.com/ImAbhishekTomar/kora-lang/commit/c364d9265dfe6058bc8e082cb1c7513aac065671))
* package capability grants ([41360ae](https://github.com/ImAbhishekTomar/kora-lang/commit/41360ae3a61c8e38880d244de6d7f77fddbff132))
* package dependencies ([76abc23](https://github.com/ImAbhishekTomar/kora-lang/commit/76abc234d36a1566d5ff9b0f27e0b30e44e0da98))
* record what is deferred, and what would start it ([d0fb6de](https://github.com/ImAbhishekTomar/kora-lang/commit/d0fb6de6ede6de446a50dae1288afcf5b3a5bf1c))
* the two things that stop a working debugger from working ([161b728](https://github.com/ImAbhishekTomar/kora-lang/commit/161b7285229b0c204bf2872c13d99544b05f7db5))
