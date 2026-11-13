# Gap 4: the user-intervention guard and XKB layout handling

Status: research only. Nothing here is implemented or authorized.
Date: 2026-11-13
Baseline: omp2 f4186b783b; upstream c308970263 (pinned checkout).

Evidence keys:

- **OBSERVED** means read in code or docs at the baseline. Citations use `path:line`. Upstream paths carry the `upstream:` prefix.
- **[INFERENCE]** means the author's reasoning. Every size estimate is an inference.
- **UNVERIFIED** means no run or source confirmed it.

No build, test or run was made for this document.

## Question

Upstream's `desktop` module stops the agent from acting on an app the user is using, and it types non-US text through the compositor's real keymap. What does that code do, and what does omp2's `omp-desktop` crate lack?

Out of scope: the accessibility tree code, capture, and the macOS `key_reroute` and `release_guard` machinery (they serve background key delivery, not intervention).

## Upstream behaviour

### Two separate mechanisms

Upstream has two different protections. They are easy to confuse.

1. **Physical-Escape emergency stop.** The user's Escape stops takeover input. It counts stops in `escape_stops` (`upstream:crates/pi-natives/src/desktop/control.rs:48`) and has one monitor type per OS (`upstream:crates/pi-natives/src/desktop/control.rs:509`). The Windows monitor is a `WH_KEYBOARD_LL` hook that watches `VK_ESCAPE` (`upstream:crates/pi-natives/src/desktop/control/windows.rs:1`). On Linux the monitor is skipped under Wayland (`upstream:crates/pi-natives/src/desktop/control/linux.rs:25`) and uses an X11 listener otherwise (`upstream:crates/pi-natives/src/desktop/control/linux.rs:30`).
2. **User-intervention guard.** It protects the agent's last read of an app from the user's own input. The module header states the rule: input aimed at an app the user has used since the session last read it waits for the user to pause, then fails without sending, so the agent reads again; input already being sent stops at the user's first key or click (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:1-7`).

### The guard's state machine (macOS)

- **Per-app record** `App`: process start time, a count of the user's inputs, and `unread`, the time of the last user input that no read has shown yet (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:28`). The start time tells a new process reusing a pid from the old one (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:81`).
- **Routes.** The user can type into a helper process that draws an app's Open or Save panel. A `Route` maps that helper's pid to the app, so the input counts for the app (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:40`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:101`). A reused helper pid takes none of the app's input (test at `upstream:crates/pi-natives/src/desktop/control/intervention.rs:514`).
- **Quiet period.** `QUIET` is 5 s (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:24`). A read clears `unread` only when the read began at least `QUIET` after the last user input (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:133`). A read made while the user is typing therefore does not count as having seen their changes.
- **Admission before sending.** `admit` loops: no unread input means the call proceeds and a watch starts. If the user last acted at least `QUIET` ago, it fails at once with `UserIntervened` ("the user was using X; nothing was sent"). If the user is still active, it waits until they pause, up to a limit just before the run deadline (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:147`). The error text always ends with "read the window again ... before acting on it" (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:25`).
- **Mid-call abort.** `InputWatch` keeps a thread-local baseline of the input count (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:187`). `check_press` runs before each key press, pointer press, accessibility action or write; if the count changed it fails, so at most the press already under way lands after the user's (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:232`). Call sites are many: for example `upstream:crates/pi-natives/src/desktop/macos/ax.rs:1219`, `upstream:crates/pi-natives/src/desktop/macos/ax/menus.rs:119` and `upstream:crates/pi-natives/src/desktop/macos/ax/open_menu.rs:356`.
- **Listener lifecycle.** `Listener` starts the tap lazily at the session's first read or input and stops it on drop. Without event-listening permission the tap does not start, nothing is recorded and no input waits (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:261`).
- **Tests.** Seven unit tests cover the pause rule, the deadline, routes, pid reuse and the wait ending before the native request times out (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:351`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:376`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:425`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:451`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:484`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:536`). The `Listener::simulated` and `record_for_test` hooks let them run without an OS tap (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:314`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:329`).

