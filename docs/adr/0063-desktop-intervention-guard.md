# 0063. Desktop input: a user-intervention guard on macOS and layout-aware XKB typing on Wayland

Status: proposed
Date: 2026-11-13
Area: tools

Research: [docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md) (Gap 4 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- Two separate protections, easy to confuse. A physical-Escape emergency stop for takeover input, one monitor per OS (`upstream:crates/pi-natives/src/desktop/control.rs:48`, `upstream:crates/pi-natives/src/desktop/control.rs:509`). And a user-intervention guard: input aimed at an app the user has used since the session last read it waits for the user to pause, then fails without sending, so the agent reads again; input already being sent stops at the user's first key or click (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:1-7`).
- The guard is a per-app state machine: a count of the user's inputs and the time of the last input no read has shown (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:28`), a 5 s quiet period (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:24`), admission before sending (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:147`) and a mid-call check before each press or accessibility action (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:232`). Seven unit tests run it with a simulated listener (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:314`).
- On macOS a listen-only event tap records key-down and mouse-down events and tells the user from the agent by omp's synthetic event tag and the source pid (`upstream:crates/pi-natives/src/desktop/control/macos.rs:663`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:413`). Without event-listening permission the tap does not start and nothing waits (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:261`).
- On X11 a takeover-only counterpart reads XI2 raw presses (`upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:1-9`). Wayland and Windows have no intervention guard, only the Escape hook on Windows (`upstream:crates/pi-natives/src/desktop/control/windows.rs:1`).
- On Wayland, typing goes through a keymap parser: libei hands over the compositor's compiled keymap, the parser reads keycodes, types and per-group levels directly and never borrows a key from another group (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:1-7`). Tests cover French levels and AltGr, group changes, NumLock and CapsLock (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:602-668`), with two real keymaps as test data.

### What omp2 has

- `omp-desktop` has the same shape: a session facade, one actor thread, per-OS backends (`crates/desktop/README.md:1-4`). It has none of the guard: no takeover mode, no `UserIntervened`, no event tap, no synthetic event tag (`crates/desktop/src/error.rs:3`, `crates/desktop/src/macos/input.rs:572`).
- Cancellation is a deadline and a flag only (`crates/desktop/src/lib.rs:62`). Delivery has two modes, background and foreground (`crates/desktop/src/backend.rs:12`).
- Wayland typing is a fixed US table: `type_text` maps each character through `evdev_char` and fails on anything else (`crates/desktop/src/linux/wayland/libei.rs:453`, `crates/desktop/src/linux/wayland/libei.rs:572`). On a French or Dvorak layout it types the wrong characters without error. [INFERENCE]
- The dependencies a port needs, `reis`, `x11rb` and `xkeysym`, are already in the crate (`crates/desktop/Cargo.toml:25-33`).

### Rules that bind

- Additive only: `ErrorCode` and the tool layer's `FaultCode` are wire-visible (`crates/desktop/src/error.rs:3`, `crates/tools/src/computer.rs:389`). A new variant is additive but still needs the owner.
- ADR 0035: typed errors, no silent degradation. A guard that cannot start must say so, not pretend to guard.
- No `pi` identity in anything ported; convars are `sv_*`; features are `cap-*`.

## Decision

**D1. Two independent parts.** Part A is layout-aware typing on Wayland. Part B is the macOS intervention guard. They share no code and ship separately.

**D2. Part A: port the upstream XKB parser** into `crates/desktop/src/linux/wayland/` with its two keymap test files, read the keymap from the libei keyboard device, resolve each character through it, and keep the US table as a fast path only when the active group is US. A character with no stroke fails with `InputFailed` naming the character and the group. No new dependency.

**D3. Part B: port the guard on macOS only.** Add `ErrorCode::UserIntervened` and the matching tool fault, tag every event omp posts, add the listen-only tap and the per-app state machine, call the mid-call check before each press and accessibility action, give `OperationState` a cancellable wait. The guard is off unless `cap-desktop-guard` is compiled and `sv_desktop_intervention_guard` is set.

**D4. Not ported now:** the X11 guard and the Escape monitors, because omp2 has no takeover mode and no input lease for them to attach to; the Wayland and Windows guards, because upstream has none. Whether omp2 should grow a takeover mode is a separate decision.

**D5. Privacy.** The tap records a pid and a time only. It never logs or stores key codes or click positions.

**D6. Honest failure.** If the tap cannot start (no Input Monitoring permission), the guard reports itself inactive in the session's capability report. It does not claim protection it lacks.

## Rejected options

- **Extend `evdev_char` with more layouts.** Low cost for one layout, unbounded for the rest, still wrong for any layout it omits.
- **Link `libxkbcommon`.** A C dependency in a Rust-only workspace, with packaging cost on every platform.
- **A frontmost-app check using the AX focus code omp2 has.** About a day, but it catches an app switch and misses typing inside the target app, which is the case that matters.
- **Do nothing for Part B.** The agent already re-reads before acting, so the gap is a race, not a hole. Acceptable if the owner refuses the new error code; this record recommends the guard.
- **Port the X11 and Windows pieces now.** They need a takeover mode omp2 does not have.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**A1. Layout parser, dead code.** Port `xkb.rs` and the two `.xkb` files; nothing calls it.
- Tests: ported from `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:602-668`: `reads_the_keymap_from_the_start_of_a_shared_fd`, `resolves_numeric_group_labels`, `resolves_french_levels_and_altgr`, `resolves_only_the_active_group_in_a_multi_layout_keymap`, `modifier_event_group_change_updates_resolution`, `num_lock_does_not_change_non_keypad_levels`, `caps_lock_uses_the_alphabetic_type_map`. No display needed.

**A2. Use the layout in `type_text`.**
- Tests: `french_keymap_types_accented_letter_and_at_sign_with_dictated_modifiers`, `character_absent_from_every_level_fails_with_input_failed_naming_group`, `us_group_keeps_the_fast_path`, and the existing libei tests unchanged.
- Gate: default-off convar `sv_desktop_xkb_layout`; flip to default-on one release later (O1).

**B1. Event tag and error code.**
- Tests: `posted_events_carry_the_synthetic_tag_in_source_user_data`; `user_intervened_fault_round_trips_through_tool_serde`; an exhaustive-match test that every `ErrorCode` maps to a `FaultCode`.
- Gate: `cap-desktop-guard`, unused until B2.

**B2. The guard.** `intervention.rs`, the macOS `UserInputMonitor`, `check_press` calls in `crates/desktop/src/macos/input.rs` and `crates/desktop/src/macos/ax.rs`, a cancellable wait in `OperationState` (`crates/desktop/src/lib.rs:62`).
- Tests, headless with a simulated listener: `the_users_input_needs_a_read_after_they_pause_in_that_app_only`, `input_waits_for_the_user_to_pause_then_fails_until_a_read`, `the_runs_deadline_ends_the_wait_for_a_user_still_typing`, `input_into_a_process_drawing_the_apps_panel_counts_as_input_into_the_app`, `a_new_process_reusing_a_watched_id_starts_unwatched`, `the_wait_ends_before_the_native_request_times_out`, and `listener_off_never_blocks_input`.
- Gate: `cap-desktop-guard` and default-off `sv_desktop_intervention_guard`.

**B3. Prompt and documentation.** The computer tool's prompt says `UserIntervened` means read again; the docs name the Input Monitoring permission.
- Tests: the prompt snapshot changes only by the new paragraph.

## Upstream origin and license

A port draws from `upstream:crates/pi-natives/src/desktop/control/intervention.rs`, `control/macos.rs`, `linux/wayland/xkb.rs`, `linux/wayland/libei.rs` and the two `testdata/*.xkb` files. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). The port keeps that notice. The `.xkb` files are dumps of xkeyboard-config layouts with their own permissive terms; their text was not read (UNVERIFIED), so read it before copying them.

