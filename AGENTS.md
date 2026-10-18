# mesh

- Read `SPEC.md` before any work. It is the source of truth. If a change conflicts with it, stop and ask. When the user agrees to a design change, update `SPEC.md` in the same change.
- Performance is rule #1 (see SPEC §1).
- Respect the module boundaries in SPEC "Architecture" (§4.5): only `registry/github.rs` runs `gh`, only `network/tailscale.rs` runs `tailscale`; backends return model types.
- NEVER write automated tests: no unit, integration, e2e, or any other tests, no test files, no test dependencies, no CI test steps. Verify manually on real devices.
- Iteration speed first. Smallest change that works.
- Rust stable. Dependencies: `serde` + `serde_json` only. Ask before adding any crate.

## Dev environment (adais, Windows)

- Repo lives on the Dev Drive: `D:\Hona\mesh`.
- Toolchain: `stable-x86_64-pc-windows-gnu` (no Visual Studio Build Tools installed). Release builds are MSVC, made in CI.
- The shell PATH may be stale. Absolute paths: cargo `%USERPROFILE%\.cargo\bin`, gh `C:\Program Files\GitHub CLI`, git `C:\Program Files\Git\cmd`, tailscale `C:\Program Files\Tailscale`. opencode: see SPEC §6.4 (desktop install, not on PATH).

## Devices for manual testing

| Mesh name | Tailscale name | OS | Notes |
|---|---|---|---|
| adais | adais | Windows | this machine; not joined yet |
| blade | lukeblade | Windows | SSH server running, key-only auth |
| air | lukes-macbook-air | macOS | often offline |
| — | — | Debian VPS | not on Tailscale yet |