### How the macOS tap tells the user from the agent

- `UserInputMonitor` is a listen-only tap on the annotated session tap, with a mask of key-down and the three mouse-down types (`upstream:crates/pi-natives/src/desktop/control/macos.rs:663`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:34`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:430`). It listens during background input and between calls, not only during takeover.
- `observe_user` reads the event's source user data, source pid, target pid and location, then records the input against the target process (`upstream:crates/pi-natives/src/desktop/control/macos.rs:487`).
- An event is the user's if it lacks omp's synthetic tag and does not come from this process. That includes dictation tools that post under their own pid (`upstream:crates/pi-natives/src/desktop/control/macos.rs:413`). One known shape, an untagged left press at the origin from another process, is treated as another omp's make-key record and ignored (`upstream:crates/pi-natives/src/desktop/control/macos.rs:417`).
- A key with no named target goes to the front process; a click with no target is dropped (`upstream:crates/pi-natives/src/desktop/control/macos.rs:443`). A compile-time assertion fails the build if a mask type has no such rule (`upstream:crates/pi-natives/src/desktop/control/macos.rs:451`).

### X11

`linux/x11/activity.rs` is the X11 counterpart for takeover only. It reads XI2 raw presses by source device. omp's virtual MPX devices are never the user's. The core XTEST devices carry takeover's own presses and also tools such as x11vnc and xdotool, so a press beyond the ones this session sent is someone else's (`upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:1-9`). It raises `UserIntervened` in three places (`upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:131`, `upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:149`, `upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:220`). The product docs say a takeover first waits until the user has pressed nothing for 5 s, never past the run timeout, and refuses if the XI2.2 listener cannot start (`upstream:docs/computer-use.md:269`, `upstream:docs/computer-use.md:271`).

### Wayland and Windows

- Wayland: no intervention guard. The Escape monitor returns `None` there (`upstream:crates/pi-natives/src/desktop/control/linux.rs:25`). A search for `intervention` and `user_intervened` under `linux` and `win32` matched only three X11 files (`activity.rs`, `input.rs`, `named_change_tests.rs`).
- Windows: only the Escape hook (`upstream:crates/pi-natives/src/desktop/control/windows.rs:1`). No user-input guard.
- The documented contract is platform-scoped: `UserIntervened` is listed as macOS (`upstream:docs/computer-use.md:284`) and the physical-Escape text is at `upstream:docs/computer-use.md:265`.
- Error code: `UserIntervened` is one variant of `ErrorCode` with a `user_intervened` constructor (`upstream:crates/pi-natives/src/desktop/error.rs:10`, `upstream:crates/pi-natives/src/desktop/error.rs:84`).

### XKB layout handling (Wayland, libei)

