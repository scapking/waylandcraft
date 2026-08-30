// This file is intentionally empty.
//
// Step 2 originally added smithay im2 / ti3 manager (InputMethodManagerState +
// TextInputManagerState + delegate_input_method_manager! /
// delegate_text_input_manager! + InputMethodHandler impl).
//
// However, smithay's `delegate_input_method_manager!` macro writes
//   impl Dispatch<ZwpInputMethodManagerV2, ()> for WLCState
//   impl GlobalDispatch<ZwpInputMethodManagerV2, ()> for WLCState
// which **directly conflicts** (E0119) with the existing impls in
// `native/src/ime/input_method_v2.rs`:
//   impl GlobalDispatch<ZwpInputMethodManagerV2, ()> for WLCState
//   impl Dispatch<ZwpInputMethodManagerV2, ()> for WLCState
// (and similarly for ZwpTextInputManagerV3).
//
// wayland_server uses (Type, UserData) as the trait identity — same Type
// + same UserData = identical impl = E0119. This is a hard Rust trait
// coherence rule that no wrapper can hide.
//
// Therefore Step 2 is **infeasible under the task's constraints**:
// - The task says "用 delegate_input_method_manager! + delegate_text_input_manager!"
//   (which forces the conflicting impls).
// - The task also says "保留现有的 input_method_v2.rs / text_input_v3.rs
//   （不能删，删了必崩）" (which mandates the existing impls stay).
//
// These two requirements are **physically incompatible** with smithay's
// design. Step 2 has been rolled back; the module is left as a tombstone
// so future readers can see we tried.
//
// See `docs/agent/implementation/STEP_2.md` for the full report and the
// options to take back to the user.
//
// Until the user picks a path (refactor ime/ or drop smithay im2), the
// waylandcraft codebase stays at Step 1 — smithay SeatState field exists
// (compiles cleanly, 48/48 tests pass) but the manager state is not used.