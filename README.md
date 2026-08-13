# Aizu

Durable desktop notifications for terminal AI agents, without a central
notification backend.

## Development setup

[mise](https://mise.jdx.dev/) is the source of truth for the Rust, Node.js, and
pnpm toolchain versions.

```bash
mise trust
mise install rust node pnpm
mise run check
```

Useful tasks:

```bash
mise tasks
mise run build
mise run cli:smoke
mise run ci
```

Run an individual command inside the pinned environment with:

```bash
mise exec -- cargo test --workspace --all-features --locked
```