- libei hands over the compositor's compiled textual keymap. `KeyboardLayout` parses `xkb_keycodes`, types and per-group symbol levels directly and never borrows a key from another group, because libei has no portable way to change the active group and a failed switch would type the wrong glyph (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:1-7`, `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:57`).
- The keymap fd shares its file offset with the compositor's copy, which may sit at EOF, so the bytes are read positionally from offset 0 (test: `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:585`).
- Behaviours with tests: numeric group labels (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:602`), French levels and AltGr (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:617`), only the active group resolves in a multi-layout keymap (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:627`), a modifier event that changes group changes resolution (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:638`), NumLock does not change non-keypad levels (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:659`) and CapsLock uses the alphabetic type map (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:668`).
- The libei backend reads the keymap from the device and falls back to no layout (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:860`). `char_stroke` resolves a character through the layout, with a US-ASCII fast path only when the group is US (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:876`). Two real keymaps (`fr.xkb`, `us-fr.xkb`) are checked in as test data (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:1125`).
- macOS reads the active input source per call (`upstream:docs/computer-use.md:159`). X11 has its own map in `linux/x11/keymap.rs` (651 lines; not read in depth for this document, so its group handling is UNVERIFIED).

## omp2 today

omp2 has the same crate shape (`omp-desktop`: session facade, actor thread, per-OS backends) but none of the guard.

- The crate describes itself as capture, input and accessibility automation for three OSes; there is no mention of user input or takeover (`crates/desktop/README.md:1-4`).
- No `takeover`, `UserIntervened`, physical-Escape, `UserInputMonitor` or event-tap code exists. A count of those terms across `crates/desktop/src`, `crates/tools/src/computer.rs` and `crates/envd/src/computer.rs` found only two unrelated lines in `crates/desktop/src/macos/input.rs` (`crates/desktop/src/macos/input.rs:958`, `crates/desktop/src/macos/input.rs:960`, both `set_integer_value_field`).
- Error codes: `ErrorCode` has no `UserIntervened` (`crates/desktop/src/error.rs:3`). It has `InvalidKey` (`crates/desktop/src/error.rs:23`) and `BackgroundUnavailable`. The tool layer's `FaultCode` mirrors those and has no intervention fault (`crates/tools/src/computer.rs:389`).
- Cancellation exists only as run cancel and deadline: `OperationState` holds `cancelled` and `deadline` (`crates/desktop/src/lib.rs:62`). Nothing in it knows about the user.
- Delivery has two modes, `Background` and `Foreground` (`crates/desktop/src/backend.rs:12`). omp2 has no takeover mode and no input-lease concept.
- macOS events are posted with a plain `CGEventSource` and no synthetic tag (`crates/desktop/src/macos/input.rs:572`, `crates/desktop/src/macos/input.rs:656`). The module sets the local-events suppression interval to 0 (`crates/desktop/src/macos/input.rs:236`). A listen-only tap could not tell omp's events from the user's without a tag. [INFERENCE]
- Wayland typing is US-only. `type_text` maps each char through `evdev_char` and fails for anything else with "libei cannot type character ... with the announced evdev keymap" (`crates/desktop/src/linux/wayland/libei.rs:453`, `crates/desktop/src/linux/wayland/libei.rs:460`). `evdev_char` is a fixed table of US-layout evdev codes (`crates/desktop/src/linux/wayland/libei.rs:572`). It does not read the compositor keymap, so on a French or Dvorak layout it types the wrong glyphs without error. [INFERENCE]
- X11 looks up a keycode by keysym in the first two columns of the server's keyboard mapping and errors when none matches (`crates/desktop/src/linux/x11/input.rs:494-510`). This respects the live layout but not other groups.
- Dependencies: `reis`, `x11rb` (with `xinput` and `xtest`) and `xkeysym` are already present (`crates/desktop/Cargo.toml:25-33`, `Cargo.toml:214`, `Cargo.toml:326`, `Cargo.toml:328`). The `wayland/xkb.rs` parser upstream is hand-written and needs no new crate.

## Gap table

| Capability | Upstream | omp2 | Gap |
|---|---|---|---|
| macOS user-input tap, per-app unread/quiet logic | yes (`intervention.rs`, `macos.rs`) | none | full port, medium |
| Synthetic-event tag on posted events | yes (`SYNTHETIC_EVENT_TAG`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:20`) | none | prerequisite for the tap |
| `UserIntervened` error code and tool fault | yes | none | small, but a public contract change |
| Mid-call abort (`check_press`) | yes, many call sites in `macos/ax*` | none | requires a hook at each press |
| X11 takeover guard (XI2 raw) | yes | none (no takeover mode) | blocked on takeover concept |
| Wayland guard | none | none | no gap |
| Windows guard | none (Escape hook only) | none | no gap |
| Physical-Escape emergency stop | macOS, X11, Windows | none | separate feature |
| XKB group-aware typing (libei) | yes, parsed keymap | fixed US table | functional gap on non-US layouts |
| X11 non-US typing | own keymap module | server mapping, 2 columns | partial |
| Keymap test data | `fr.xkb`, `us-fr.xkb` | none | add with the parser |

## Recommendation

Two independent slices. They share no code.

