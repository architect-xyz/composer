# Changelog

## [0.10.8](https://github.com/architect-xyz/composer/compare/v0.10.7...v0.10.8) (2026-03-22)


### Bug Fixes

* status falls back to composer CLI then docker compose ps ([147c837](https://github.com/architect-xyz/composer/commit/147c837839a417df1461759ec52ce8bb0f2dae2d))

## [0.10.7](https://github.com/architect-xyz/composer/compare/v0.10.6...v0.10.7) (2026-03-22)


### Features

* improve install defaults — use SUDO_USER, CWD, and confirm ([5a5fd23](https://github.com/architect-xyz/composer/commit/5a5fd23f4dcf25f340a7206d00dde0024d13f5cc))

## [0.10.6](https://github.com/architect-xyz/composer/compare/v0.10.5...v0.10.6) (2026-03-20)


### Features

* add `composer install uninstall` and update install.sh ([b05f606](https://github.com/architect-xyz/composer/commit/b05f606ab1b5cadf8e4f02dbb6e744bc107c557a))
* add install.sh for one-line binary installation ([e5c4b06](https://github.com/architect-xyz/composer/commit/e5c4b06e642b6b3fbe281835bbff15e0a6f49131))
* add top-level `composer uninstall` and `composer update` commands ([306b9b0](https://github.com/architect-xyz/composer/commit/306b9b04023d0c4d4bb6b7750cb49a3cc1b91035))

## [0.10.5](https://github.com/architect-xyz/composer/compare/v0.10.4...v0.10.5) (2026-03-20)


### Bug Fixes

* **ci:** fix musl OpenSSL and macos-13 runner issues ([50d46ce](https://github.com/architect-xyz/composer/commit/50d46ce4862d2b1f9cd79a846102845acb0ea582))

## [0.10.4](https://github.com/architect-xyz/composer/compare/v0.10.3...v0.10.4) (2026-03-20)


### Bug Fixes

* **ci:** use GitHub-hosted runners for binary builds ([f91de80](https://github.com/architect-xyz/composer/commit/f91de80f72cd279f8873a0b105ffb9a71145faa9))

## [0.10.3](https://github.com/architect-xyz/composer/compare/v0.10.2...v0.10.3) (2026-03-20)


### Bug Fixes

* **ci:** use larger Depot ARM runner, add fail-fast: false ([514226e](https://github.com/architect-xyz/composer/commit/514226e1e00321f50571f96a5e62251243cd129c))

## [0.10.2](https://github.com/architect-xyz/composer/compare/v0.10.1...v0.10.2) (2026-03-20)


### Features

* add `composer install status` command ([d3a064a](https://github.com/architect-xyz/composer/commit/d3a064a0e61de7b52ea43b58a187207b1e1e83fb))
* auto-detect compose file in current directory ([c186885](https://github.com/architect-xyz/composer/commit/c186885f6426b623e229bae851d2e670c7846967))
* native binary releases and systemd/launchd install subcommands ([c938eb1](https://github.com/architect-xyz/composer/commit/c938eb1cfe59bca6a964d61a8c8dd8bde8bad53a))
* native binary releases and systemd/launchd install subcommands ([d6ce77c](https://github.com/architect-xyz/composer/commit/d6ce77cebfe795f69bf460733f702d05987083ab))
* rewrite shell aliases as canonical version, add `composer install zsh` ([4ef862f](https://github.com/architect-xyz/composer/commit/4ef862f7d7d7476138788c4f61dcf407ed57d82d))


### Bug Fixes

* **ci:** chain build workflow from release-please, fix tag format ([4e2c7d8](https://github.com/architect-xyz/composer/commit/4e2c7d858d777d19757274aa282d04db66a2d0af))

## [0.10.1](https://github.com/architect-xyz/composer/compare/composer-v0.10.0...composer-v0.10.1) (2026-03-20)


### Features

* add `composer install status` command ([d3a064a](https://github.com/architect-xyz/composer/commit/d3a064a0e61de7b52ea43b58a187207b1e1e83fb))
* auto-detect compose file in current directory ([c186885](https://github.com/architect-xyz/composer/commit/c186885f6426b623e229bae851d2e670c7846967))
* native binary releases and systemd/launchd install subcommands ([c938eb1](https://github.com/architect-xyz/composer/commit/c938eb1cfe59bca6a964d61a8c8dd8bde8bad53a))
* native binary releases and systemd/launchd install subcommands ([d6ce77c](https://github.com/architect-xyz/composer/commit/d6ce77cebfe795f69bf460733f702d05987083ab))
* rewrite shell aliases as canonical version, add `composer install zsh` ([4ef862f](https://github.com/architect-xyz/composer/commit/4ef862f7d7d7476138788c4f61dcf407ed57d82d))