## Consequences

- Non-US Wayland users get the characters they typed. Before, they got wrong ones silently.
- macOS sessions gain a refusal path: an input call can fail with `UserIntervened` and the agent must read again. Tool prompts and docs change.
- Two enums gain a variant; consumers that match exhaustively need an arm.
- The tap needs the Input Monitoring permission. Without it the guard is inert, and says so.

## Open questions for the owner

- **O1.** Does a pure bug fix (Part A) need a default-off gate under the additive-only rule? Recommendation: yes for one release, then default-on; it changes what is typed, from wrong to right.
- **O2.** Accept a `UserIntervened` variant in `ErrorCode` and `FaultCode`? Recommendation: yes. Without it Part B cannot ship.
- **O3.** Should omp2 grow a takeover mode? Recommendation: not in this record; decide separately, then revisit the X11 guard and the Escape stops.
- **O4.** Should the 5 s quiet period be a convar? Recommendation: no; keep it a constant matching upstream until someone needs to change it.
- **O5.** Read the licence text of the `.xkb` fixtures before copying them. Recommendation: yes, and generate omp2's own fixtures if the terms are unclear.
- **O6.** Wait cost: `admit` can block up to the run deadline. Recommendation: cap the wait at the smaller of the deadline margin and 30 s, and report which.

## Status in omp

**Status: Not started.** Reusable today: the `reis`, `x11rb` and `xkeysym` dependencies, the actor thread, `OperationState`, the typed error enums.

## References

[docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md); ADR 0035 (error discipline); ADR 0006 (host policy and the sandbox stub, for the trust boundary around desktop control).

