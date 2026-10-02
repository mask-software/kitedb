# AGENTS.md - AI Coding Agent Guidelines

## Philosophy

This codebase will outlive you. Every shortcut becomes someone else's burden. Every hack compounds
into technical debt that slows the whole team down.

You are not just writing code. You are shaping the future of this project.The
patterns you establish will be copied. The corners you cut will be cut again.

Fight entropy. Leave the codebase better than you found it.

Mask owns this. Start: say hi + 1 motivating line. Work style: telegraph; noun-phrases ok; drop grammar; min tokens.

## Bugs

When I report a bug, don't start by trying to fix it. Instead, write a test that reproduces the bug and fails.
Then spawn a subagent that tries to fix the bug. If it fails, the test fails. If it succeeds, the test passes.

## Release Notes

Releases are manual for now; don't rely on CI to publish. CI can still publish, so know what triggers it:

- `ray-rs.yml` runs on pushes to main that touch `ray-rs/**` or the workflow. It greps EVERY line of the latest
  commit's message (subject and body) for `^(all|js|ts|py|rs|core): X.Y.Z`. Any matching line publishes. Never start
  a commit-message line with that pattern unless you mean to release.
- `js`/`ts` publish npm (`@kitedb/core`), `py` publishes PyPI, `rs`/`core` publish crates.io, and `all` publishes all three.
- npm: a line that is exactly `js: X.Y.Z` (nothing after) publishes to `latest`; extra text after the version
  publishes to `next`. PyPI and crates.io ignore that rule and always publish a stable release.
- The version in the message is not checked against `ray-rs/package.json`, `ray-rs/Cargo.toml` or
  `ray-rs/pyproject.toml`. Bump all of them first, or registries get mismatched versions.
- The workflow cancels in-progress runs on main, so a later push can cancel a release mid-publish. Push nothing to
  main until the release run finishes.
- `release.yml` (tag `v*`) is broken: it never reaches its publish step. Don't use it.
