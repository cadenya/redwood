# Contributing to Redwood

Thank you for helping improve Redwood.

## Development setup

Install Rust 1.85 or newer with the `rustfmt` and `clippy` components. Some
end-to-end tests also require Node.js 18+, Go, Python 3.9+, and Ruby.

```sh
cargo build --locked
cargo test --locked
```

Before opening a pull request, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

Changes to the intermediate representation should include lowering tests and
corresponding coverage for every affected backend. Changes to generated
runtimes should exercise the generated artifact, not only the Rust emitter.

## Pull requests

- Keep changes focused and explain their effect on generated public APIs.
- Use Conventional Commit titles, such as `feat:`, `fix:`, `docs:`, or `ci:`.
- Update documentation when behavior or configuration changes.
- Do not commit credentials, `.env` files, generated `gen/` trees, or local
  evidence under `tmp/`.

By contributing, you agree that your contributions are licensed under the
Apache License 2.0.
