# 0063. Desktop input: layout-aware XKB typing on Wayland and release of held input on abort; the macOS intervention guard is deferred

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-14 (first draft 2026-11-13)
Area: tools

Research: [docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md) (Gap 4 of the upstream gap closure). This record states decisions; the note holds the evidence.

Review state: this record is under Elephant-Goldfish (EGM) review. In each round, fresh-context reviewers ("Goldfish") read only this record and the files it cites, and the author verifies every finding against source. Round 1 returned five findings. Round 2 returned two blockers, seventeen majors and sixteen minors. Both rounds are resolved (see "What the review changed"). No slice below may be implemented before round 3 passes.

Round 2 cut the scope. This record now decides two small fixes:

- **Part A**: layout-aware typing on Wayland.
- **Part C**: release of held input when an input call stops early.

**Part B**, the macOS user-intervention guard, is deferred to its own future record. The section "Part B: deferred" lists what that record must settle.

Terms used here:

- "run": one `action=run` program of the `computer@3` tool.
- "actor thread": the single native thread that owns a desktop backend (`crates/desktop/src/lib.rs:730-748`, `crates/desktop/src/linux/actor.rs:91`).
- "libei": the Wayland emulated-input protocol that omp2 drives through the `reis` crate.

Citation keys: omp2 paths are relative to the repository root at `omp2` 9cb6fd638. Upstream paths carry the `upstream:` prefix and refer to can1357/oh-my-pi at c308970263.

## Context

### The problems in omp2

**Part A: wrong characters on non-US Wayland layouts.**
- Wayland typing goes through a fixed US table. `type_text` maps each character through `evdev_char` (`crates/desktop/src/linux/wayland/libei.rs:453-484`, `crates/desktop/src/linux/wayland/libei.rs:572`).
- `key_chord` maps `KeyName::Char` through the same table (`crates/desktop/src/linux/wayland/libei.rs:429-451`, `crates/desktop/src/linux/wayland/libei.rs:517-570`).
- The send loop knows only Shift, keycode 42 (`crates/desktop/src/linux/wayland/libei.rs:470-481`).
- On a French layout, `a` presses the US-`A` key, which types `q`, and no error is reported. [INFERENCE from the evdev codes]
- `ActorLibei::new` drops the libei event stream after the one discovery pass (`let (_connection, mut events)`, `crates/desktop/src/linux/wayland/libei.rs:84-91`).
- `EiDevice` holds only a device and a serial (`crates/desktop/src/linux/wayland/libei.rs:45-48`), and nothing in `crates/desktop/src` handles `KeyboardModifiers` or a keymap. So omp2 cannot know the active group even after discovery.

**Part C: held buttons and keys after an early exit.**
- On macOS:
  - `background_drag` posts button-down, then can leave through `crate::operation_checkpoint()?` without button-up (`crates/desktop/src/macos/input.rs:497-529`, checkpoint at `:513`).
  - `global_pointer`'s drag arm has the same shape (`crates/desktop/src/macos/input.rs:902-905`).
  - `key_chord` releases keys only after every press succeeded (`crates/desktop/src/macos/input.rs:684-698`).
- On Wayland, the libei drag presses the modifiers and the button, then can leave through the checkpoint at `crates/desktop/src/linux/wayland/libei.rs:401` with both still held (`crates/desktop/src/linux/wayland/libei.rs:379-410`).
- A cancelled run or an expired deadline therefore leaves a button or key held on the user's desktop.

### What OMPv1 does

**Typing on Wayland.**
- libei hands over the compositor's compiled keymap. `KeyboardLayout` parses keycodes, types and per-group levels, and never borrows a key from another group (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:1-7`).
- The backend keeps the event stream (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:389-414`) and drains it before each input in `refresh_devices`, at 1 ms per poll and at most 256 events, failing with "did not settle; no input was sent" (`:459`, `:711`, `:751`).
- `handle_event` reads a keyboard device's keymap at `DeviceAdded` and tracks `DeviceResumed`, `DevicePaused`, `DeviceRemoved` and `SeatRemoved`. It feeds `KeyboardModifiers` into `KeyboardLayout::update_modifiers` (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:316-388`).
- `char_stroke` sends control characters (`\n`, `\t`) through fixed evdev keys in every group, because the keymap would resolve `'\n'` to `<LNFD>`. It sends ASCII through fixed keys only when `can_use_us_ascii_fast_path` holds; that is decided by the `group_is_us` row check, not by group names. Everything else resolves through the active group.
- With no usable keymap, printable text is an error and is never assumed to be US (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:871-896`, `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:521-540`, test `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:1117-1121`).
- `read_keymap` rejects non-XKB keymaps, size 0 and sizes over 16 MiB (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:860-869`).
- Chords route `KeyName::Char` through the layout and press every modifier in the stroke, including AltGr (100) (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:815-833`).
- Tests: eight in `xkb.rs`, including a helper that rewrites `symbols[GroupN]` to the numeric form newer libxkbcommon writes (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:560-676`). Two more in `libei.rs` (`:1117-1128`).