### Slice A: XKB typing on Wayland (do first)

This is a correctness bug, not a feature. On a non-US layout omp2 types the wrong characters.

1. Port `xkb.rs` into `crates/desktop/src/linux/wayland/` with its two `.xkb` test files. It is about 676 lines of std-only parsing. Upstream's file is the source of truth for behaviour; the licence is the same project's, so no new third-party licence applies. [INFERENCE]
2. Read the keymap from the libei keyboard device (upstream: `read_keymap`, `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:860`), keep it in the device struct, and replace `evdev_char` in `type_text` with a layout lookup that falls back to the US table only when the active group is US.
3. Return `InvalidKey` or `InputFailed` with the character and the active group when no stroke exists.
4. Tests: copy the upstream unit tests at `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:602-668` plus a French string test. They need no display. Estimate: 1 to 2 days. [INFERENCE]

### Slice B: macOS intervention guard (do second, behind a design review)

Prerequisites, in order: (1) add `ErrorCode::UserIntervened` and the matching `FaultCode` (a wire-visible change, so it needs the owner's agreement); (2) tag every event omp posts so the tap can ignore it; (3) port `intervention.rs` (550 lines) with its tests and `UserInputMonitor` (`control/macos.rs`, which reuses a tap-thread helper that omp2 may not have); (4) insert `check_press` before each press and AX action in `crates/desktop/src/macos/input.rs` and `crates/desktop/src/macos/ax.rs`; (5) update the computer tool prompt and docs so the agent knows to read again after `UserIntervened`.

Estimate: 4 to 7 days including permission handling. [INFERENCE] The tap needs the Input Monitoring permission; without it upstream silently sees nothing, which is a safe default but means the guard is off. The docs must say so.

### Not recommended now

- Windows and Wayland guards: upstream has none.
- X11 and the Escape monitors: they need a takeover mode and an input lease that omp2 does not have. Decide on takeover first; that is a larger product question than this gap.

## Risks and open questions

- **Privacy.** The tap sees every key and click system-wide. Upstream stores only pid and timestamp. Do the same; never log key codes. [INFERENCE]
- **False positives.** A user's dictation tool or remote desktop posts input under another pid and counts as the user. That matches upstream's intent (`upstream:crates/pi-natives/src/desktop/control/macos.rs:413`), but it will make the agent wait for them.
- **Wait cost.** `admit` can block up to the run deadline. omp2's `OperationState` has a deadline but no wait primitive (`crates/desktop/src/lib.rs:62`), so a cancellable wait must be added.
- **Tap permission and lifecycle.** The tap must stop on session drop. omp2's actor thread owns platform objects (`crates/desktop/README.md`), which fits a tap thread owned by the actor.
- **Contract.** Whether `UserIntervened` should exist on Linux at all is open; upstream scopes it to macOS and X11 takeover.
- UNVERIFIED: the X11 map's group handling upstream (`linux/x11/keymap.rs`), and whether omp2's X11 path behaves correctly on a non-US layout; neither was run.

## Upstream origin and license

A port would draw from `upstream:crates/pi-natives/src/desktop/control/intervention.rs`, `control/macos.rs`, `linux/wayland/xkb.rs`, `linux/wayland/libei.rs` and the two `testdata/*.xkb` files. The upstream `LICENSE` is MIT, with copyright lines for Mario Zechner, Can Bölük and Stencil Labs, Inc. (`upstream:LICENSE:1-5`). A port keeps that notice. The `.xkb` testdata are dumps of the xkeyboard-config layouts, which carry their own (permissive, MIT-style) terms; check `crates/desktop/NOTICE`-style handling and `THIRD-PARTY-NOTICES.txt` before copying them. UNVERIFIED: the licence text of the `.xkb` files was not read.

## Verdict

Implement slice A (Wayland XKB typing) as a bug fix. Defer slice B until the owner agrees to the new error code and the event tag. Research only for X11, Windows and Escape-stop, because they depend on a takeover mode omp2 does not have.
