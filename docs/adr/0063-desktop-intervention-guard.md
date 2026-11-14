# 0063. Desktop input: a user-intervention guard on macOS and layout-aware XKB typing on Wayland

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-14 (first draft 2026-11-13)
Area: tools

Research: [docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md) (Gap 4 of the upstream gap closure). This record states decisions; the note holds the evidence.

Review state: round 1 of the Elephant-Goldfish review returned five findings, and this revision resolves all five (see "What the review changed"). Rounds 2 and 3 are still to run, with fresh-context reviewers, and no slice below may be implemented before they pass.

Citation keys: omp2 paths are relative to the repository root at `omp2` 9cb6fd638. Upstream paths carry the `upstream:` prefix and refer to can1357/oh-my-pi at c308970263.

## Context

### What OMPv1 does

- There are two separate protections, and they are easy to confuse:
  - A physical-Escape emergency stop for takeover input, with one monitor per OS (`upstream:crates/pi-natives/src/desktop/control.rs:48`, `upstream:crates/pi-natives/src/desktop/control.rs:509`).
  - A user-intervention guard. Input aimed at an app the user has used since the session last read it waits for the user to pause, then fails without sending, so the agent reads again. Input already being sent stops at the user's first key or click (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:1-7`).
- The guard is a per-app state machine:
  - Per app, a count of the user's inputs and the time of the last input that no read has shown yet (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:28-36`).
  - A 5 s quiet period (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:24`).
  - Admission before sending (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:147-183`).
  - A mid-call check before each press or accessibility action (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:232-246`).
  - Seven unit tests run it with a simulated listener (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:351-575`).
- Upstream builds each refusal message with `format!`, putting the app name and the idle time into it (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:169-179`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:238-242`).
- The admission wait is cancellable. `Listener::admit` waits through `OperationToken::wait` (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:307-311`), which blocks on a `parking_lot::Condvar` that `cancel` and `retire` notify (`upstream:crates/pi-natives/src/desktop/control.rs:205-216`, `upstream:crates/pi-natives/src/desktop/control.rs:79-98`). The wait ends `DEADLINE_MARGIN` (250 ms) before the run's deadline or the caller's reply timeout, whichever is earlier (`upstream:crates/pi-natives/src/desktop/control.rs:41`, `upstream:crates/pi-natives/src/desktop/control.rs:166-172`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:154-157`).
- Held input is always released. `control::cleanup` runs release posts with cancellation checks off (`upstream:crates/pi-natives/src/desktop/control.rs:344-355`). Uses of it:
  - `hold_transitions` releases every held key, in reverse order, when a post, the body or the guard fails (`upstream:crates/pi-natives/src/desktop/macos/input.rs:3621-3657`).
  - `stroke_path` posts the drag's button-up at the last point it reached, whatever happened before (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2700-2735`).
  - `post_guarded` runs the guard only before a press; "releases always go out so nothing stays held" (`upstream:crates/pi-natives/src/desktop/macos/input.rs:1078-1089`).
