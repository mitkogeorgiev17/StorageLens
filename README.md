<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="120" alt="" />

# StorageLens

### See what fills your Mac. Free up space safely.

StorageLens maps every file on your disk in seconds, shows you what is taking up
space, and helps you clear out what you don't need. Everything it removes goes to
the Trash first, so you can always undo.

<br />

<a href="#install-macos"><img alt="Download for macOS" src="https://img.shields.io/badge/Download_for-macOS-000000?style=for-the-badge&logo=apple&logoColor=white" height="44" /></a>

<br />

<a href="#install-windows"><img alt="Download for Windows" src="https://img.shields.io/badge/Download_for-Windows-0078D4?style=for-the-badge&logo=windows&logoColor=white" height="44" /></a>


<sub>Free &middot; Works offline &middot; macOS 14+ (Apple Silicon &amp; Intel) &middot; Windows 10+</sub>

</div>

<br />

## Install

### macOS

Open **Terminal** (press `Cmd+Space`, type *Terminal*, press `Return`), paste this
line, and press `Return`:

```bash
curl -fsSL https://raw.githubusercontent.com/mitkogeorgiev17/StorageLens/main/install.sh | bash
```

That's it. StorageLens is installed in your Applications folder and opens by itself.
**To update later, run the same line again.**

<details>
<summary><b>Prefer a normal download?</b></summary>

<br />

1. Download **[StorageLens.dmg](https://github.com/mitkogeorgiev17/StorageLens/releases/latest/download/StorageLens.dmg)**.
2. Open it and drag **StorageLens** onto **Applications**.
3. Open StorageLens. macOS says it can't verify the developer: click **Done**.
4. Open **System Settings -> Privacy & Security**, scroll down, and click
   **Open Anyway** next to StorageLens. You only do this once.

Why the extra step? Apple charges developers a yearly fee to remove it. StorageLens
is free, so it skips that. The install command above avoids the step entirely.

</details>

### Windows

1. Download **[StorageLens-setup.exe](https://github.com/mitkogeorgiev17/StorageLens/releases/latest/download/StorageLens-setup.exe)**.
2. Run the installer. Windows SmartScreen may warn that the publisher is unknown --
   click **More info -> Run anyway**. (StorageLens is not code-signed yet.)
3. StorageLens installs and opens. Grant it permission to scan your disks when asked.

To update later, download the new installer and run it again -- your settings and
scan data are kept.

## First launch

StorageLens asks for three permissions before it starts, and waits until they are on.
Click **Open settings** next to each one and switch StorageLens on:

| Permission | What it's for |
|---|---|
| **Full Disk Access** | Seeing every folder, including Mail, Messages and app data. Without it, the map has holes. |
| **App Management** | Moving apps you choose to uninstall to the Trash. |
| **Automation: Finder** | Removing App Store apps, which only Finder is allowed to move. |

You grant them once; updates keep them.

## What you can do

| | |
|---|---|
| 🗺️ **See your disk as a map** | Every folder is a box sized by how much space it takes. Click to zoom in. Group by file type, by app, by age or by size. |
| 📊 **Get the big picture** | How full your disk is, what kinds of files fill it, and where the biggest wins are. |
| 🧹 **Clean up safely** | Caches, leftovers from old downloads, developer clutter and more, each with a plain explanation and a risk level. Safe items are picked for you. |
| 🐳 **Tame Docker** | If you use Docker, see what's inside it and remove old images and build cache. |
| 🧬 **Find duplicates** | Identical files in different places. One copy of each always stays. |
| 📦 **Uninstall apps properly** | Every app with its real size, removed together with the files it leaves behind. |
| 🚀 **Control startup** | See what launches when you log in, and switch it off. |
| 🔎 **Find any file** | Search by name across your whole disk. |

## Your files stay safe

- **Cleanups go to the Trash first.** Nothing is deleted outright, and you can undo
  with one click. (Docker data is the one exception: Docker deletes it for good, so
  StorageLens asks twice.)
- **You see a list before anything moves.** No surprises.
- **System files are off-limits.** macOS folders and apps' own files are never
  suggested; your personal data is shown but never picked for you.
- **Nothing leaves your Mac.** StorageLens has no account, no tracking, no ads and no
  internet connection at all.

## How it works

1. **Scan.** StorageLens reads your disk's file list directly, much faster than
   opening every folder one by one. A full Mac usually takes 10–30 seconds.
2. **Understand.** It sorts what it found by type, app and age, and checks it against
   a list of things known to be safe to remove, like caches that apps rebuild on
   their own.
3. **Clean.** You choose what goes. It moves those items to the Trash, then rescans so
   the numbers stay true.

It rescans on its own when you open it and after every cleanup, so what you see is
always current.

## Questions

**Is it really free?** Yes. No trial, no subscription, no ads.

**Will it delete something I need?** It only suggests things from a list of known
safe categories, shows you everything before acting, and uses the Trash so you can
put things back.

**Why does my disk not get emptier right after a cleanup?** The files are in the
Trash, which is what makes undo possible. Empty the Trash when you're sure.

**How do I uninstall StorageLens?** Drag it from Applications to the Trash. To also
remove its data, delete `~/Library/Application Support/com.mitko.storagelens`.

<br />

---

## For developers

<details>
<summary><b>Build from source</b></summary>

<br />

Needs Node 22+ and Rust stable ([rustup](https://rustup.rs)).

```bash
npm install
npm run build:test    # target/release/bundle/macos/StorageLens TEST.app
npm run tauri dev     # run with live reload
```

Local builds are named **StorageLens TEST**, with their own identifier, data folder and
permissions, so they never mix with the installed app.

</details>

<details>
<summary><b>Releasing</b></summary>

<br />

Bump `version` in `src-tauri/tauri.conf.json`, merge, then tag:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

[`release.yml`](.github/workflows/release.yml) builds a universal `.dmg`, signs it with
the project's self-signed certificate (so macOS keeps the app's permissions across
updates), and publishes a GitHub Release. It runs only on version tags.

</details>

<details>
<summary><b>Under the hood</b></summary>

<br />

| Layer | Choice |
|---|---|
| App shell | Tauri 2 |
| Core | Rust: a native `getattrlistbulk` scanner, rules engine, and safety layer (path guards, Trash, undo) |
| Interface | React 19 + TypeScript + Vite, canvas treemap |
| Storage | SQLite, latest scan only |

[`PLAN.md`](./PLAN.md) has the full design; [`AGENTS.md`](./AGENTS.md) has how to work
in this repository.

</details>
