use herdr_file_viewer::fuzzy::match_and_rank;

// (a) AC-3: case-insensitive subsequence matching
#[test]
fn subsequence_match_is_case_insensitive() {
    let candidates = vec!["src/App.rs".to_string(), "README.md".to_string()];
    // "app" is a case-insensitive subsequence of "src/App.rs"
    let result = match_and_rank("app", &candidates);
    assert!(result.contains(&0), "app should match src/App.rs (index 0)");
    assert!(!result.contains(&1), "app should not match README.md");
}

#[test]
fn non_subsequence_does_not_match() {
    let candidates = vec!["src/main.rs".to_string()];
    // "xyz" is not a subsequence of "src/main.rs"
    let result = match_and_rank("xyz", &candidates);
    assert!(result.is_empty(), "xyz should not match src/main.rs");
}

// (b) AC-4: basename weighting — basename hit ranks before directory-only hit
#[test]
fn basename_hit_ranks_before_directory_only_hit() {
    // "src/app.rs": "app" matches the basename "app.rs" directly
    // "apple/x.rs": "app" matches via directory characters only
    let candidates = vec![
        "src/app.rs".to_string(), // index 0 — basename hit
        "apple/x.rs".to_string(), // index 1 — directory-only hit
    ];
    let result = match_and_rank("app", &candidates);
    assert_eq!(result.len(), 2, "both paths should match");
    assert_eq!(result[0], 0, "src/app.rs (basename hit) must rank first");
    assert_eq!(result[1], 1, "apple/x.rs (dir hit) must rank second");
}

#[test]
fn basename_hit_ranks_first_regardless_of_candidate_order() {
    // Same paths, reversed in the slice — ranking must still prefer the basename hit
    let candidates = vec![
        "apple/x.rs".to_string(), // index 0 — directory-only hit
        "src/app.rs".to_string(), // index 1 — basename hit
    ];
    let result = match_and_rank("app", &candidates);
    assert_eq!(result.len(), 2, "both paths should match");
    assert_eq!(result[0], 1, "src/app.rs (basename hit) must rank first");
    assert_eq!(result[1], 0, "apple/x.rs (dir hit) must rank second");
}

// (c) AC-2 backing: empty query → empty Vec
#[test]
fn empty_query_returns_empty_vec() {
    let candidates = vec!["src/main.rs".to_string(), "README.md".to_string()];
    let result = match_and_rank("", &candidates);
    assert!(result.is_empty(), "empty query must return empty Vec");
}

// (d) AC-6: query that matches nothing → empty Vec
#[test]
fn query_matching_nothing_returns_empty_vec() {
    let candidates = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];
    let result = match_and_rank("zzzzz", &candidates);
    assert!(result.is_empty(), "no-match query must return empty Vec");
}

// (e) AC-N3: a string in neither path is not matched
#[test]
fn string_in_neither_path_does_not_match() {
    let candidates = vec!["src/tree.rs".to_string(), "src/render.rs".to_string()];
    // "xyz" appears in neither path
    let result = match_and_rank("xyz", &candidates);
    assert!(
        result.is_empty(),
        "xyz is in neither path and must not match"
    );
}

// Additional: subsequence ordering — "apprs" matches "src/app.rs" (chars in order, not adjacent)
#[test]
fn non_adjacent_subsequence_matches() {
    let candidates = vec!["src/app.rs".to_string()];
    let result = match_and_rank("apprs", &candidates);
    assert_eq!(result, vec![0], "apprs is a subsequence of src/app.rs");
}

// Additional: deterministic ordering — same input always produces same output
#[test]
fn result_is_deterministic() {
    let candidates = vec!["src/app.rs".to_string(), "apple/x.rs".to_string()];
    let a = match_and_rank("app", &candidates);
    let b = match_and_rank("app", &candidates);
    assert_eq!(a, b, "result must be deterministic");
}