- On macOS a listen-only event tap records key-down and mouse-down events. It tells the user's input from the agent's by omp's synthetic event tag and by the source pid (`upstream:crates/pi-natives/src/desktop/control/macos.rs:663`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:413`). Upstream's tag value is `0x7069_6465_736b`, which is ASCII `pidesk` (`upstream:crates/pi-natives/src/desktop/control/macos.rs:20`). If the session lacks event-listening permission, the tap does not start and nothing waits (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:257-281`).
- On X11 a counterpart that works during takeover only reads XI2 raw presses (`upstream:crates/pi-natives/src/desktop/linux/x11/activity.rs:1-9`). Wayland and Windows have no intervention guard. Windows has only the Escape hook (`upstream:crates/pi-natives/src/desktop/control/windows.rs:1`).
- On Wayland, typing goes through a keymap parser:
  - libei hands over the compositor's compiled keymap. The parser reads keycodes, types and per-group levels directly, and never borrows a key from another group (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:1-7`).
  - Eight tests cover it (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:584-676`). They read two checked-in keymaps (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:570-571`).
  - Those two files, `testdata/fr.xkb` (66,390 bytes) and `testdata/us-fr.xkb` (70,112 bytes), carry no copyright or licence header. The only match for "copyright" in them is the `copyright` keysym (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb:1472`).

### What omp2 has

- `omp-desktop` has the same overall shape: a session facade, one actor thread and per-OS backends (`crates/desktop/README.md:1-4`). It has no part of the guard: no takeover mode, no `UserIntervened`, no event tap and no synthetic event tag (`crates/desktop/src/error.rs:3-40`, `crates/desktop/src/macos/input.rs:572`).
- Cancellation:
  - `OperationState` holds only an `AtomicBool` and a deadline (`crates/desktop/src/lib.rs:62-75`). Nothing can wake a sleeping actor thread.
  - `CancelOnDrop` sets the flag when the async caller goes away (`crates/desktop/src/lib.rs:82-88`). `call` sets the flag after `recv_deadline` times out (`crates/desktop/src/lib.rs:763-781`).
  - Each operation's deadline is `OPERATION_TIMEOUT`, one minute (`crates/desktop/src/lib.rs:46`, `crates/desktop/src/lib.rs:1140-1150`).
  - The run's own timeout lives only in `omp-envd`: a default of 20 s and a maximum of 300 s, enforced by `tokio::time::timeout` (`crates/envd/src/computer.rs:183-197`). `omp-desktop` never sees it.
  - `close` cancels the active run token before it takes the run lock (`crates/envd/src/computer.rs:150-161`).
- Errors reach the model with fixed text only. `native_fault` maps each `ErrorCode` to a `FaultCode` and a static string, and drops `DesktopError::message` (`crates/envd/src/computer.rs:1724-1755`). Upstream's formatted refusal text would never reach the model.
- Held input is not released on an early exit:
  - `background_drag` posts button-down, then leaves the loop through `crate::operation_checkpoint()?` without posting button-up (`crates/desktop/src/macos/input.rs:497-529`, checkpoint at `crates/desktop/src/macos/input.rs:513`).
  - `global_pointer`'s drag arm has the same shape (`crates/desktop/src/macos/input.rs:902-905`).
  - `key_chord` presses keys with `?` and releases them only after every press succeeds (`crates/desktop/src/macos/input.rs:675-700`).
  - So today a cancel or deadline in the middle of a drag leaves the mouse button held.
- Delivery has two modes, background and foreground (`crates/desktop/src/backend.rs:12`).
- Wayland typing uses a fixed US table. `type_text` maps each character through `evdev_char` and fails on anything the table lacks (`crates/desktop/src/linux/wayland/libei.rs:453-466`, `crates/desktop/src/linux/wayland/libei.rs:572`). On a French or Dvorak layout it types the wrong characters and reports no error. [INFERENCE]
- The dependencies a port needs, `reis`, `x11rb` and `xkeysym`, are already in the crate (`crates/desktop/Cargo.toml:25-33`).
- Features and convars:
  - `omp-desktop`'s only feature is `wayland-pipewire` (`crates/desktop/Cargo.toml:11-13`).
  - `omp-envd` has `cap-computer = ["dep:omp-desktop", "omp-tools/cap-computer"]` (`crates/envd/Cargo.toml:18`).
  - `omp-app` turns on `cap-computer` by default and forwards it (`crates/app/Cargo.toml:24-37`).
  - The computer convars are `sv_computer_*`. They are declared in `crates/envd/src/computer.rs:32-68`, read once in `ComputerSettings::from_con` (`crates/envd/src/computer.rs:76-84`), and passed to the desktop crate through `DesktopSessionOptions` (`crates/envd/src/computer.rs:99-104`, `crates/desktop/src/types.rs:137-141`).
- The `computer@3` tool:
  - Its description is a fixed string in `spec()` (`crates/tools/src/computer.rs:490-502`).
  - Its capability payload `Capabilities` is a serde output struct (`crates/tools/src/computer.rs:338-363`).
  - `FaultCode` is a serde `snake_case` enum (`crates/tools/src/computer.rs:385-430`).

### Rules that bind

- Additive only. `ErrorCode` and the tool layer's `FaultCode` are visible on the wire (`crates/desktop/src/error.rs:3`, `crates/tools/src/computer.rs:389`). A new variant is additive, but it still needs the owner's agreement (O2). The description and the `Capabilities` output of `computer@3` stay as they are (O7, O9).
- ADR 0035 errors:
  - Static-message errors are unit variants with their text fixed at the definition site.
  - No formatted strings in library errors (`docs/adr/0035-rust-for-the-engine.md:55-57`, `AGENTS.md` "Composition/errors/state").
  - Failures are never silent: a guard that cannot start must say so.
- ADR 0049. A capability is a Cargo feature `cap-<name>`; T1 capabilities are on by default in `omp-app` only (`docs/adr/0049-tiered-integration-architecture.md:47`). A convar belonging to a capability compiles only with that capability (`docs/adr/0049-tiered-integration-architecture.md:207`, the `cap-browser` paragraph). `just check-minimal-caps` must stay green (`justfile:232-254`).
- Naming. No `pi` identity in anything ported, and that includes the synthetic event tag's value. Convars are named `sv_*` and capability features `cap-*`.

## Decision

**D1. Two independent parts.** Part A is layout-aware typing on Wayland. Part B is the macOS intervention guard. They share no code and ship separately.

**D2. Part A: port the upstream XKB parser** into `crates/desktop/src/linux/wayland/xkb.rs`.
- Read the keymap from the libei keyboard device and resolve each character through it.
- Keep the US table as a fast path, used only when the active group is US.
- A character that no level of the active group can produce fails with `ErrorCode::InputFailed`.
- No new dependency.
- Gate: the convar `sv_computer_xkb_layout` (bool, default `false`, flags `archive`). It is declared beside the other `sv_computer_*` convars in `crates/envd/src/computer.rs` under `cap-computer`, read in `ComputerSettings::from_con`, and passed as the new field `DesktopSessionOptions::xkb_layout: bool`. When it is off, `type_text` behaves exactly as it does today.

**D3. Part A fixtures are generated, not copied.**
- omp2 generates its own two keymaps (`fr`, and the two-group `us,fr`) with libxkbcommon's `xkbcli compile-keymap --rules evdev --model pc105 --layout fr` (and `--layout us,fr`). This produces the same serialisation a compositor hands to libei.
- The command, the libxkbcommon version and the xkeyboard-config version go in a header comment at the top of each file.
- The keymaps are data derived from xkeyboard-config. Its terms are a set of X11/MIT-style permission notices that require the copyright and permission notice in all copies. They were read from the Debian `xkb-data` 2.41 `copyright` file; it has twelve copyright lines and no copyleft clause (`/usr/share/doc/xkb-data/copyright` on the build host).
- The slice adds `crates/desktop/NOTICE` in the shape of `crates/tui/NOTICE`. It lists the fixtures, xkeyboard-config's notices and the upstream MIT notice for the ported parser (`upstream:LICENSE:1-5`).
- Fallback: if the ported tests fail on the regenerated files, the upstream fixtures may be copied instead, under the same NOTICE, and the failure is recorded as a parser finding.

**D4. Part B: the macOS guard, behind a named capability and a convar.**
- Feature chain, every link default-off:
  - `omp-desktop` feature `intervention-guard`. This is crate-internal and not a `cap-*` name, matching `wayland-pipewire` (`crates/desktop/Cargo.toml:11-13`).
  - `omp-envd` feature `cap-desktop-guard = ["cap-computer", "omp-desktop/intervention-guard"]`.
  - `omp-app` feature `cap-desktop-guard = ["omp-envd/cap-desktop-guard"]`. It is **not** added to `omp-app`'s `default` list (`crates/app/Cargo.toml:25-33`).
- Convar `sv_computer_intervention_guard` (bool, default `false`, flags `archive`, meta `ui.tab = "tools"`, `ui.group = "Computer"`). It is declared in `crates/envd/src/computer.rs` under `#[cfg(feature = "cap-desktop-guard")]`, read in `ComputerSettings::from_con`, and passed as `DesktopSessionOptions::intervention_guard: bool`. That field exists only with `intervention-guard`.
- The guard runs only when the feature is compiled **and** the convar is `true` **and** the backend is macOS. Every other combination leaves input exactly as it is today.

**D5. `UserIntervened` is a typed code with fixed text (ADR 0035).**
- Add `ErrorCode::UserIntervened` (`crates/desktop/src/error.rs`) and `FaultCode::UserIntervened` (`crates/tools/src/computer.rs`; serde `user_intervened`). Both variants compile unconditionally; only the guard produces them.
- Constructor: `DesktopError::user_intervened()` takes no arguments and uses one `&'static str`. It never formats an app name, a pid or an idle time into the message.
- The new arm in `native_fault` (`crates/envd/src/computer.rs:1724-1755`) maps it to `FaultCode::UserIntervened` with this fixed text: "the user pressed a key or clicked in the target app since it was last read; no further input was sent, and input sent earlier in this call may have landed; read the window again before acting on it".
- One text covers both of upstream's cases. A refusal at admission sent nothing; a refusal mid-call may follow input that landed. `native_fault` cannot tell the two apart, and the text is true for both.

**D6. Synthetic event tag.**
- Every event omp posts on macOS carries an omp-owned constant in the source user data: `OMP_EVENT_TAG = 0x6f6d_7064_736b` (ASCII `ompdsk`). This applies to the posts at `crates/desktop/src/macos/input.rs:572` and `crates/desktop/src/macos/input.rs:656`, and to every other `post_*` helper.
- Upstream's `pidesk` value is not ported. The tag is set only when `intervention-guard` is compiled.

**D7. The admission wait is cancellable on the actor thread.**
- `OperationState` gains three things under `intervention-guard`:
  - a `parking_lot::Mutex<()>` and a `parking_lot::Condvar`;
  - `cancel(&self)`, which stores the flag and then calls `notify_all`;
  - `wait(&self, duration) -> CoreResult<()>`, which blocks on the condvar and returns the existing `check()` error as soon as the operation is cancelled or past its deadline.
- `CancelOnDrop::drop` (`crates/desktop/src/lib.rs:82-88`) and the timeout path in `call` (`crates/desktop/src/lib.rs:776-777`) call `cancel()` instead of storing the flag directly.
- The wait never uses `thread::sleep` and never polls on a timer.
- Run deadline: add the method `DesktopSession::set_run_deadline(&self, Option<Instant>)`, under the feature. `omp-envd` calls it at the start of each run with the instant its `tokio::time::timeout` will fire (`crates/envd/src/computer.rs:183-197`).
- The wait limit is `min(operation deadline, run deadline) − DEADLINE_MARGIN`, with `DEADLINE_MARGIN = 250 ms` as upstream has it. A user who is still typing at the limit therefore gets `UserIntervened` before the caller times out, not a `Timeout`.
- Closing the session ends a wait. `close` already cancels the active run token (`crates/envd/src/computer.rs:150-153`). Dropping the run future fires `CancelOnDrop`, and that now wakes the condvar.
- The wait holds the actor thread. This is acceptable because runs are already serialised by `run_lock` (`crates/envd/src/computer.rs:94`).

**D8. Release on abort.**
- With the guard active, every macOS input routine that presses something owns its release. Those routines are `background_click`, `background_drag`, `global_pointer` (click and drag arms) and `key_chord` (`crates/desktop/src/macos/input.rs:362-415`, `crates/desktop/src/macos/input.rs:478-551`, `crates/desktop/src/macos/input.rs:838-940`, `crates/desktop/src/macos/input.rs:675-700`).
- On any early exit after a press, the routine posts the release for everything still held: the button-up at the last point the pointer reached, and held keys in reverse order. An early exit is a guard refusal, a cancel, a deadline or a failed post.
- Release posts skip both the guard check and `operation_checkpoint`, as upstream's `control::cleanup` does.
- If the release itself fails, the routine returns the original error. The release failure is logged with `tracing::warn!` and no key codes.
- With the guard off, these routines behave exactly as they do today. Making the release unconditional fixes an existing bug, but it is a change to existing behaviour, so it is an owner question (O8).

**D9. Where the guard checks.**
- `admit(pid)` runs before the first press of an input request. `check_press()` runs before each press and accessibility action, the same call sites upstream uses (`upstream:crates/pi-natives/src/desktop/macos/input.rs:968`, `upstream:crates/pi-natives/src/desktop/macos/input.rs:1072-1077`, `upstream:crates/pi-natives/src/desktop/macos/ax.rs:1219`).
- omp2 also runs `check_press()` at each existing drag-step checkpoint (`crates/desktop/src/macos/input.rs:513`, `crates/desktop/src/macos/input.rs:904`), so a user's click in the middle of a drag stops the stroke and D8 releases it.
- This is stricter than upstream, whose `stroke_path` checks only for cancellation between points (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2716-2723`). omp2 accepts the difference because the checkpoint already exists.
- Reads clear an app's unread input by upstream's rule (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:133-141`). The reads that count are a capture of one of the app's windows and an accessibility snapshot.

**D10. Not ported now.**
- The X11 guard and the Escape monitors. omp2 has no takeover mode and no input lease for them to attach to.
- The Wayland and Windows guards, because upstream has none.

**D11. Privacy.** The tap records a pid and a time only. It never logs or stores key codes or click positions.

**D12. Honest failure when the tap cannot start.**
- If the user turned the guard on (feature compiled and convar `true`) and the tap cannot start, input calls fail with `ErrorCode::PermissionDenied`. That maps to the existing fixed fault "required desktop permission is unavailable" (`crates/envd/src/computer.rs:1726-1728`).
- The cause is usually a missing Input Monitoring permission. A one-time `tracing::warn!` names that permission.
- This differs from upstream, where input simply goes ahead unguarded (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:257-260`). Going ahead unguarded would be silent degradation for a user who asked for protection.
- Reporting the guard's state in the `capabilities` action would change `computer@3`'s output shape, so it is O7.

## Rejected options

- **Extend `evdev_char` with more layouts.** Cheap for one layout and unbounded for the rest, and still wrong for any layout it leaves out.
- **Link `libxkbcommon`.** A C dependency in a Rust-only workspace, with packaging cost on every platform. Using `xkbcli` once to generate test fixtures adds no build dependency.
- **Copy upstream's `.xkb` fixtures as they are.** They carry no notice (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb:1`) and no record of how they were made. Generated fixtures with a recorded command and a NOTICE are traceable. Copying stays the fallback in D3.
- **Carry the app name in the `UserIntervened` text.** `native_fault` drops the native message (`crates/envd/src/computer.rs:1724-1755`). Carrying the name would need a field on `Fault`, which is a `computer@3` output change. The model already knows which window it targeted.
- **Gate with a convar only, with no feature.** That would compile the tap and the tag into every macOS build. ADR 0049 keeps optional capability code out of builds that do not ask for it.
- **Let input go ahead unguarded when the tap cannot start (upstream).** This is silent degradation for a user who turned the guard on (ADR 0035).
- **A frontmost-app check using the AX focus code omp2 already has.** About a day of work, but it catches only an app switch and misses typing inside the target app, which is the case that matters.
- **Do nothing for Part B.** The agent already re-reads before it acts, so the gap is a race, not a hole. This is acceptable if the owner refuses the new error code.
- **Port the X11 and Windows pieces now.** They need a takeover mode omp2 does not have.

## Slices

Each slice is one PR. Tests are written first and are not edited to pass.

Proof levels:
- No slice has an e2e proof in `crates/e2e`. The e2e harness has no desktop (`crates/e2e/tests` holds no computer test), and no CI runner grants Input Monitoring or a Wayland RemoteDesktop portal.
- No slice adds a golden-journal test. `Fault` and its serde shape already exist, and the journal shape does not change.
- The owning seams are `omp-desktop` unit tests, which are headless and use pure functions or simulated listeners, and `omp-envd` tests.
- macOS-only tests run in the macOS workspace job. They create `CGEvent`s and need no permission.

**A1. Layout parser and fixtures, as dead code.**
- Scope: port `xkb.rs`; generate `testdata/fr.xkb` and `testdata/us-fr.xkb` (D3); add `crates/desktop/NOTICE`. Nothing calls the parser yet.
- Acceptance: the eight upstream tests, ported under omp names, pass with no display: `reads_the_keymap_from_the_start_of_a_shared_fd`, `resolves_numeric_group_labels`, `resolves_french_levels_and_altgr`, `resolves_only_the_active_group_in_a_multi_layout_keymap`, `modifier_event_group_change_updates_resolution`, `serialized_modifier_fields_affect_resolution`, `num_lock_does_not_change_non_keypad_levels`, `caps_lock_uses_the_alphabetic_type_map` (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:584-676`). Each fixture's header names the command it was generated with.
- Gate: none. It is unreachable code.

**A2. Use the layout in `type_text`.**
- Scope: read the keymap from the libei device (as upstream's `read_keymap` does, `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:860`); a pure `char_stroke(layout, char)` (as at `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:876`); the convar `sv_computer_xkb_layout` and the field `DesktopSessionOptions::xkb_layout`.
- Acceptance: `french_keymap_types_accented_letter_and_at_sign_with_dictated_modifiers`, `character_absent_from_the_active_group_fails_with_input_failed`, `us_group_keeps_the_fast_path`, and `layout_off_keeps_the_us_table`, all unit tests on `char_stroke` against the A1 fixtures. The existing libei test `discovery_waits_for_every_granted_device` still passes.
- Gate: `sv_computer_xkb_layout`, default `false` (O1).

**B1. Plumbing, the error code and the tag.**
- Scope: the D4 feature chain and convar; `ErrorCode::UserIntervened`, `FaultCode::UserIntervened` and the `native_fault` arm (D5); `OMP_EVENT_TAG` on every post (D6).
- Acceptance:
  - `user_intervened_reaches_the_model_with_fixed_text_only` (envd). A `DesktopError` whose message holds an app name maps to `FaultCode::UserIntervened` and the D5 text; the name is absent.
  - `user_intervened_fault_serializes_as_user_intervened` (tools; wire bytes).
  - `posted_events_carry_the_omp_event_tag` (desktop, macOS).
  - `just check-minimal-caps` passes, and `cargo check -p omp-app --features cap-desktop-guard` passes.
- Gate: `cap-desktop-guard`. Nothing produces the code before B4.

**B2. A cancellable wait and the run deadline.**
- Scope: D7, all of it, under `intervention-guard`.
- Acceptance (desktop unit tests, headless):
  - `cancel_wakes_a_waiting_operation_without_polling`. A wait of 30 s returns within 50 ms of `cancel()`, and the error is the `check()` error.
  - `dropping_the_caller_ends_the_wait`. Dropping `CancelOnDrop` wakes the wait.
  - `the_wait_ends_a_margin_before_the_run_deadline`.
  - `the_wait_ends_a_margin_before_the_operation_deadline`.
- Gate: `cap-desktop-guard`.

**B3. Release on abort.**
- Scope: D8 for the four routines, with their `post` closures injectable for tests.
- Acceptance (desktop unit tests, macOS, recorded posts):
  - `guard_refusal_mid_drag_posts_button_up_at_the_last_point`.
  - `cancel_mid_drag_with_guard_on_posts_button_up`.
  - `guard_refusal_mid_chord_releases_held_keys_in_reverse`.
  - `release_posts_skip_the_guard_and_the_checkpoint`.
  - `failed_release_keeps_the_original_error`.
  - `guard_off_keeps_todays_post_sequence`. With the guard off, the recorded posts equal today's sequence.
- Gate: `cap-desktop-guard` and `sv_computer_intervention_guard`.

**B4. The guard.**
- Scope: `intervention.rs` (state machine, `QUIET`, `admit`, `check_press`, routes); the macOS `UserInputMonitor`; the call sites in D9; D12.
- Acceptance:
  - The seven upstream state-machine tests, ported with a simulated listener and run headless: `the_users_input_needs_a_read_after_they_pause_in_that_app_only`, `input_waits_for_the_user_to_pause_then_fails_until_a_read`, `the_runs_deadline_ends_the_wait_for_a_user_still_typing`, `input_into_a_process_drawing_the_apps_panel_counts_as_input_into_the_app`, `a_new_process_reusing_a_watched_id_starts_unwatched`, `a_new_process_reusing_a_panels_id_takes_none_of_the_apps_input`, `the_wait_ends_before_the_native_request_times_out` (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:351-575`). They assert `ErrorCode::UserIntervened`, not message text.
  - `convar_off_never_blocks_input`.
  - `guard_on_without_a_tap_fails_input_with_permission_denied`.
  - `admission_wait_is_ended_by_cancel`. This uses B2's wait under a simulated user who keeps typing.
- Gate: `cap-desktop-guard` and `sv_computer_intervention_guard`.

**B5. Documentation.**
- Scope: the computer documentation names the Input Monitoring permission, the convar, the feature and the meaning of `user_intervened`. The `computer@3` description does not change (O9).
- Acceptance: documentation only; no test.

## Upstream origin and license

- The port draws from these upstream files:
  - `upstream:crates/pi-natives/src/desktop/control/intervention.rs`
  - `upstream:crates/pi-natives/src/desktop/control/macos.rs`
  - the cancellation and cleanup parts of `upstream:crates/pi-natives/src/desktop/control.rs`
  - `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs`
  - `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs`
- Upstream's `LICENSE` is MIT, with copyright lines for Mario Zechner, Can Bölük and Stencil Labs, Inc. (`upstream:LICENSE:1-5`). The new `crates/desktop/NOTICE` keeps that notice.
- The keymap fixtures are generated by omp2 from xkeyboard-config data (D3), and the same NOTICE carries xkeyboard-config's notices.
- Nothing keeps a `pi` identity. The tag value, the module names and the feature names are omp's own.

## Consequences

- Non-US Wayland users get the characters they typed once `sv_computer_xkb_layout` is on. Before, they got the wrong ones and no error.
- macOS builds with `cap-desktop-guard` and the convar on gain a refusal path: an input call can fail with `user_intervened`, and the agent must read again. The fixed fault text says so.
- Two enums gain a variant. Consumers that match exhaustively need a new arm; `native_fault` is one of them, and the compiler enforces it.
- Changing `crates/tools/src/computer.rs` moves `computer@3`'s `projection_code` (`crates/tools/src/computer.rs:522-527`), as any edit to that file does. The revision stays 3 unless the owner says otherwise (O2).
- The tap needs the Input Monitoring permission. Without it, a guard the user turned on refuses input rather than pretending to guard (D12).
- A guard wait blocks the session's actor thread for at most the run deadline minus 250 ms.

## What the review changed

### Round 1 (Goldfish, five reviewers' findings as recorded in STATUS.md row 0063)

1. **Release on abort mid-drag.** Confirmed against source.
   - omp2's `background_drag` leaves through `operation_checkpoint()?` after button-down and posts no button-up (`crates/desktop/src/macos/input.rs:497-529`, `crates/desktop/src/macos/input.rs:513`). `global_pointer`'s drag arm does the same (`crates/desktop/src/macos/input.rs:902-905`).
   - `key_chord` releases keys only after every press succeeds (`crates/desktop/src/macos/input.rs:684-698`).
   - Upstream releases in every case (`upstream:crates/pi-natives/src/desktop/control.rs:344-355`, `upstream:crates/pi-natives/src/desktop/macos/input.rs:2700-2735`, `upstream:crates/pi-natives/src/desktop/macos/input.rs:3621-3657`).
   - Changed: added D8 and slice B3. Making the release unconditional when the guard is off is O8, because it changes today's behaviour on cancel.
2. **Fixed `UserIntervened` fault text; `native_fault` drops messages.** Confirmed against source.
   - `native_fault` discards `DesktopError::message` and uses one static string per code (`crates/envd/src/computer.rs:1724-1755`).
   - Upstream puts the app name and idle time into its messages with `format!` (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:169-179`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:238-242`). That also goes against ADR 0035's rule against formatted library errors (`docs/adr/0035-rust-for-the-engine.md:55-57`).
   - Changed: D5 fixes the text, gives the constructor no arguments, and states the one sentence. B1's acceptance test proves the name never reaches the model. Carrying the name is now a rejected option.
3. **The wait must be cancellable on the actor thread.** Confirmed against source.
   - `OperationState` is an `AtomicBool` and a deadline, with nothing to wake on (`crates/desktop/src/lib.rs:62-75`).
   - The run timeout is known only to envd (`crates/envd/src/computer.rs:183-197`).
   - Upstream waits on a condvar that cancel notifies (`upstream:crates/pi-natives/src/desktop/control.rs:205-216`).
   - Changed: added D7 (condvar wait, `cancel()` used by `CancelOnDrop` and `call`, `set_run_deadline`, the 250 ms margin) and slice B2. This also resolves the first draft's O6 (wait cost), so O6 is closed.
4. **Feature and convar plumbing.** Confirmed against source.
   - The first draft named `cap-desktop-guard` and `sv_desktop_intervention_guard` but no crate declared or forwarded either. `omp-desktop` has only `wayland-pipewire` (`crates/desktop/Cargo.toml:11-13`). Capability features live in envd and app (`crates/envd/Cargo.toml:17-22`, `crates/app/Cargo.toml:24-43`). Computer convars are `sv_computer_*` in envd (`crates/envd/src/computer.rs:32-68`).
   - Changed: D4 names every link (`omp-desktop/intervention-guard`, `omp-envd/cap-desktop-guard`, `omp-app/cap-desktop-guard`, default-off) and the convar path (`ComputerSettings::from_con` to `DesktopSessionOptions`).
   - The convars are renamed `sv_computer_intervention_guard` and `sv_computer_xkb_layout`, to sit with the existing `sv_computer_*` group. Neither name ever shipped.
   - B1 now proves `just check-minimal-caps` and a `cap-desktop-guard` build.
5. **`.xkb` fixture licence.** Confirmed against source.
   - The upstream fixtures have no notice (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb:1`). The only "copyright" in them is a keysym at line 1472.
   - xkeyboard-config's terms were read: X11/MIT-style permission notices with no copyleft clause, requiring the notice in copies (Debian `xkb-data` 2.41 `copyright`).
   - Changed: added D3 (generate fixtures with a recorded `xkbcli` command, add `crates/desktop/NOTICE`, copy upstream's only as a fallback). The first draft's O5 is closed.

### Corrections found while verifying round 1 (not reviewer findings)

- Upstream's synthetic tag spells `pidesk` (`upstream:crates/pi-natives/src/desktop/control/macos.rs:20`). D6 uses an omp value.
- The A1 test list lacked `serialized_modifier_fields_affect_resolution` (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:647`). The B2 list (now B4) lacked `a_new_process_reusing_a_panels_id_takes_none_of_the_apps_input` (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:514`), and it named `listener_off_never_blocks_input` as if it came from upstream. That test is omp's own, now `convar_off_never_blocks_input`.
- The first draft's B1 asked for a runtime test that every `ErrorCode` maps to a `FaultCode`. `native_fault`'s `match` is already exhaustive at compile time (`crates/envd/src/computer.rs:1725`). Replaced with a test of the fixed text.
- The first draft's B3 changed the `computer@3` prompt snapshot, and D6 reported the guard in the capability report. Both change an existing tool's description or output (`crates/tools/src/computer.rs:494-502`, `crates/tools/src/computer.rs:338-363`). They are moved to O9 and O7. D12 now fails closed instead.

### Rounds 2 and 3

Pending. Fresh-context Goldfish reviewers will review this revision, and their findings will be recorded here. Implementation waits until a round returns no confirmed blocker or major finding.

## Open questions for the owner

- **O1.** Does a pure bug fix (Part A) need a default-off gate under the additive-only rule? Recommendation: yes, `sv_computer_xkb_layout` for one release, then on by default. It changes what is typed, from wrong to right.
- **O2.** Accept a `UserIntervened` variant in `ErrorCode` and `FaultCode`? Does a new output enum value need a `computer@4` revision? Recommendation: accept, and keep `computer@3`, because the input schema (`Params`) does not change. Without the variant Part B cannot ship.
- **O3.** Should omp2 grow a takeover mode? Recommendation: not in this record. Decide it separately, then revisit the X11 guard and the Escape stops.
- **O4.** Should the 5 s quiet period be a convar? Recommendation: no. Keep it a constant matching upstream until someone needs to change it.
- **O5.** Closed by D3 (fixtures are generated and carry a NOTICE).
- **O6.** Closed by D7 (the wait is bounded by `min(run deadline, operation deadline) − 250 ms` and ended by cancel).
- **O7.** May `computer@3`'s `Capabilities` gain an optional `intervention_guard` field (`active`, `no_permission` or `off`)? That is an output shape change to an existing tool. Recommendation: yes, as an additive optional field. Until then D12 fails closed.
- **O8.** Should release on abort (D8) apply to every session, not only to guarded ones? Today a cancel in the middle of a drag leaves the button held (`crates/desktop/src/macos/input.rs:513`). Fixing that changes existing behaviour. Recommendation: yes, ungated, in its own PR after B3. The Wayland and X11 drag paths should be checked for the same defect.
- **O9.** Should the `computer@3` description mention `user_intervened`? Recommendation: no. The fixed fault text already tells the agent to read again, and changing the description changes an existing tool and its prompt prefix.

## Status in omp

**Status: Not started.** EGM round 1 is resolved and rounds 2-3 are pending. Implementation is not authorized until a round passes.

- Reusable today: the `reis`, `x11rb` and `xkeysym` dependencies, the actor thread, `OperationState`, the typed error enums, and the `sv_computer_*` convar path.
- Implementation, once authorized, sits behind the default-off feature `cap-desktop-guard` (`omp-envd`, `omp-app`) and the default-off convar `sv_computer_intervention_guard` (Part B). Part A sits behind the default-off convar `sv_computer_xkb_layout`.

## References

- [docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md)
- ADR 0035 (error discipline)
- ADR 0049 (`cap-*` features)
- ADR 0012 (convars)
- ADR 0006 (host policy and the sandbox stub, for the trust boundary around desktop control)
- `crates/tui/NOTICE` (the bundled-data notice pattern)
