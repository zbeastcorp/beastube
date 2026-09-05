# Releasing

How a new version reaches people who already have BEASTUBE installed, and what has to be true for
it to work.

## What an update is

The application checks a release feed, downloads the **next installer**, verifies its signature, and
runs it without a wizard before restarting. To the person using it that is one button in
Settings → About.

It is not a patch. Windows offers no delta mechanism on this path, so every update pulls the whole
installer — roughly 50 MB, most of which is the bundled ffmpeg. That cost is stated in the settings
row before the button is pressed rather than discovered afterwards.

## The signing key

An update installs software without asking a second time, so the signature is what makes it safe.
The public half is compiled into every build (`plugins.updater.pubkey` in `tauri.conf.json`); the
private half signs each release.

The keypair for this project was generated to:

```
%USERPROFILE%\.beastube-keys\beastube-updater.key       <- private, never commit
%USERPROFILE%\.beastube-keys\beastube-updater.key.pub   <- public, already in tauri.conf.json
```

Three things follow, and none of them are optional:

- **Back the private key up somewhere you will still have in a year.** Lose it and you cannot sign
  another update. Everyone already running BEASTUBE stops receiving them permanently, and the only
  remedy is asking every user to reinstall by hand.
- **Never commit it.** `.gitignore` excludes `*.key`, but that is a safety net, not a plan.
- **Do not change `pubkey` between releases** unless you intend exactly that. An installed copy only
  accepts updates signed by the key it was built with.

To generate a fresh pair (only when starting over):

```powershell
pnpm tauri signer generate -w "$env:USERPROFILE\.beastube-keys\beastube-updater.key"
```

## Cutting a release: the short way

`.github/workflows/release.yml` does all of it. Once, before the first release, put the key into
the repository's secrets (Settings → Secrets and variables → Actions):

| Secret                               | Value                                                    |
| ------------------------------------ | -------------------------------------------------------- |
| `TAURI_SIGNING_PRIVATE_KEY`          | the **contents** of `beastube-updater.key`, not a path   |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password used when generating it, or an empty string |

Then, for each release:

```powershell
# 1. Bump the version in all three files so they agree, and commit.
# 2. Tag it and push the tag.
git tag v0.2.0
git push origin v0.2.0
```

The workflow builds on a Windows runner, fetches the bundled tools, signs, and opens a **draft**
release with the installer, its `.sig`, and `latest.json` attached. Read the notes, then press
publish — the updater endpoint resolves `releases/latest/`, which ignores drafts, so nothing is
offered to anyone until you do.

Two guards run before anything is built, because both failures are otherwise discovered late and
look like something else:

- **No signing key** stops the run. Without it the bundler still produces an installer, just no
  `.sig` — a release that looks complete and that every installed copy refuses.
- **A tag that disagrees with `package.json`** stops the run. The updater compares the version
  compiled into the binary, not the name of the tag, so `v0.2.0` built from a tree that still says
  `0.1.0` produces a release nobody is ever offered.

## Cutting a release: by hand

Still worth knowing, and the fallback when the workflow cannot run.

1. **Bump the version** in `src-tauri/tauri.conf.json`, `package.json` and the workspace
   `Cargo.toml`. They must agree — the updater compares the running version against the feed, so a
   mismatch either offers an update that is already installed or hides one that is not.

2. **Build, with the signing key in the environment.** The bundler signs the installer only when it
   can find the key:

   ```powershell
   $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content "$env:USERPROFILE\.beastube-keys\beastube-updater.key" -Raw
   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
   pnpm tools:fetch
   pnpm tauri build
   ```

   The key's **contents**, not its path. `TAURI_SIGNING_PRIVATE_KEY_PATH` is documented but was
   not picked up here: the build failed with "a public key has been found, but no private key"
   and produced an unsigned installer. Passing the contents works. The failure is the useful
   kind — `createUpdaterArtifacts` makes an unsigned release an error rather than something
   discovered later, when an update silently refuses to install.

   This produces the installer and, beside it, a `.sig` file:

   ```
   target/release/bundle/nsis/BEASTUBE_<version>_x64-setup.exe
   target/release/bundle/nsis/BEASTUBE_<version>_x64-setup.exe.sig
   ```

   No `.sig` means the key was not found and the release cannot be updated to. Check before
   publishing rather than after.

3. **Write `latest.json`** — the feed the application polls. `signature` is the _contents_ of the
   `.sig` file, not its path:

   ```json
   {
     "version": "0.2.0",
     "notes": "What changed, in a sentence someone would want to read.",
     "pub_date": "2026-09-04T00:00:00Z",
     "platforms": {
       "windows-x86_64": {
         "signature": "<contents of BEASTUBE_0.2.0_x64-setup.exe.sig>",
         "url": "https://github.com/BEASTUBE/beastube/releases/download/v0.2.0/BEASTUBE_0.2.0_x64-setup.exe"
       }
     }
   }
   ```

4. **Publish a GitHub release** tagged `v<version>`, attaching both the installer and
   `latest.json`.

The configured endpoint is:

```
https://github.com/BEASTUBE/beastube/releases/latest/download/latest.json
```

`releases/latest/download/` always resolves to the newest published release, so the endpoint never
needs changing. Update it in `tauri.conf.json` if the repository is renamed or moved — and remember
that already-installed copies keep polling the **old** URL until they have taken one update, so a
move needs a final release at the old address pointing people to the new one.

## Verifying an update actually works

Signature failures are silent by design — a bad update is refused rather than announced — so this is
worth doing once per release rather than trusting it:

1. Install the **previous** version from its installer.
2. Publish the new release.
3. Open Settings → About and press _Check for updates_.

It should report the new version, download it with a progress percentage, install without showing a
wizard, and restart into the new version. If it reports being up to date, the version numbers
disagree; if it fails, the signature or the URL in `latest.json` is wrong.

## Making updates smaller

Most of the 50 MB is ffmpeg, which changes rarely and is re-downloaded on every update regardless.
If update size becomes a problem, the option is to stop bundling the tools and fetch them into the
application's data directory on first run instead — the installer and every update then drop to
around 6 MB. That trades a smaller update for a first run that needs a network connection before
downloads work, which is the reason it was not done that way to begin with.