**Releasing held input.**
- `control::cleanup` runs release posts with cancellation checks off (`upstream:crates/pi-natives/src/desktop/control.rs:344-355`).
- `hold_transitions` releases every held key, in reverse order, when a post, the body or a check fails (`upstream:crates/pi-natives/src/desktop/macos/input.rs:3621-3657`).
- `stroke_path` posts the drag's button-up at the last point it reached, whatever failed. Only when a drag-and-drop session has begun does it first move back to the start point (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2700-2735`).

### Rules that bind

- **Additive only, and implementation behind a default-off gate** (`/home/user/oh-my-pi/docs/research/upstream-gap-closure-brief.md:43-46`, `:95-97`, `:150-152`). Both parts change what an existing tool does, from wrong to right. Each ships behind its own default-off convar. Turning one on by default is the owner's call (O1, O8).
- **ADR 0035.** Typed errors. A failure is never silent, so a layout that cannot be read must not fall back to a guessed US layout.
- **Naming.** No `pi` identity in anything ported. The computer convars are `sv_computer_*`, declared in `crates/envd/src/computer.rs:32-68` with `ui.tab`, `ui.group` and `ui.label` meta, and compiled only with `cap-computer` (`crates/envd/src/lib.rs:28-29`).

## Decision

**D1. Two independent fixes, each with its own gate.**
- Part A (Wayland layout) and Part C (release on abort) share no code, ship separately, and can be authorised separately.
- Part B (the macOS intervention guard) is not decided here (D9).

**D2. Part A: the libei backend owns its event stream and a layout per keyboard.**
- `ActorLibei` keeps the handshake's `EiConvertEventStream` and the `reis::event::Connection` instead of dropping them (`crates/desktop/src/linux/wayland/libei.rs:84-91`).
- `EiDevice` gains `layout: Option<KeyboardLayout>` and `resumed: bool`.
- One `handle_event` serves both discovery and refresh:
  - `DeviceAdded`: read the keymap (`read_keymap`).
  - `DeviceResumed` and `DevicePaused`: set `resumed`.
  - `DeviceRemoved` and `SeatRemoved`: drop the device.
  - `KeyboardModifiers`: call `update_modifiers` on that device's layout.
  - `Disconnected`: return `InputFailed`.
- Before each `type_text` and `key_chord`, `refresh_devices` drains the stream on the Linux actor thread through `runtime.block_on` (`crates/desktop/src/linux/actor.rs:91`, `:145`, `:152`). It polls 1 ms at a time, at most 256 events. If the stream has not settled, the call fails with `InputFailed` before sending anything.
- `type_text` and `key_chord` take the actor's runtime handle so they can do this. Today they take only `&mut self`.
- A user who switches layout mid-session gets the new group on the next call.

