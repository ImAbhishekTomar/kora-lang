# Changelog

## [0.3.0](https://github.com/ImAbhishekTomar/kora-lang/compare/v0.2.0...v0.3.0) (2026-09-05)


### Features

* added docs internals ([a177d11](https://github.com/ImAbhishekTomar/kora-lang/commit/a177d1149d812b9ab39ed443daf27b0c0ad3307d))
* an effect is identified by which call it is, not which line ([#37](https://github.com/ImAbhishekTomar/kora-lang/issues/37)) ([5e867e9](https://github.com/ImAbhishekTomar/kora-lang/commit/5e867e99e56fb1681aa809c5f49c27534aab9fe0))
* **analyze:** document and demonstrate streamed answers ([fb1169a](https://github.com/ImAbhishekTomar/kora-lang/commit/fb1169addcd4dd1097aa9431dd6e97169fc61fa8))
* **analyze:** stream a str result token by token ([a355cbd](https://github.com/ImAbhishekTomar/kora-lang/commit/a355cbd0a5f048cb711a63f4e8ee4952284193ea))
* **analyze:** token-by-token streaming for str results ([7379455](https://github.com/ImAbhishekTomar/kora-lang/commit/73794555500a1aaa2dcebcbc97dec13681df0f4c))
* **examples:** the orchestrator-worker pattern uses list[Section] directly ([513949e](https://github.com/ImAbhishekTomar/kora-lang/commit/513949e85056c8fedefabe35b3d0ee8b3e114a28))
* **lang:** accept an agent as a tool target ([4f406bd](https://github.com/ImAbhishekTomar/kora-lang/commit/4f406bd747de08420db7be221f17a13599acc0f9))
* **lang:** budget: max_seconds — a scope that runs out of time ([#38](https://github.com/ImAbhishekTomar/kora-lang/issues/38)) ([543bbd2](https://github.com/ImAbhishekTomar/kora-lang/commit/543bbd2df2d6c8f51ddaacbf9c4048e1ac889cf7))
* **lang:** else (why, kind), or-patterns, and a stream shortcut ([ac14aef](https://github.com/ImAbhishekTomar/kora-lang/commit/ac14aef6cd261d88786e442a43027bd72a5b5eef))
* **lang:** open the tool loop with on tool_call(name, args) ([a2cd792](https://github.com/ImAbhishekTomar/kora-lang/commit/a2cd792f2432febbb400be8a6844de45d9abc654))
* **lang:** with context(...) — a lexical fence on request material ([a7fa938](https://github.com/ImAbhishekTomar/kora-lang/commit/a7fa938b545a1443e2009b0f778af669f5305e39))
* **runtime:** interactive input(), and notes — a durable run's own scratch space ([3ff689b](https://github.com/ImAbhishekTomar/kora-lang/commit/3ff689bad03bc0d4c35239dcd6350b01f78ed7ce))
* **runtime:** journal the with-context pruning decision per turn ([ba67a63](https://github.com/ImAbhishekTomar/kora-lang/commit/ba67a6384c663ee8f5b8e6252b9f1196569f4848))
* **testing:** mocks fall through to the one that matches the call ([7f5c24a](https://github.com/ImAbhishekTomar/kora-lang/commit/7f5c24a150d29f82338d7b3638b02c03e753a223))
* **vscode:** add Kora light and dark themes with low-noise syntax highlighting ([ba6d6f1](https://github.com/ImAbhishekTomar/kora-lang/commit/ba6d6f1a5a8dffed77ce16a23d30d9cc7ba5538b))


### Fixes

* **check:** catch Python-method-call and kwargs mistakes at check time ([fccbfbc](https://github.com/ImAbhishekTomar/kora-lang/commit/fccbfbc0a0095123b7fdadc14dc392ae726b88d8))
* **ci:** publish package crate before types ([34cca2c](https://github.com/ImAbhishekTomar/kora-lang/commit/34cca2c612733672802b89fa1bc52f89c59b5cbe))
* **models:** decode surrogate pairs in streamed answers ([3cbfc38](https://github.com/ImAbhishekTomar/kora-lang/commit/3cbfc3831fda1cc282d8cb7edf7608be1212140a))
* **models:** do not fail a stream over an SSE keep-alive ([3ddf8ee](https://github.com/ImAbhishekTomar/kora-lang/commit/3ddf8ee1665372ea2c682aad685a279d723a7810))
* **runtime:** report the declassified value, not its local alias ([0515d0b](https://github.com/ImAbhishekTomar/kora-lang/commit/0515d0be789d58370aa3c89f5c6f5e6fe5272432))
* **test:** stop pinning dict key order in the tool-call-hook test ([2215e6c](https://github.com/ImAbhishekTomar/kora-lang/commit/2215e6cc9b19d8293c5ab70da2ffe1cc020940f1))


### Documentation

* **agents:** add guidelines for using git worktree in feature development ([cc7018c](https://github.com/ImAbhishekTomar/kora-lang/commit/cc7018c76464402356c9df9882839660bf933b20))
* catch up README, TODO, and the editor to the last four features ([001d1e5](https://github.com/ImAbhishekTomar/kora-lang/commit/001d1e501a86c3837d518254f7fb512d4ac1cc68))
* **decisions:** design the next context-engineering phase ([c11c29a](https://github.com/ImAbhishekTomar/kora-lang/commit/c11c29a326f796da11305d8b4ef18d75e4bc5713))
* **decisions:** mark unimplemented phase-2 syntax as illustrative ([fc0e8ef](https://github.com/ImAbhishekTomar/kora-lang/commit/fc0e8ef44cdb235386ebeeb2cac35724d03d18ec))
* update AGENTS.md and TODO.md with feature development guidelines and capability roadmap ([600e1ea](https://github.com/ImAbhishekTomar/kora-lang/commit/600e1eaffb3b3cc1ac0e95ebd9f6e7cce72b2c13))

## [0.2.0](https://github.com/ImAbhishekTomar/kora-lang/compare/v0.1.0...v0.2.0) (2026-08-29)


### ⚠ BREAKING CHANGES

* **lang:** `analyze` can return `Failed(reason)`, so an existing three-arm `match` over a model call is no longer exhaustive. The unmatched-value error names the arm to add, and `else` collapses the whole thing to one line where the difference does not matter.

### Features

* **examples:** the agent and workflow patterns, and a mock that crosses a fan-out ([20fd6ac](https://github.com/ImAbhishekTomar/kora-lang/commit/20fd6ac0dd5f0dc481dbc701b0ceccd8b5b49743))
* **lang:** match guards, the `else` binding, and provider failure as an outcome ([a10aa4c](https://github.com/ImAbhishekTomar/kora-lang/commit/a10aa4c7e2922e617c8a7ce272c96aac5b8d805e))
* **lsp:** resolve `use pkg` for completion and go-to-definition ([a8e0108](https://github.com/ImAbhishekTomar/kora-lang/commit/a8e0108a5a3916263a5a09afd3d9b9776d0c1648))


### Fixes

* **ci:** give every workflow an explicit least-privilege token ([f59a852](https://github.com/ImAbhishekTomar/kora-lang/commit/f59a8523dd3640a9d7d7dc953ca0cb67deb9f827))
* **docs:** repair the documentation check after the site restructure ([d8ba9a8](https://github.com/ImAbhishekTomar/kora-lang/commit/d8ba9a81244e86ea1a8937632d85ee1179dfec53))
* **pkg:** a local repository path means the same on every host ([c2e272e](https://github.com/ImAbhishekTomar/kora-lang/commit/c2e272e27efe14f5260f7edb0bcc83a8af7a3175))
* **pkg:** agree on what a local repository path is ([5d62e97](https://github.com/ImAbhishekTomar/kora-lang/commit/5d62e975be9c0ee34fe6374fcafa1afcc6e9ebd5))

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
