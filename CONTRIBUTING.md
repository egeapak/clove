# Contributing to clove

Thank you for your interest in clove. This file covers the legal side of a
contribution. For how to build, test and track work, see `CLAUDE.md` and
`CLOVE.md`.

## Licenses

clove uses two licenses:

| Part | License |
|---|---|
| The plugin SDK: `crates/clove-types`, `crates/clove-plugin`, `crates/clove-plugin-echo` | MIT OR Apache-2.0 |
| Everything else | AGPL-3.0-only, with the plugin exception in `LICENSE-PLUGIN-EXCEPTION` |

A contribution is licensed under the license of the files it changes
("inbound = outbound"). You keep the copyright of your contribution.

The SDK crates must never depend on an AGPL crate. CI enforces this (the "SDK
license guard" step in `.github/workflows/ci.yml`).

## Developer Certificate of Origin (DCO)

Every commit must be signed off. A sign-off certifies the
[Developer Certificate of Origin 1.1](https://developercertificate.org/): you
wrote the change, or you have the right to submit it under the license of the
files it changes.

To sign off a commit, use `git commit -s`. Git adds a trailer with your name
and email:

```text
Signed-off-by: Your Name <you@example.com>
```

The email must match the commit's author email. CI checks every commit of a
pull request (`scripts/ci/check-dco.sh`).

To sign off commits that you already made on your branch, run:

```sh
git rebase --signoff <base-branch>
git push --force-with-lease
```
