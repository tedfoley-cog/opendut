# Testing openDuT Application

## Overview
openDuT is a Rust workspace with a CARL backend and LEA frontend (WASM/Leptos). CARL serves LEA as embedded static files over HTTPS.

## Prerequisites
- Rust toolchain 1.91.1 with `wasm32-unknown-unknown` target (auto-installed via `rust-toolchain.toml`)
- System packages: `build-essential`, `pkg-config`, `libssl-dev`, `clang`
- `clang` is required for the `ring` crate when compiling LEA to WASM

## Building
```bash
# Build the CI orchestration tool first
cargo build -p opendut-ci

# Build the full workspace
cargo build --workspace
```

## Running Tests
```bash
# Run all tests (recommended)
cargo ci test --disable-logging

# Run clippy
cargo clippy --workspace --all-features
```
Note: Some tests related to NetBird VPN integration may be ignored if environment variables are not set. This is expected.

## Running the Application
```bash
# Start CARL backend + LEA frontend
cargo run -p opendut-carl -- service
```
- The app is served at `https://localhost:8080` (self-signed TLS cert)
- OIDC authentication is disabled in development mode
- VPN is disabled in development mode
- Dev config: `opendut-carl/carl-development.toml`
- Dev TLS certs: `resources/development/tls/`

## Important: The `cargo carl run` alias
The `cargo carl run` alias (defined in `.cargo/config.toml`) builds LEA via trunk first, then starts CARL. However, the actual CARL binary uses the `service` subcommand, not `run`. If running CARL directly:
```bash
cargo run -p opendut-carl -- service   # correct
cargo run -p opendut-carl -- run       # WRONG - 'run' is not a valid subcommand
```

## UI Testing Checklist
When testing the LEA frontend:
1. **Dashboard** (`/`): Shows "Welcome" heading, Clusters card (Deployed/Undeployed counts), Peers card (Online/Offline counts)
2. **Peers** (`/peers`): Table with Health/Name/Clusters/Action columns. Click "+" to create a new peer.
3. **Peer Configurator** (`/peers/<uuid>/configure/general`): Tabs for General, Network, Devices, Executor. Enter peer name to clear validation error.
4. **Clusters** (`/clusters`): Table with Deploy/Health/Name/Action columns. Click "+" to create a cluster.
5. **Downloads** (`/downloads`): CLEO and EDGAR download cards with architecture-specific download links.
6. **Navigation**: openDuT logo returns to dashboard. Nav buttons highlight active page.

## Browser Certificate Warning
The dev TLS certs are self-signed. When accessing `https://localhost:8080` in a browser, you'll need to bypass the certificate warning (click Advanced > Proceed to localhost).

## Devin Secrets Needed
No secrets are required for local development testing. OIDC and VPN are disabled in the development configuration.
