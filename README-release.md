# WA2 release process

This project releases three things together for each version:

- one thin VS Code extension VSIX
- per-platform `wa2lsp` server archives
- per-platform `intent` CLI archives

The extension version pins the `wa2lsp` version exactly.

For release `X.Y.Z`:

- workspace Rust version must be `X.Y.Z`
- `client/wa2/package.json` version must be `X.Y.Z`
- git tag must be `vX.Y.Z`

---

## Release outputs

Each release should produce these assets:

- `wa2-X.Y.Z.vsix`

- `wa2lsp-linux-x64.tar.gz`
- `wa2lsp-darwin-x64.tar.gz`
- `wa2lsp-darwin-arm64.tar.gz`
- `wa2lsp-win32-x64.zip`

- `intent-linux-x64.tar.gz`
- `intent-darwin-x64.tar.gz`
- `intent-darwin-arm64.tar.gz`
- `intent-win32-x64.zip`

---

## Important release rule

The GitHub release must be **published**, not left as a draft.

The extension downloads `wa2lsp` from public GitHub release asset URLs like:

`https://github.com/unremarkable-technology/tooling/releases/download/vX.Y.Z/wa2lsp-linux-x64.tar.gz`

If the release is still draft, the extension will get a 404 and fail to install the language server.

---

## Version bump

### 1. Update Rust workspace version

Edit root `Cargo.toml`:

```toml
[workspace.package]
version = "X.Y.Z"
edition = "2024"
```

### 2. Update extension version

From client/wa2:

```bash
npm version patch --no-git-tag-version
```

Or set the exact version manually if needed.

This must match the workspace version exactly.

### 3. Regenerate lockfiles if needed

If versions changed and Cargo/npm updated lockfiles, keep those changes.

### Pre-release checks

Before tagging, confirm:

* root `Cargo.toml` workspace version is `X.Y.Z`
* `client/wa2/package.json` version is `X.Y.Z`

Useful check:

`grep -n "wa2lsp-.*\(tar.gz\|zip\)" client/wa2/src/extension.ts .github/workflows/build.yml`

### Commit and tag

From repo root:

```bash
git add .
git commit -m "release X.Y.Z"
git push origin main
git tag vX.Y.Z
git push origin vX.Y.Z
```
Pushing the tag triggers the GitHub Actions release workflow.

### CI behavior

The workflow validates that:
* git tag is `vX.Y.Z`
* workspace Rust version is `X.Y.Z`
* extension version is `X.Y.Z`

If they do not match, CI fails.

CI then builds:
* one thin VSIX
* four wa2lsp archives
* four intent archives

and creates a GitHub release draft.

### Publish the GitHub release

After CI completes:

1. Open the GitHub release page
1. Open the draft release for vX.Y.Z
1. Verify all 9 assets are attached
1. Publish the release

**Do not skip this step.**

The extension cannot download wa2lsp from a draft release.

### Verify release assets

Expected assets:

* `wa2-X.Y.Z.vsix`
* `wa2lsp-linux-x64.tar.gz`
* `wa2lsp-darwin-x64.tar.gz`
* `wa2lsp-darwin-arm64.tar.gz`
* `wa2lsp-win32-x64.zip`
* `intent-linux-x64.tar.gz`
* `intent-darwin-x64.tar.gz`
* `intent-darwin-arm64.tar.gz`
* `intent-win32-x64.zip`

Also verify the `wa2lsp` archives extract flat, with the binary at archive root:

Unix:
* `wa2lsp`

Windows:
* `wa2lsp.exe`

### Publish the extension to Marketplace

The Marketplace publish is still manual.

After the GitHub release is published:
1. Download or use the generated thin VSIX
1. Publish it manually to the VS Code Marketplace

The thin extension does not contain the LSP binary.
It downloads the matching wa2lsp release asset on first run.

### Post-release verification

Test the real production flow:

* Remove any local wa2.serverPath override
* Install/run the extension at version X.Y.Z
* Confirm logs show:
  * managed wa2lsp download
  * extraction
  * install into extension storage
  * successful LSP startup

Expected storage location will look like:
* Linux:
`~/.var/app/com.visualstudio.code/config/Code/User/globalStorage/figmentengineltd.wa2/wa2lsp/wa2lsp`

### Local development note

For local development, use workspace settings in the test workspace:

```json
{
  "wa2.serverPath": "/absolute/path/to/wa2lsp"
}
```

This bypasses managed download and uses a locally built server directly.

**Do not rely on PATH fallback.**

### If a release must be redone

If a tag was pushed incorrectly:

```bash
git tag -d vX.Y.Z
git push origin :refs/tags/vX.Y.Z
```

Then fix versions, recreate the tag, and push again.

If the GitHub release already exists, clean it up before reusing the same tag.

### Summary checklist

* bump root workspace version
* bump `client/wa2/package.json` to same version
* commit
* push `main`
* create and push tag `vX.Y.Z`
* wait for CI
* verify 9 assets
* publish GitHub release
* publish thin VSIX to Marketplace
* verify managed server download works