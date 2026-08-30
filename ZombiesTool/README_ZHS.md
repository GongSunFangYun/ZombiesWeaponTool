<div align="center">

<img src="app.ico" alt="Logo" width="160" height="160">

# ZombiesWeaponTool

[English](README.md) | [简体中文](README_ZHS.md)

[![GitHub release](https://img.shields.io/github/v/release/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Downloads](https://img.shields.io/github/downloads/GongSunFangYun/ZombiesWeaponTool/total?style=flat-square)]()
[![Stars](https://img.shields.io/github/stars/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Forks](https://img.shields.io/github/forks/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![Issues](https://img.shields.io/github/issues/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()
[![License](https://img.shields.io/github/license/GongSunFangYun/ZombiesWeaponTool?style=flat-square)]()

</div>

ZombiesWeaponTool是一款基于终端（TUI）的全局键鼠自动化工具，主要面向Hypixel Zombies模式及类似玩法的第一人称射击游戏。它通过全局热键触发，激活后按预设配置循环执行宏操作：根据绑定按键自动切枪，同时以精准节奏模拟右键连点。工具基于 Rust 编写，使用 `ratatui`、`crossterm` 构建界面，`rdev` 负责全局输入监听与模拟注入，即使游戏处于前台也能正常工作。

---

## 功能特性

- **三个武器槽位绑定** —— 可为每个槽位分配任意按键（默认数字键 `2`/`3`/`4`）或鼠标按钮。
- **执行热键**（默认 **左 Alt**）—— 随时启动或停止宏循环。
- **配置速切热键**（默认 **右 Alt**）—— 在预设的配置文件列表中循环切换。
- **执行集合** —— 选择参与循环的武器槽及顺序（`A`、`AB`、`ABC`……）。`A` 表示仅使用槽位 #1，`AB` 在 #1 与 #2 之间交替，`ABC` 按 #1 → #2 → #3 顺序循环。
- **执行方式** —— *长按*（按住热键时运行，松开即停）或 *切换*（按一次启动，再按一次停止）。
- **乱序执行** —— 每轮开始时随机打乱所选槽位的顺序。
- **独立定时器** —— 切枪间隔与射击（右键）间隔分别调度，各自支持可配置的 **± 随机抖动**（毫秒）。
- **配置路由** —— 通过轻量的 `zwtcfg_router.yaml` 文件列出多个 JSON 配置；速切热键按顺序加载并自动跳过无效条目。
- **配置热管理** —— 在 TUI 界面中即可导出或读取配置。每次编辑都会立即保存到当前激活的配置文件，而外部对该文件的修改也会被后台线程实时检测并自动重载。
- **会话恢复** —— 在 `~/.zwt` 中记录上次使用的配置、路由表、路由位置及界面语言，下次启动时自动恢复。
- **中英文双语界面** —— 默认英文；可在「操作」行随时切换至简体中文，偏好设置会保存在会话文件中。

---

## 系统要求

- **Windows** —— 工具使用 Win32 API 实现高精度定时器，并通过 `rdev` 进行全局输入钩子。
- 支持**原始模式 / 备用屏幕**的终端模拟器（Windows Terminal、Alacritty、WezTerm 等）。
- 如需嵌入图标及文件元数据，编译时需要将 `rc.exe` 或 `windres` 加入 `PATH`（可选——缺失时仅给出警告，不会导致编译失败）。

---

## 编译与运行

```bash
cargo build --release
./target/release/ZombiesWeaponTool.exe
```

开发调试：`cargo run`。运行测试：`cargo test`。

> 首次启动时，若当前目录下不存在 `zwtcfg.json`，会自动生成一份默认配置。

---

## 界面语言

界面默认使用**英文**。如需切换到**简体中文**，在「操作」行找到并确认 `语言切换` 项即可（英文界面下显示为 `Switch Language`）。切回英文同样简单。语言偏好会存入会话文件，下次启动时自动沿用。

也可通过命令行或环境变量强制指定语言（优先级高于会话记录）：

| 方式 | 示例 |
| --- | --- |
| 命令行参数 | `ZombiesWeaponTool.exe --lang zh` |
| 命令行参数（等号形式） | `ZombiesWeaponTool.exe --lang=zh` |
| 环境变量 | `set ZWT_LANG=zh`（或 `en`） |

`zh` 与 `en` 不区分大小写，并支持常见变体（`zh-cn`、`zh_cn`、`en-us` 等）。

---

## TUI 操作说明

| 按键 | 作用 |
| --- | --- |
| `Tab` / `←` `→` | 横向移动焦点 |
| `↑` `↓` | 纵向移动焦点（行间切换） |
| `Enter` | 编辑 / 确认 / 切换当前字段 |
| *任意键* | 当焦点位于**绑定**字段时，按下任意键即完成绑定 |
| `Esc` | 取消捕获或编辑，或退出程序 |
| 执行热键（如 左 Alt） | 全局启动或停止宏 |
| 速切热键（如 右 Alt） | 全局切换到下一个路由配置 |

状态行会显示引擎当前是否在运行（`● 执行中` / `○ 已停止`），以及当前激活的配置文件或路由条目。无效的路由条目会以红色标示。

---

## 配置文件

### `zwtcfg.json`

主配置文件。字段名即为 JSON 键（英文，不带单位）。缺失字段会自动回退为默认值，因此旧版配置依然兼容。

| JSON 键 | 默认值 | 说明 |
| --- | --- | --- |
| `weapon_slot_1` | `"2"` | 武器槽位 #1 的按键绑定 |
| `weapon_slot_2` | `"3"` | 武器槽位 #2 的按键绑定 |
| `weapon_slot_3` | `"4"` | 武器槽位 #3 的按键绑定 |
| `execution_hotkey` | `"LALT"` | 启动 / 停止宏的执行热键 |
| `router_hotkey` | `"RALT"` | 速切配置文件的热键 |
| `execution_order` | `"ABC"` | 参与循环的槽位及顺序（`A`/`B`/`C`，每个最多出现一次） |
| `execution_mode` | `"toggle"` | `hold`（长按）或 `toggle`（切换） |
| `random_execution` | `false` | 每轮是否随机打乱槽位顺序 |
| `switch_interval` | `100` | 切枪间隔（毫秒） |
| `switch_interval_offset` | `20` | 切枪间隔的 ± 随机抖动（毫秒） |
| `shoot_interval` | `50` | 射击（右键）间隔（毫秒） |
| `shoot_interval_offset` | `10` | 射击间隔的 ± 随机抖动（毫秒） |

示例：

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

> 间隔值必须为正整数。主间隔设为 `0` 会在加载或编辑时被拒绝，以防止进入忙等状态。`execution_order` 只能包含 `A`、`B`、`C`，长度 1 至 3，且不可重复。

### `zwtcfg_router.yaml`（速切路由）

一份固定格式的 JSON 配置路径列表，供速切功能使用：

```yaml
config:
  - zwtcfg-2nd.json
  - zwtcfg-3rd.json
```

- 路径相对于路由文件所在目录解析，也支持绝对路径。
- 无效条目（文件不存在或 JSON 格式错误）会在 YAML 中被标注 `# [无效]` 注释，并在速切时自动跳过。
- 当前生效的条目会在状态行高亮显示。

### `~/.zwt`（会话状态）

存放于用户主目录。这是一个 JSON 快照，记录了上次使用的配置、路由表、路由位置及界面语言。若文件缺失或损坏，程序将以默认配置和英文界面启动。

---

## 绑定名称

绑定使用人类可读的名称（不区分大小写）：

- **按键：** `0`–`9`、`A`–`Z`、`F1`–`F12`、`TAB`、`SPACE`、`ENTER`、`BACKSPACE`、`DEL`、`INSERT`、`HOME`、`END`、`PAGEUP`、`PAGEDOWN`、`ARROW_LEFT`、`ARROW_RIGHT`、`ARROW_UP`、`ARROW_DOWN`、`CAPSLOCK`、`SCROLLLOCK`、`NUMLOCK`、`PRINTSCREEN`、`PAUSE`、`MENU`、`PERIOD`、`COMMA`、`SLASH`、`BACKSLASH`、`MINUS`、`EQUALS`、`LBRACKET`、`RBRACKET`、`SEMICOLON`、`APOSTROPHE`、`GRAVE`。
- **修饰键：** `LALT`（AltGr/`ALT`）、`RALT`（AltGr）、`CTRL`、`WIN` —— 可与 `+` 组合使用，例如 `CTRL+1`、`ALT+F5`。
- **鼠标按键：** `MB1`（左键）、`MB2`（右键）、`MB3`（中键）。

---

## 宏循环的工作方式

当执行热键被触发时（*长按*模式下按住即运行；*切换*模式下每按一次切换运行状态），引擎会启动一个循环，独立调度两项任务：

1. **切枪** —— 每隔 `切枪间隔 ± 抖动` 毫秒，按下当前槽位对应的按键，并按 `execution_order` 定义的顺序循环推进（若启用乱序则每轮重新打乱）。
2. **射击** —— 每隔 `射击间隔 ± 抖动` 毫秒，模拟按下 `MB2`（右键）。

引擎在每次循环迭代中都会读取共享配置的最新状态，因此速切配置、外部文件重载、绑定或间隔的修改都会在下一个调度周期立即生效。运行期间，系统定时器精度会被提升至 1 毫秒，结束后自动恢复。

当 TUI 处于**捕获或编辑**模式时，键盘输入会被导向界面用于绑定，而不会驱动宏引擎——这样在设置启动热键时就不会误触宏。

---

## 法律与免责声明

- **本软件按现状提供，不附带任何形式的担保。**
- 作者对使用本软件所造成的任何直接或间接损失**概不负责**。
- 使用者有责任确保自身使用行为符合相关游戏的服务条款及当地法律法规。

© GongSunFangYun。