**D3. Part A: how a character becomes keys.**
- `char_stroke(layout, char)` follows upstream (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:871-896`):
  - Control characters go through fixed evdev keys in every group.
  - ASCII goes through the fixed US table only when `can_use_us_ascii_fast_path` holds (upstream's `group_is_us` row check).
  - Every other character resolves through the active group alone.
- A character the active group cannot produce fails with `ErrorCode::InputFailed`.
- With the convar on and no usable keymap (non-XKB, size 0, over 16 MiB, parse failure, or no keyboard device), printable characters fail with `InputFailed`. They are never typed through the US table, so the gate never brings back the silent wrong-character bug.
- `key_chord` resolves `KeyName::Char` through `char_stroke` too. Every stroke carries a modifier list, which can include AltGr (100). It is pressed in order and released in reverse.
- With the convar off, `type_text` and `key_chord` behave exactly as they do today.

**D4. Part A: gate.**
- Convar `sv_computer_xkb_layout`: bool, default `false`, flags `archive`. Meta: `ui.tab = "tools"`, `ui.group = "Computer"`, `ui.label = "Computer Wayland Keyboard Layout"`.
- It is declared beside the other `sv_computer_*` convars and read in `ComputerSettings::from_con` (`crates/envd/src/computer.rs:76-84`).
- It reaches the backend as a new field `DesktopSessionOptions::xkb_layout: bool`. The only struct literal is updated in the same change (`crates/desktop/src/types.rs:137-141`, `crates/envd/src/computer.rs:103`).
- The worker hands the value to the Linux backend when it creates `ActorLibei`.
- No Cargo feature. The code sits inside `omp-desktop`, which is already behind `cap-computer` (`crates/envd/Cargo.toml:18`).

**D5. Part A: fixtures and licence.**
- omp2 generates its own keymaps with libxkbcommon's `xkbcli compile-keymap --rules evdev --model pc105 --layout fr` and `--layout us,fr` (US first, French second). That is the serialisation a compositor hands to libei.
- They are stored at `crates/desktop/src/linux/wayland/testdata/fr.xkb` and `us-fr.xkb`, the paths the tests' `include_str!` expects.
- `crates/desktop/NOTICE`, in the shape of `crates/tui/NOTICE`, records three things:
  - the exact command and the libxkbcommon and xkeyboard-config versions;
  - xkeyboard-config's notices: the `COPYING` at tag `xkeyboard-config-2.41`, 190 lines, twelve copyright lines, all X11/MIT-style permission notices, no copyleft clause;
  - upstream's MIT notice for the ported parser (`upstream:LICENSE:1-5`).
- The provenance lives in the NOTICE, not in the fixture files, because the ported parser is not specified to skip comments.
- Both group-label forms are tested. The fixtures carry whichever form the generator writes. A test helper rewrites them into the other form, as upstream's `numeric_group_labels` does in one direction (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:577-582`).
- If a ported assertion disagrees with the generated keymap, the fix is to the assertion (citing the fixture's line) or to the parser. Upstream's fixtures are never copied.

**D6. Part C: release on abort.**
- Each input routine that presses something owns its release. On any exit after a press attempt (a cancel, an expired deadline or a failed post), it posts the release for what it still holds. Those release posts skip `operation_checkpoint`, as upstream's `control::cleanup` does.
- What each routine tracks:
  - macOS drags (`background_drag`, `crates/desktop/src/macos/input.rs:478-551`; `global_pointer` drag arm, `crates/desktop/src/macos/input.rs:883-913`): the button and the last point posted. On abort they post button-up at that last point.
  - macOS clicks (`background_click`, `crates/desktop/src/macos/input.rs:362-415`; `global_pointer` click arm, `crates/desktop/src/macos/input.rs:840-872`): whether a down was attempted without its up. A failed down may still have been delivered, so it gets its up.
  - macOS `key_chord` (`crates/desktop/src/macos/input.rs:675-700`): the keys pressed so far and the running modifier flags. On abort it releases them in reverse, with the flags each release would carry.
  - libei drag (`crates/desktop/src/linux/wayland/libei.rs:379-410`): the button and the pressed modifiers. On abort it releases the button, then the modifiers.
- If a release fails, the routine returns the original error. The release failure goes to `tracing::warn!` without key codes.
- A drag released mid-path drops whatever it carries at the last point. Upstream does the same for strokes that have not begun a drag-and-drop session (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2728-2734`). omp2 has no drag-session detection, so the move back to the start point is not ported (O10).
- Seam: each routine takes its post and checkpoint as closures, like `stroke_path(path, picked_up, post)` upstream. Tests inject a failing checkpoint and record the posts. No guard is involved.

**D7. Part C: gate.**
- Convar `sv_computer_release_on_abort`: bool, default `false`, flags `archive`. Meta: `ui.tab = "tools"`, `ui.group = "Computer"`, `ui.label = "Computer Release Held Input On Abort"`.
- It is declared and plumbed like D4, as `DesktopSessionOptions::release_on_abort: bool`.
- With the convar off, the routines behave exactly as they do today.
- Win32 posted-message drags (`crates/desktop/src/win32/input.rs:324-341`) and X11 drags were not checked for the same defect. They are out of this record (O8).

**D8. Proceeding while owner questions are open.** Every slice lands behind its default-off convar on the recommendation stated here. Only turning a default on waits for the owner's answer (O1, O8). No slice depends on O2-O7 or O9.

**D9. Part B, the macOS intervention guard, is deferred.**
- It is not ported in this record.
- A future record must design the following against omp2's code:
  - the per-site admission and `check_press` map;
  - the pid source for `Target::Desktop` and for accessibility handles;
  - the tap's attribution, lifecycle and re-enable;
  - the `UserIntervened` wire code;
  - a lost-wakeup-free cancellable wait;
  - how its feature-gated tests run in CI.
- That record must pass its own review. The requirements and the evidence gathered so far are in "Part B: deferred" below.
- Reason for deferring:
  - The design is large and cannot be proven in CI: no runner grants Input Monitoring.
  - It depends on owner rulings (O2, O3).
  - Without it, the agent's read-before-act loop leaves a race, not a hole.
  - Parts A and C fix observable defects now.

## Rejected options

- **Extend `evdev_char` with more layouts.** Cheap for one layout and unbounded for the rest, and still wrong for any layout it leaves out.
- **Link `libxkbcommon`.** A C dependency in a Rust-only workspace. `reis` 0.5.0 uses `xkbcommon` only as a dev-dependency, so the ported parser adds nothing. Using `xkbcli` once to generate fixtures adds no build dependency.
- **Fall back to the US table when the keymap cannot be read.** This brings back the silent wrong-character bug (ADR 0035). Upstream refuses too (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:1117-1121`).
- **No gate, using the layout whenever it parses.** The brief requires default-off gates for implementation and forbids changing existing behaviour without the owner. O1 asks the owner to turn it on.
- **Read the keymap once at discovery.** That freezes the group (round 2, B1).
- **Copy upstream's `.xkb` fixtures.** They carry no notice and no record of how they were made (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb:1`).
- **Trim the fixtures to the keycodes the tests use.** The parser must be tested on what a compositor actually sends: a full compiled keymap.
- **Drop the US ASCII fast path.** It is optional (round 2, m12). Keeping it ports upstream's behaviour and tests unchanged.
- **Return to the start point before releasing an aborted drag.** Upstream does this only once a drag-and-drop session has begun, and omp2 cannot detect that (O10).
- **Port the intervention guard now.** Deferred (D9).

## Slices

Each slice is one PR. Tests are written first and are not edited to pass.

Proof levels:
- No slice has an e2e proof in `crates/e2e`. The harness has no desktop (`crates/e2e/tests` has no computer test), and no CI runner grants a Wayland RemoteDesktop portal.
- No golden journal applies, because no journal shape changes.
- Proof is at the owning seams:
  - `omp-desktop` unit tests, headless;
  - an in-process EIS peer for the libei seam. `reis` 0.5.0 exposes the server side as `reis::eis`, with `ei::Context::new(UnixStream)` and `eis::Context::new(UnixStream)` (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/reis-0.5.0/src/lib.rs:8-12`, `src/eis.rs:98-99`);
  - recorded-post unit tests for Part C. The macOS ones run in the macOS workspace job and need no permission to create `CGEvent`s.
- Manual check before turning either default on (O1, O8), recorded in the PR that flips it:
  - Part A: on a GNOME Wayland session with `us,fr`, `type_text` types `é@aq` correctly in each group, and switching group mid-session changes the result.
  - Part C: on macOS, cancelling a run mid-drag leaves no button held.

**A1. Layout-aware typing on Wayland, behind `sv_computer_xkb_layout`.**
- Scope:
  - port `xkb.rs`;
  - generate the fixtures and add `crates/desktop/NOTICE` (D5);
  - stream ownership and `handle_event`/`refresh_devices` (D2);
  - `char_stroke` and chords (D3);
  - the convar and the options field (D4).
- Documentation: `crates/desktop/README.md` names the convar and the layout rule.
- Acceptance:
  - The eight ported `xkb.rs` tests pass on the generated fixtures:
    - `reads_the_keymap_from_the_start_of_a_shared_fd`, under `cfg(target_os = "linux")` because it uses `memfd_create`;
    - `resolves_numeric_group_labels` and its inverse-rewrite twin, `resolves_named_group_labels`;
    - `resolves_french_levels_and_altgr`;
    - `resolves_only_the_active_group_in_a_multi_layout_keymap`;
    - `modifier_event_group_change_updates_resolution`;
    - `serialized_modifier_fields_affect_resolution`;
    - `num_lock_does_not_change_non_keypad_levels`;
    - `caps_lock_uses_the_alphabetic_type_map`.
  - The two ported `libei.rs` tests pass: `printable_text_without_a_keymap_never_assumes_us_layout` and `newline_presses_enter_even_when_the_keymap_has_a_linefeed_key`.
  - New unit tests:
    - `tab_and_return_use_fixed_keys_on_a_french_group`;
    - `character_absent_from_the_active_group_fails_with_input_failed`;
    - `char_chord_on_azerty_presses_the_layouts_key`;
    - `altgr_stroke_presses_and_releases_its_modifiers_in_reverse`.
  - EIS-peer seam tests:
    - `keyboard_modifiers_event_switches_the_group_used_by_the_next_type_text`;
    - `removed_keyboard_fails_typing_with_input_failed`;
    - `unsettled_event_stream_fails_before_sending_any_key`;
    - `convar_off_keeps_the_us_table`.
  - The existing `discovery_waits_for_every_granted_device` still passes.
- Gate: `sv_computer_xkb_layout`, default `false`.

**C1. Release held input on abort, behind `sv_computer_release_on_abort`.**
- Scope: D6 for the macOS drags, clicks and `key_chord` and for the libei drag; closure seams; the convar and the options field (D7).
- Acceptance (recorded posts, injected failing checkpoint):
  - `cancel_mid_drag_posts_button_up_at_the_last_point` (macOS background and global);
  - `cancel_mid_chord_releases_held_keys_in_reverse_with_running_flags` (macOS);
  - `failed_down_post_still_gets_its_up` (macOS click);
  - `cancel_mid_libei_drag_releases_button_then_modifiers` (EIS peer);
  - `release_posts_skip_the_checkpoint`;
  - `failed_release_keeps_the_original_error`;
  - `completed_drag_sequence_is_unchanged`. With the convar on or off, a drag that is not aborted posts exactly today's sequence.
- Gate: `sv_computer_release_on_abort`, default `false`.

## Part B: deferred

The round-1 resolutions for the guard stand as requirements for its future record:

- **Fixed fault text.** `native_fault` drops `DesktopError::message` (`crates/envd/src/computer.rs:1724-1755`). The code must be a typed `UserIntervened` with fixed text, never upstream's formatted app name (`upstream:crates/pi-natives/src/desktop/control/intervention.rs:169-179`).
- **A cancellable wait on the actor thread.** Today `OperationState` is only a flag and a deadline (`crates/desktop/src/lib.rs:62-75`).
- **Named, default-off plumbing per ADR 0049**, or a reasoned choice of a convar alone (round 2, M11).
- **An omp-owned synthetic event tag.** Upstream's `0x7069_6465_736b` spells `pidesk` (`upstream:crates/pi-natives/src/desktop/control/macos.rs:20`).

Round 2's findings on the guard, all confirmed against source, are what that record must answer:

- **B2.** Map every omp2 entry point and per-press site (`crates/desktop/src/macos/mod.rs:84-125`, `crates/desktop/src/macos/ax.rs:170-197`, `crates/desktop/src/macos/input.rs:56-132`). Decide the pid for `Target::Desktop`, which has none (`crates/desktop/src/types.rs:256-261`). Admit before focus is moved, as upstream's request-level `watch_app` does (`upstream:crates/pi-natives/src/desktop/mod.rs:1274`).
- **M3.** `cancel` must store and notify under the wait's mutex, as upstream's `cancel` does under the state lock (`upstream:crates/pi-natives/src/desktop/control.rs:88-98`).
- **M4.** State one wait bound, its function and where the run deadline is stored and cleared.
- **M5.** Specify tap attribution and filtering by tag and source pid (`upstream:crates/pi-natives/src/desktop/control/macos.rs:413-440`). Choose one tagging choke point across all event-creation sites (`crates/desktop/src/macos/input.rs:572`, `:616`, `:656`, `:710`, `:927`, `:956`).
- **M6, M7.** Specify the tap's lifecycle, its re-enable on the disabled pseudo-events (`upstream:crates/pi-natives/src/desktop/control/macos.rs:375`, `:494`, `:596`) and a permission preflight. Port the classification tests (`upstream:crates/pi-natives/src/desktop/control/macos.rs:786-914`). The upstream file is 929 lines.
- **M8.** Port panel routes with their call sites (`upstream:crates/pi-natives/src/desktop/macos/input.rs:98`, `:573`, `:862`), or drop them and their tests.
- **M9.** A recorded acceptance procedure on a real Mac.
- **M10, M11.** A CI recipe and job that build and test the gated code, or no feature at all.
- **M13.** No guard check between drag points without porting drag-session detection.
- **Minors.** m5 (the constructor and test premise), m7 (a stale convar on a build without the feature), m9 (deadline clearing, close during a wait, the shape of the options struct), m11 (Decision altitude), m13 (thread-local watch), m14 (documentation path), m15 (older readers of a new `FaultCode`).

## Upstream origin and license

- The port draws from:
  - `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs`
  - `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs` (`handle_event`, `refresh_devices`, `read_keymap`, `char_stroke`, `plan_keys`)
  - the release pattern in `upstream:crates/pi-natives/src/desktop/control.rs` and `upstream:crates/pi-natives/src/desktop/macos/input.rs`
- Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). The new `crates/desktop/NOTICE` keeps it.
- The keymap fixtures are generated by omp2 from xkeyboard-config 2.41 data, and their notices go in the same NOTICE (D5).
- Nothing keeps a `pi` identity.

## Consequences

- With `sv_computer_xkb_layout` on, non-US Wayland users get the characters they typed and the chords they named, including after a layout switch. A character the active group lacks fails loudly instead of typing a wrong glyph.
- With `sv_computer_release_on_abort` on, a cancelled or expired run no longer leaves a mouse button or key held on macOS or Wayland. A drag aborted mid-path drops at its last point.
- Accepted limitation: `native_fault` maps both failures to the existing generic text "desktop input delivery failed" (`crates/envd/src/computer.rs:1730`). Adding a typed code for a character the layout lacks is O11.
- With either convar off, nothing changes.
- The intervention guard is still missing on omp2. The read-before-act loop is the only protection against a user typing in the same app.

## What the review changed

### Round 1 (Goldfish, five findings as recorded in STATUS.md row 0063)

1. **Release on abort mid-drag.** Confirmed against source.
   - `background_drag` leaves through `operation_checkpoint()?` after button-down (`crates/desktop/src/macos/input.rs:497-529`, `:513`), and so does the `global_pointer` drag arm (`:902-905`).
   - `key_chord` releases only after every press succeeds (`:684-698`).
   - Upstream always releases (`upstream:crates/pi-natives/src/desktop/control.rs:344-355`, `upstream:crates/pi-natives/src/desktop/macos/input.rs:2700-2735`, `:3621-3657`).
   - Changed: added release on abort. Round 2 made it the standalone Part C.
2. **Fixed `UserIntervened` text; `native_fault` drops messages.** Confirmed: `crates/envd/src/computer.rs:1724-1755`, against upstream's formatted text at `upstream:crates/pi-natives/src/desktop/control/intervention.rs:169-179`. Changed: fixed text and an argument-free constructor. Round 2 carried this into "Part B: deferred" as a requirement.
3. **The wait must be cancellable on the actor thread.** Confirmed: `crates/desktop/src/lib.rs:62-75`, against `upstream:crates/pi-natives/src/desktop/control.rs:205-216`. Changed: a condvar wait and a run deadline. Round 2 carried this into "Part B: deferred", with M3 and M4 added.
4. **Feature and convar plumbing.** Confirmed: the first draft declared neither (`crates/desktop/Cargo.toml:11-13`, `crates/envd/Cargo.toml:17-22`, `crates/app/Cargo.toml:24-43`). Changed: every link named, and the convars renamed `sv_computer_*` (`crates/envd/src/computer.rs:32-68`). Round 2 dropped the feature for Parts A and C (see M11) and kept the convars.
5. **`.xkb` fixture licence.** Confirmed: the upstream fixtures carry no notice (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb:1`, "copyright" only as a keysym at `:1472`). Changed: generated fixtures plus a NOTICE. Round 2 replaced the evidence with xkeyboard-config's own `COPYING` (see m10).

Corrections found while verifying round 1:
- Upstream's tag spells `pidesk`.
- The test lists were incomplete: `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:647`, `upstream:crates/pi-natives/src/desktop/control/intervention.rs:514`.
- A runtime test duplicated the compile-time exhaustive `match` (`crates/envd/src/computer.rs:1725`).
- A change to the `computer@3` description and its `Capabilities` output became owner questions (`crates/tools/src/computer.rs:494-502`, `:338-363`). Round 2 deferred both with Part B.

### Round 2 (Goldfish, four fresh reviewers and a synthesis: 2 blockers, 17 majors, 16 minors)

Each finding was verified against source before it was accepted or rejected.

- **B1 (blocker): the Wayland layout cannot track the active group.**
  - Confirmed. The event stream is dropped at `crates/desktop/src/linux/wayland/libei.rs:84-91`. `EiDevice` has no layout (`:45-48`). A grep finds no `KeyboardModifiers` in `crates/desktop/src`. Upstream drains its stream before each input (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:316-414`).
  - Changed: D2 (stream ownership, `handle_event`, `refresh_devices` bounded at 256 events and 1 ms polls on the actor thread, device lifecycle) and EIS-peer seam tests in A1.
  - **Resolved.**
- **B2 (blocker): no omp2 call-site, admission or pid map for the guard.**
  - Confirmed. The draft's D9 cited only upstream lines. omp2's entry points are at `crates/desktop/src/macos/mod.rs:84-125`, and `Target::Desktop` carries no pid (`crates/desktop/src/types.rs:256-261`).
  - Changed: a scope cut. Part B is deferred to its own record (D9), with this finding as a requirement.
  - **Resolved for this record**, by not deciding the guard here.
- **M1: control characters and the no-keymap case.**
  - Confirmed: `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:871-896`, `:1117-1128`, and `upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:521-540`.
  - Changed: D3 ports upstream's `char_stroke` rule and refuses printable text without a keymap (no US fallback). The rejected-options list says why.
- **M2: chord scope and Shift-only sending.**
  - Confirmed: `crates/desktop/src/linux/wayland/libei.rs:429-451`, `:470-481`, `:567-569`, and `upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:815-833`.
  - Changed: D3 routes chords through the layout and presses a modifier list released in reverse. A1 adds two tests.
- **M3 (lost wakeup), M4 (wait bound), M5 (tap attribution and tagging), M6 (tap lifecycle), M7 (size of the tap port), M8 (routes), M9 (real-Mac acceptance), M10 (CI for gated code).**
  - Confirmed. The citations are in "Part B: deferred", for example `upstream:crates/pi-natives/src/desktop/control.rs:88-98`, `upstream:crates/pi-natives/src/desktop/control/macos.rs:375`, `:494`, `:596` (re-enable) and `crates/desktop/src/macos/input.rs:616`, `:710`, `:927`, `:956` (unnamed event sites).
  - Changed: all move to Part B's future record.
- **M11: the three-crate feature chain is ceremony.**
  - Confirmed for Parts A and C. Their code sits in `omp-desktop`, already behind `cap-computer` (`crates/envd/Cargo.toml:18`, `crates/envd/src/lib.rs:28-29`). They add no dependency.
  - Changed: no Cargo feature. A default-off convar each (D4, D7). For Part B, left to its record.
- **M12: the release fix is gated behind the guard, and a test pins the bug.**
  - Confirmed: `crates/desktop/src/macos/input.rs:513`, `:684-698`, `:902-905`.
  - Changed: Part C is a standalone slice with its own convar (D7). The guard-off test is replaced by `completed_drag_sequence_is_unchanged`, which covers only the sequence that is not aborted.
  - Partly rejected: the fix is not ungated. The brief forbids changing existing behaviour without the owner and requires default-off gates (`/home/user/oh-my-pi/docs/research/upstream-gap-closure-brief.md:43-46`, `:95-97`). O8 asks to turn it on.
- **M13: releasing at the last point drops at an arbitrary place.**
  - Confirmed that upstream returns to the start only once a drag session has begun (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2728-2734`).
  - Changed: the per-step guard check that made mid-path aborts frequent went with Part B. D6 keeps upstream's rule for strokes with no drag session (release at the last point) and states the drop. Drag-session detection is O10.
- **M14: B3 depends on B4 and has no seam.**
  - Confirmed: `background_drag` and `background_click` call `post_mouse` directly (`crates/desktop/src/macos/input.rs:362-415`, `:478-551`).
  - Changed: C1 needs no guard. D6 gives every routine post and checkpoint closures, and its tests use an injected failing checkpoint.
- **M15: drop the Part A convar.**
  - Rejected. The brief requires implementation behind a default-off gate and forbids changing existing tool behaviour without the owner (`/home/user/oh-my-pi/docs/research/upstream-gap-closure-brief.md:43-46`, `:95-97`). The silent-fallback concern is handled by D3's no-US-fallback rule. The `meta` part is accepted: D4 now carries `ui.tab`, `ui.group` and `ui.label` like `crates/envd/src/computer.rs:32-68`.
- **M16: fixtures and the recipe.**
  - Confirmed: upstream's tests hard-code keycodes and rewrite labels one way only (`upstream:crates/pi-natives/src/desktop/linux/wayland/xkb.rs:577-624`). The fixtures use `symbols[Group1]` (`upstream:crates/pi-natives/src/desktop/linux/wayland/testdata/fr.xkb`, `xkb_symbols` section).
  - Changed: D5 states the path, the group order and the generator versions recorded in the NOTICE; both label forms are tested; on a mismatch the assertion or parser is fixed and upstream's fixtures are never copied; `memfd_create` is under `cfg(target_os = "linux")`. The test names stay upstream's; "under omp names" was dropped.
- **M17: no rule for proceeding while owner questions are open.**
  - Confirmed: the draft's header and O-list gave none.
  - Changed: D8. O-questions are now referenced only after they are defined, or by number with "see Open questions".
- **m1: dead-code slices.** Confirmed: the draft's A1, B1 and B2 had no caller. Changed: the parser lands with its caller in one slice, A1. The other dead slices went with Part B.
- **m2: two decisions in one record.** Confirmed. Changed: Part B is deferred, and Parts A and C are authorised independently (D1, D8).
- **m3: the omp2 problem is never stated.** Confirmed. Changed: Context opens with the omp2 failures.
- **m4: undefined terms.** Confirmed. Changed: a terms paragraph at the top. Most of the undefined terms belonged to Part B and left with it.
- **m5, m7, m9, m11, m13, m14, m15.** Confirmed where they apply. All concern Part B and are listed under "Part B: deferred".
- **m6: inconsistent review status.**
  - Confirmed for the ADR's wording ("five reviewers' findings"), now "five findings".
  - Rejected for `docs/adr/STATUS.md:75`: the gap orchestrator owns that file, and this record proposes the row text in its report instead.
- **m8: distinct failures collapse into generic text.**
  - Confirmed: `crates/envd/src/computer.rs:1730`.
  - Changed: recorded as an accepted limitation in Consequences. O11 asks for a typed code. The draft's Part B `PermissionDenied` case went with Part B.
- **m10: licence evidence and the fallback.**
  - Confirmed: the draft cited a host file and a copy fallback that contradicted the draft's own rejection of copying.
  - Changed: D5 cites xkeyboard-config's `COPYING` at tag `xkeyboard-config-2.41` (190 lines, twelve copyright lines, no copyleft), and the copy fallback is removed.
  - Trimming the fixtures is rejected (see Rejected options).
- **m12: drop the US fast path.** Rejected as optional. Keeping it ports upstream's rule and tests unchanged (`upstream:crates/pi-natives/src/desktop/linux/wayland/libei.rs:877-884`).
- **m16: release state per routine.** Confirmed: `crates/desktop/src/macos/input.rs:684-698`. Changed: D6 lists what each routine tracks, including that clicks hold only between down and up.

### Round 3

Pending.

## Open questions for the owner

- **O1.** Turn `sv_computer_xkb_layout` on by default? Recommendation: yes, one release after A1, once the manual check in "Proof levels" passes. It changes what is typed, from wrong to right.
- **O2-O7, O9.** These were about Part B: the `UserIntervened` code and a `computer@4` revision, a takeover mode, the quiet-period constant, the guard's capability field, and the tool description. They move to Part B's future record.
- **O8.** Turn `sv_computer_release_on_abort` on by default, and check the Win32 and X11 drag paths for the same defect? Recommendation: yes to both. A button held after a cancel is never intended.
- **O10.** Port drag-and-drop session detection, so an aborted drag that lifted an item returns it to the start before releasing (`upstream:crates/pi-natives/src/desktop/macos/input.rs:2716-2734`)? Recommendation: yes, as a follow-up after C1.
- **O11.** Add a typed code for "the active layout cannot produce this character", so the model sees more than "desktop input delivery failed"? Recommendation: yes, additive, with the same wire caveat as any new `FaultCode`.
- **O12.** Is the guard (Part B) still wanted, given that omp2 has no takeover mode? Recommendation: yes, as its own record, after the owner rules on the `UserIntervened` code.

## Status in omp

**Status: Not started.** EGM round 2 is resolved and round 3 is pending. Implementation waits for round 3.

- Reusable today: `reis` (with `reis::eis` for test peers), `xkeysym`, the Linux actor thread and its runtime, the `sv_computer_*` convar path, and `DesktopSessionOptions`.
- Implementation, once authorised, sits behind two default-off convars: `sv_computer_xkb_layout` (Part A) and `sv_computer_release_on_abort` (Part C). There is no new Cargo feature.

## References

- [docs/research/desktop-intervention-guard.md](../research/desktop-intervention-guard.md)
- ADR 0035 (error discipline)
- ADR 0049 (`cap-*` features)
- ADR 0012 (convars)
- `crates/tui/NOTICE` (the bundled-data notice pattern)
- xkeyboard-config `COPYING` at tag `xkeyboard-config-2.41`
