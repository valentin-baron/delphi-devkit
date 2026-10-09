use std::path::PathBuf;
use ddk_core::utils::normalize_path;

#[test]
fn strips_extended_length_prefix() {
    let input = r"\\?\C:\Users\foo\project";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo\project"));
}

#[test]
fn leaves_normal_absolute_path_unchanged() {
    let input = r"C:\Users\foo\project";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo\project"));
}

#[test]
fn leaves_relative_path_unchanged() {
    let input = r"src\main.rs";
    assert_eq!(normalize_path(input), PathBuf::from(r"src\main.rs"));
}

#[test]
fn leaves_unc_path_without_prefix_unchanged() {
    let input = r"\\server\share\file.txt";
    assert_eq!(normalize_path(input), PathBuf::from(r"\\server\share\file.txt"));
}

/// Delphi project files sometimes store UNC paths without the leading `\\`.
#[test]
fn converts_bare_unc_prefix_to_unc_path() {
    let input = r"UNC\Mac\repos\be\BE\D12\be.dpr";
    assert_eq!(normalize_path(input), PathBuf::from(r"\\Mac\repos\be\BE\D12\be.dpr"));
}

#[test]
fn converts_bare_unc_prefix_case_insensitive() {
    let input = r"unc\server\share\path\file.exe";
    assert_eq!(normalize_path(input), PathBuf::from(r"\\server\share\path\file.exe"));
}

/// `\\?\UNC\server\share\...` is the extended-length UNC form: both prefixes
/// have to go.
#[test]
fn strips_extended_length_unc_prefix() {
    let input = r"\\?\UNC\Mac\repos\be\BE\D12\be.dproj";
    assert_eq!(normalize_path(input), PathBuf::from(r"\\Mac\repos\be\BE\D12\be.dproj"));
}

#[test]
fn resolves_parent_dir_after_unc_conversion() {
    let input = r"UNC\Mac\repos\be\BE\D12\..\other\file.exe";
    assert_eq!(normalize_path(input), PathBuf::from(r"\\Mac\repos\be\BE\other\file.exe"));
}

#[test]
fn resolves_parent_dir_in_absolute_path() {
    let input = r"C:\Users\foo\..\bar";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\bar"));
}

#[test]
fn resolves_multiple_parent_dirs() {
    let input = r"C:\a\b\c\..\..\d";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\a\d"));
}

#[test]
fn resolves_parent_dir_at_end() {
    let input = r"C:\Users\foo\bar\..";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo"));
}

#[test]
fn resolves_parent_dir_in_relative_path() {
    let input = r"a\b\..\c";
    assert_eq!(normalize_path(input), PathBuf::from(r"a\c"));
}

#[test]
fn parent_dir_past_root_stays_at_root() {
    let input = r"C:\..";
    let result = normalize_path(input);
    assert_eq!(result, PathBuf::from(r"C:\"));
}

#[test]
fn parent_dir_in_relative_path_is_preserved_when_nothing_to_pop() {
    let input = r"..\foo\bar";
    assert_eq!(normalize_path(input), PathBuf::from(r"..\foo\bar"));
}

#[test]
fn consecutive_parent_dirs_in_relative_path() {
    let input = r"..\..\foo";
    assert_eq!(normalize_path(input), PathBuf::from(r"..\..\foo"));
}

#[test]
fn removes_current_dir_segments() {
    let input = r"C:\Users\.\foo\.\bar";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo\bar"));
}

#[test]
fn removes_current_dir_at_start_of_relative_path() {
    let input = r".\src\main.rs";
    assert_eq!(normalize_path(input), PathBuf::from(r"src\main.rs"));
}

#[test]
fn strips_prefix_and_resolves_parent_dir() {
    let input = r"\\?\C:\Users\foo\..\bar\project";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\bar\project"));
}

#[test]
fn strips_prefix_and_resolves_current_dir() {
    let input = r"\\?\C:\Users\.\foo";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo"));
}

#[test]
fn strips_prefix_and_resolves_complex_path() {
    let input = r"\\?\C:\a\b\.\c\..\d\..\..\e";
    // a, b, c, .., d, .., .., e → a, b, [c popped], d, [d popped], [b popped], e
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\a\e"));
}

#[test]
fn empty_path_returns_dot() {
    assert_eq!(normalize_path(""), PathBuf::from("."));
}

#[test]
fn single_dot_returns_relative_empty() {
    assert_eq!(normalize_path("."), PathBuf::from("."));
}

#[test]
fn root_only_stays_root() {
    let input = r"C:\";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\"));
}

#[test]
fn no_op_on_already_clean_path() {
    let input = r"C:\Users\foo\project\src\main.rs";
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\Users\foo\project\src\main.rs"));
}

#[test]
fn accepts_pathbuf_input() {
    let input = PathBuf::from(r"C:\Users\foo\..\bar");
    assert_eq!(normalize_path(&input), PathBuf::from(r"C:\Users\bar"));
}

#[test]
fn accepts_path_reference() {
    let input = std::path::Path::new(r"C:\a\b\..\c");
    assert_eq!(normalize_path(input), PathBuf::from(r"C:\a\c"));
}
