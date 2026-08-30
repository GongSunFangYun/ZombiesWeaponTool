<div align="center">

<img src="app.ico" alt="Logo" width="160" height="160">

# ZombiesWeaponTool - Hypixel Zombies Assistant Tool

[English](README.md) | [简体中文](README_ZHS.md)

[![GitHub release](https://img.shields.io/github/v/release/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Downloads](https://img.shields.io/github/downloads/GongSunFangYun/ZombiesWeaponTool/total?style=flat-square)]()
[![Stars](https://img.shields.io/github/stars/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Forks](https://img.shields.io/github/forks/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Issues](https://img.shields.io/github/issues/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![License](https://img.shields.io/github/license/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()

</div>

A terminal-based (TUI) global keyboard and mouse automation tool for first-person shooters, built with Hypixel Zombies and Black-Ops–style gameplay in mind. It listens for a global execution hotkey and, once activated, runs a configurable macro loop that switches weapons according to your bindings and autoclicks the right mouse button at a precise rhythm. Written in Rust with `ratatui`, `crossterm`, and `rdev` for global input capture and injection, it keeps working even when your game is in the foreground.

---

## Features

- **Three weapon‑slot bindings** — assign any key (number keys `2`/`3`/`4` by default) or mouse button to each slot.
- **Execution hotkey** (default **Left-Alt**) — start or stop the macro loop at any time.
- **Quick‑switch router hotkey** (default **Right-Alt**) — cycle through a list of configuration files instantly.
- **Execution order set** — choose which slots participate and in what order (`A`, `AB`, `ABC`, …). `A` means slot #1 only, `AB` toggles between #1 and #2, `ABC` cycles #1 → #2 → #3.
- **Execution mode** — *Hold* (runs while the hotkey is held) or *Toggle* (press once to start, press again to stop).
- **Random execution** — shuffles the selected slot sequence each round.
- **Independent timers** — weapon‑switch and shooting (right‑click) intervals are scheduled separately, each with its own configurable **± random jitter** in milliseconds.
- **Configuration router** — a lightweight `zwtcfg_router.yaml` file lists multiple JSON configs; the router hotkey loads them in order, skipping invalid ones.
- **Live config management** — export or load configs directly from the TUI. Every edit is saved immediately to the active file, and external changes to that file are picked up automatically in real time via a background file‑watcher thread.
- **Session persistence** — remembers the last used config, router table, router position, and UI language in `~/.zwt`, restored on next launch.
- **English / 中文 (Simplified Chinese) localization** — English by default; switch languages in‑app from the Operations row. Your preference is stored in the session file.

---

## System Requirements

- **Windows** — the tool uses Win32 APIs for high‑resolution timers and `rdev` for global input hooks.
- A terminal emulator that supports **raw mode / alternate screen** (Windows Terminal, Alacritty, WezTerm, …).
- To embed the icon and file metadata, `rc.exe` or `windres` must be on `PATH` at build time (optional — the build will warn if missing but will not fail).

---

## Building and Running

```bash
cargo build --release
./target/release/ZombiesWeaponTool.exe
```

For development: `cargo run`. Run tests with `cargo test`.

> On first launch, if no `zwtcfg.json` exists in the current directory, a default one is generated automatically.

---

## Language (i18n)

The interface defaults to **English**. To switch to **简体中文**, open the **Operations** row and confirm the `Switch Language` entry (it will show `语言切换` while the UI is in English). Switching back works the same way. Your choice is saved to the session file and restored on next launch.

You can also override the language via command line or environment variables (these take precedence over the session):

| Method | Example |
| --- | --- |
| CLI flag | `ZombiesWeaponTool.exe --lang zh` |
| CLI flag (equals) | `ZombiesWeaponTool.exe --lang=en` |
| Env var | `set ZWT_LANG=zh` (or `en`) |

`zh` and `en` are case‑insensitive and accept common variants (`zh-cn`, `zh_cn`, `en-us`, …).

---

## TUI Controls

| Key | Action |
| --- | --- |
| `Tab` / `←` `→` | Move focus horizontally |
| `↑` `↓` | Move focus vertically between rows |
| `Enter` | Edit / confirm / toggle the focused field |
| *(any key)* | When a **binding** field is focused, press any key to assign it |
| `Esc` | Cancel capture/edit, or quit the application |
| Execution hotkey (e.g. `Left-Alt`) | Start / stop the macro globally |
| Router hotkey (e.g. `Right-Alt`) | Quick‑switch to the next router config globally |

The status line shows whether the engine is **running** (`● Executing` / `○ Stopped`) and which config or router entry is active. Invalid router entries are displayed in red.

---

## Configuration

### `zwtcfg.json`

The main configuration file. Field names are the JSON keys (English, no units). Missing fields fall back to defaults, so older configs remain compatible.

| JSON key | Default | Description |
| --- | --- | --- |
| `weapon_slot_1` | `"2"` | Binding for weapon slot #1 |
| `weapon_slot_2` | `"3"` | Binding for weapon slot #2 |
| `weapon_slot_3` | `"4"` | Binding for weapon slot #3 |
| `execution_hotkey` | `"LALT"` | Hotkey to start / stop the macro |
| `router_hotkey` | `"RALT"` | Hotkey to quick‑switch router configs |
| `execution_order` | `"ABC"` | Which slots to use and their order (`A`/`B`/`C`, each at most once) |
| `execution_mode` | `"toggle"` | `hold` or `toggle` |
| `random_execution` | `false` | Shuffle the slot order each round |
| `switch_interval` | `100` | Weapon‑switch interval in milliseconds |
| `switch_interval_offset` | `20` | ± random jitter for the switch interval |
| `shoot_interval` | `50` | Shooting (right‑click) interval in milliseconds |
| `shoot_interval_offset` | `10` | ± random jitter for the shoot interval |

Example:

```json
{
  "weapon_slot_1": "2",
  "weapon_slot_2": "3",
  "weapon_slot_3": "4",
  "execution_hotkey": "LALT",
  "router_hotkey": "RALT",
  "execution_order": "ABC",
  "execution_mode": "toggle",
  "random_execution": false,
  "switch_interval": 100,
  "switch_interval_offset": 20,
  "shoot_interval": 50,
  "shoot_interval_offset": 10
}
```

> Intervals must be positive integers. Setting a main interval to `0` is rejected at load or edit time to prevent a busy‑loop. `execution_order` may only contain `A`, `B`, and `C`, each no more than once, with a length between 1 and 3.

### `zwtcfg_router.yaml` (quick‑switch router)

A fixed‑schema list of JSON config paths used for quick‑switching:

```yaml
config:
  - zwtcfg-2nd.json
  - zwtcfg-3rd.json
```

- Paths are resolved relative to the router file's directory; absolute paths work as well.
- Invalid entries (missing files or malformed JSON) are marked with a `# [invalid]` comment in the YAML and are skipped during quick‑switching.
- The active entry is highlighted in the status line.

### `~/.zwt` (session state)

Stored in your user profile directory. This JSON snapshot remembers the last used config, router table, router position, and UI language. If it is missing or corrupt, the app starts with the default config and English language.

---

## Binding Names

Bindings use human‑readable names (case‑insensitive):

- **Keys:** `0`–`9`, `A`–`Z`, `F1`–`F12`, `TAB`, `SPACE`, `ENTER`, `BACKSPACE`, `DEL`, `INSERT`, `HOME`, `END`, `PAGEUP`, `PAGEDOWN`, `ARROW_LEFT`, `ARROW_RIGHT`, `ARROW_UP`, `ARROW_DOWN`, `CAPSLOCK`, `SCROLLLOCK`, `NUMLOCK`, `PRINTSCREEN`, `PAUSE`, `MENU`, `PERIOD`, `COMMA`, `SLASH`, `BACKSLASH`, `MINUS`, `EQUALS`, `LBRACKET`, `RBRACKET`, `SEMICOLON`, `APOSTROPHE`, `GRAVE`.
- **Modifiers:** `LALT` (AltGr/`ALT`), `RALT` (AltGr), `CTRL`, `WIN` — can be combined with `+`, e.g., `CTRL+1`, `ALT+F5`.
- **Mouse buttons:** `MB1` (left), `MB2` (right), `MB3` (middle).

---

## How the Macro Loop Works

When the execution hotkey is triggered (*Hold* mode runs while held; *Toggle* mode starts/stops on each press), the engine launches a loop that independently schedules two things:

1. **Weapon switching** — every `switch_interval ± jitter` milliseconds, it taps the next slot's key, cycling through the slots defined by `execution_order` (shuffled if random execution is enabled).
2. **Shooting** — every `shoot_interval ± jitter` milliseconds, it taps `MB2`.

The engine reads the live shared configuration on each iteration, so hot‑switching configs, external reloads, and binding or interval changes take effect on the very next scheduled action. While running, the system timer resolution is raised to 1 ms for accuracy and restored afterwards.

When the TUI is in **capture/edit** mode, key presses are routed to the TUI for binding instead of driving the engine — so assigning the start key won't accidentally trigger the macro.

---

## Legal and Disclaimer

- **Provided AS-IS without warranty of any kind.**
- The author accepts **no liability** for any loss or damage caused by using this software.
- You are responsible for ensuring your use complies with the relevant game's terms of service and with applicable local laws.

© GongSunFangYun.