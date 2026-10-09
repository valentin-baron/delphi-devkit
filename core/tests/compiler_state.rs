use ddk_core::projects::compiler_state;

// These tests share module-level statics and must run serially:
// `cargo test --test compiler_state -- --test-threads=1`.

#[test]
fn activate_returns_true_first_time() {
    compiler_state::reset();
    assert!(compiler_state::activate());
}

#[test]
fn activate_returns_false_on_reentry() {
    compiler_state::reset();
    assert!(compiler_state::activate());
    assert!(!compiler_state::activate());
}

#[test]
fn reset_allows_reactivation() {
    compiler_state::reset();
    compiler_state::activate();
    compiler_state::reset();
    assert!(compiler_state::activate());
}

#[test]
fn is_active_after_activate() {
    compiler_state::reset();
    assert!(!compiler_state::is_active());
    compiler_state::activate();
    assert!(compiler_state::is_active());
}

#[test]
fn not_cancelled_by_default() {
    compiler_state::reset();
    assert!(!compiler_state::is_cancelled());
}

#[test]
fn cancel_sets_flag() {
    compiler_state::reset();
    compiler_state::cancel();
    assert!(compiler_state::is_cancelled());
}

#[test]
fn reset_clears_cancelled() {
    compiler_state::reset();
    compiler_state::cancel();
    compiler_state::reset();
    assert!(!compiler_state::is_cancelled());
}

#[test]
fn success_default_false() {
    compiler_state::reset();
    assert!(!compiler_state::is_success());
}

#[test]
fn set_success() {
    compiler_state::reset();
    compiler_state::set_success(true);
    assert!(compiler_state::is_success());
}

#[test]
fn code_default_minus_one() {
    compiler_state::reset();
    assert_eq!(compiler_state::get_code(), -1);
}

#[test]
fn set_code() {
    compiler_state::reset();
    compiler_state::set_code(42);
    assert_eq!(compiler_state::get_code(), 42);
}

#[test]
fn track_and_take_diagnosed_files() {
    let _ = compiler_state::take_diagnosed_files();

    compiler_state::track_diagnosed_file("file1.pas".to_string());
    compiler_state::track_diagnosed_file("file2.pas".to_string());
    compiler_state::track_diagnosed_file("file1.pas".to_string()); // duplicate

    let files = compiler_state::take_diagnosed_files();
    assert_eq!(files.len(), 2);
    assert!(files.contains("file1.pas"));
    assert!(files.contains("file2.pas"));
}

#[test]
fn take_diagnosed_files_returns_empty_after_drain() {
    let _ = compiler_state::take_diagnosed_files();
    compiler_state::track_diagnosed_file("a.pas".to_string());
    let _ = compiler_state::take_diagnosed_files();
    let files = compiler_state::take_diagnosed_files();
    assert!(files.is_empty());
}
