# Releasing

`prensa` (library) and `prensa-cli` (the `prensa` command) share one version,
set once in the root `Cargo.toml`, and are released together.

## Versioning

prensa follows [SemVer](https://semver.org/). Below 1.0, the minor number is
the breaking one:

| Change since the last release | Next version |
|---|---|
| bug fixes only | patch: 0.1.0 → 0.1.1 |
| new features, or anything breaking | minor: 0.1.x → 0.2.0 |

**A PR that breaks the library API bumps the version in that same PR.** Cargo
honors version numbers even for git and path dependencies, so `main` must never
hold code that is incompatible with the version it declares. The `semver` CI
check (`cargo-semver-checks`, against the latest release on crates.io) fails a
breaking PR that does not bump. It only sees the library: a breaking change to
the CLI (a renamed flag, a changed default) follows the same rule by judgment.

Fixes and additions do not touch the version; it goes up in the release PR.

## 1. Release PR

On a branch `release/vX.Y.Z`, open a PR titled `chore: release vX.Y.Z` that:

- sets `version = "X.Y.Z"` in the root `Cargo.toml` (`[workspace.package]`)
- sets the same version on the `prensa` dependency in
  `crates/prensa-cli/Cargo.toml`
- updates `Cargo.lock` (any `cargo build` does it)
- moves the changes into a `## [X.Y.Z]` section of `CHANGELOG.md`, written from
  the titles of the PRs merged since the last tag, and updates the compare
  links at the bottom

Wait for CI and squash merge.

## 2. Tag

Tag the merge commit, which is now the tip of `main`:

```sh
git switch main && git pull --ff-only
git tag -s vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

Pull first: a tag on an older commit carries the old version, and the release
workflow rejects it.

## 3. Approve

Pushing the tag starts [`release.yml`](.github/workflows/release.yml). It waits
on the `release` environment: open the run in the Actions tab, then **Review
deployments** → approve. The workflow then:

1. checks that the tag is on `main` and matches the `prensa-cli` version
2. runs the tests
3. gets a short-lived crates.io token through trusted publishing (OIDC); no
   token is stored anywhere
4. runs `cargo publish --workspace` (`prensa`, then `prensa-cli`)
5. creates the GitHub Release `vX.Y.Z` with notes generated from the PRs
6. deploys the web page built from the tag to GitHub Pages
   ([`pages.yml`](.github/workflows/pages.yml))

Afterwards, check the new version on crates.io and `cargo install prensa-cli`,
and open the site. Edit the GitHub Release if it needs an introduction above
the generated notes.

## Fixing the site between releases

Only small fixes to the page (a typo, a broken style, a wrong link); new
features ship with a release. Merge the fix, then deploy by hand:

```sh
gh workflow run pages.yml -f ref=main
```

The wasm is built from the same ref as the page, so the workflow refuses a ref
whose `crates/`, `Cargo.toml` or `Cargo.lock` differ from the latest release:
the site would run unreleased code. If `main` already has such changes, make a
branch from the latest tag, apply only the page fix there, and deploy that
branch with `-f ref=<branch>`.

`gh workflow run pages.yml` without `ref` redeploys the latest release, and
`-f ref=vX.Y.Z` rolls back to an older one.

## When something goes wrong

| What happened | What to do |
|---|---|
| Failed before publishing (tag check, tests, authentication) | Nothing was published. Fix the cause, delete the tag (`git push --delete origin vX.Y.Z && git tag -d vX.Y.Z`), tag again and push |
| `prensa` published, `prensa-cli` failed | Re-run the job from the Actions tab. If it stops because `prensa` X.Y.Z already exists, publish `prensa-cli` alone |
| Published, but the `pages` job failed | The crates and the release are fine; only the site is behind. Fix the cause and run `gh workflow run pages.yml` (deploys the latest release) |
| A published version is broken | Published versions cannot be replaced or deleted. Release a fix (patch); if the version is harmful, yank it (crates.io → crate → Versions) |

Both crates only accept trusted publishing (`trustpub_only`), so a local
`cargo publish` with an API token is rejected. To publish by hand, temporarily
turn off **Require trusted publishing** in the crate settings, use a new
short-lived token, then turn it back on and revoke the token.

## One-time setup (reference)

- **crates.io**, for both `prensa` and `prensa-cli` → Settings → Trusted
  Publishing: owner `eoSalinas`, repository `prensa`, workflow `release.yml`,
  environment `release`; **Require trusted publishing** on.
- **GitHub environment `release`**: required reviewer (self-review allowed),
  administrators cannot bypass, deployments limited to tags matching `v*`, no
  secrets.
- **`main` ruleset**: pull request required, squash only, linear history, no
  force pushes or deletion, required checks `check`, `msrv`, `semver` and
  `conventional-title`.
