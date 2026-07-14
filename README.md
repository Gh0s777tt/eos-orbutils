# eos-orbutils

**E-OS fork of [`redox-os/orbutils`](https://gitlab.redox-os.org/redox-os/orbutils).** Part of the [**E-OS**](https://github.com/Gh0s777tt/E-OS) ecosystem — a hardened, Crimson-branded downstream of [Redox OS](https://www.redox-os.org).

This repository is the **Orbital desktop utilities** (launcher, background, settings, greeter).

## E-OS changes vs upstream

- The E-OS **Crimson desktop environment** — a floating launcher bar with a diamond-E Start, a desktop icon grid, and an animated smoke/ember background.
- **`eos-settings`** — a native control panel (orbital/orbclient, no libcosmic) (R-D01).
- **`orblogin`** greeter enforces the first-boot password change (R-602c).

## How it's pinned

The E-OS build pins this fork in [`recipes/gui/orbutils/recipe.toml`](https://github.com/Gh0s777tt/E-OS/blob/main/recipes/gui/orbutils/recipe.toml):

- branch **`master`** · rev **`3ac6436a0e4a`**
- up to date with upstream

## Build standalone

This fork is normally built by the E-OS cookbook (`make CI=1 …` in the [main repo](https://github.com/Gh0s777tt/E-OS)). To build it on its own you need the Redox toolchain; see the main repo's [build guide](https://github.com/Gh0s777tt/E-OS/blob/main/docs/building.md).

## Hosting

**GitLab (source of truth):** https://gitlab.com/e-os/eos-orbutils  
**GitHub (read-only mirror):** https://github.com/Gh0s777tt/eos-orbutils

## License

MIT (inherited from upstream Redox). The E-OS project as a whole is AGPL-3.0; see the [main repo](https://github.com/Gh0s777tt/E-OS/blob/main/LICENSE).

---
[E-OS main repo](https://github.com/Gh0s777tt/E-OS) · [Docs](https://github.com/Gh0s777tt/E-OS/tree/main/docs) · [Upstream](https://gitlab.redox-os.org/redox-os/orbutils)
