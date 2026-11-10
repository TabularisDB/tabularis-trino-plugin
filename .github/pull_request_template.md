# Summary

<!-- What does this PR change and why? -->

Closes #<!-- issue number -->

## Type of change

- [ ] Fix
- [ ] Feature
- [ ] Docs
- [ ] CI
- [ ] Release

## Testing

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --locked --all-targets`
- [ ] `./scripts/e2e-fixture-test.sh` (when query, metadata, or protocol code changes)
- [ ] Relevant fixture checked manually (`fixture/https` for TLS/Basic, `fixture/oauth` for token/OAuth2)

## Checklist

- [ ] README / `MANUAL_TEST_PLAN.md` updated for user-visible changes
- [ ] No credentials, tokens, or private hostnames in code, fixtures, or screenshots

## Release note

Version bumps go in a separate `chore: prepare release vX.Y.Z` PR touching `Cargo.toml`, `Cargo.lock`, and `.tabularium`.
