# Publishing NEXIUM to npm

The npm package lives in [`npm/`](../npm) and ships a small Node launcher,
not the Rust binary itself. Prebuilt binaries are attached to GitHub Releases
and downloaded by the package's postinstall script.

## Name

The unscoped name `nexium` is already taken on npm, so the package is
published as **`nexiumx`** (npm names must be lowercase). The installed
CLI command is still `nexium`.

## Layout

```
npm/
├── package.json        name, bin, postinstall, files
├── bin/nexium.js       resolves and spawns the platform binary
├── scripts/install.js  best-effort release-asset downloader
├── README.md           npm-facing documentation
└── LICENSE             MIT
```

The launcher looks for the binary in this order:

1. `NEXIUM_BINARY_PATH`
2. `vendor/<process.platform>-<process.arch>/nexium` (installed by the
   postinstall script)
3. fails with build-from-source instructions

`NEXIUM_SKIP_DOWNLOAD=1` disables the postinstall download.

## Release flow

1. Bump the crate version in `Cargo.toml` and the npm version in
   `npm/package.json` (or let the workflow set it from the tag).
2. Tag and push:

   ```bash
   git tag v0.8.0
   git push origin v0.8.0
   ```

3. `.github/workflows/release.yml` builds release binaries for
   linux-x64, linux-arm64, darwin-x64, darwin-arm64 and win32-x64, uploads
   archives named `nexium-v<version>-<platform>-<arch>.tar.gz` to the GitHub
   release, then publishes `npm/` to npm (if configured).
4. Add the npm automation token as the repository secret `NPM_TOKEN`
   (Settings → Secrets and variables → Actions). Without it the npm job is
   skipped.

## Local validation

```bash
cd npm
npm pack --dry-run            # inspect package contents
NEXIUM_BINARY_PATH=../target/debug/nexium node bin/nexium.js --version
NEXIUM_BINARY_PATH=../target/debug/nexium node bin/nexium.js --mcp   # stdio MCP
```

## Publishing manually

```bash
cd npm
npm login
npm publish --access public
```

The release asset for the current platform must exist before users can install
without `NEXIUM_BINARY_PATH`; otherwise the launcher prints build instructions.
