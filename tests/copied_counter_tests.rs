/// Tests for the copied_counter module.
///
/// All tests here are pure unit tests -- no network, no async runtime required.
/// The count_intersection() function is extracted specifically to be testable.
use polycopier::copied_counter::count_intersection;
use std::collections::HashSet;

fn hset(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn vec_ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

// -- Basic intersection -------------------------------------------------------

#[test]
fn empty_our_tokens_returns_zero() {
    let our = hset(&[]);
    let target = vec_ids(&["tok_a", "tok_b"]);
    assert_eq!(count_intersection(&our, &target), 0);
}

#[test]
fn empty_target_tokens_returns_zero() {
    let our = hset(&["tok_a", "tok_b"]);
    let target = vec_ids(&[]);
    assert_eq!(count_intersection(&our, &target), 0);
}

#[test]
fn both_empty_returns_zero() {
    assert_eq!(count_intersection(&hset(&[]), &vec_ids(&[])), 0);
}

#[test]
fn full_overlap_returns_all() {
    let ids = ["tok_a", "tok_b", "tok_c"];
    assert_eq!(count_intersection(&hset(&ids), &vec_ids(&ids)), 3);
}

#[test]
fn partial_overlap_returns_matching_count() {
    let our = hset(&["tok_a", "tok_b", "tok_c"]);
    let target = vec_ids(&["tok_b", "tok_c", "tok_d", "tok_e"]);
    // tok_b and tok_c overlap -> 2
    assert_eq!(count_intersection(&our, &target), 2);
}

#[test]
fn no_overlap_returns_zero() {
    let our = hset(&["tok_a", "tok_b"]);
    let target = vec_ids(&["tok_c", "tok_d"]);
    assert_eq!(count_intersection(&our, &target), 0);
}

#[test]
fn single_match_returns_one() {
    let our = hset(&["tok_x"]);
    let target = vec_ids(&["tok_x"]);
    assert_eq!(count_intersection(&our, &target), 1);
}

// -- Direction: target tokens drive the iteration ----------------------------

#[test]
fn our_tokens_superset_of_target() {
    // We hold 5, target holds 2 that we also hold + 1 we don't
    let our = hset(&["tok_a", "tok_b", "tok_c", "tok_d", "tok_e"]);
    let target = vec_ids(&["tok_a", "tok_c", "tok_z"]);
    // tok_a and tok_c match -> 2
    assert_eq!(count_intersection(&our, &target), 2);
}

#[test]
fn target_superset_of_our_tokens() {
    // Target holds 10, we only hold 3, all of which match
    let our = hset(&["tok_a", "tok_b", "tok_c"]);
    let target = vec_ids(&[
        "tok_a", "tok_b", "tok_c", "tok_d", "tok_e", "tok_f", "tok_g", "tok_h", "tok_i", "tok_j",
    ]);
    assert_eq!(count_intersection(&our, &target), 3);
}

// -- Deduplication (HashSet guarantees uniqueness on our side) ---------------

#[test]
fn duplicate_target_ids_are_each_counted() {
    // If somehow the API returns the same token twice in target (shouldn't happen
    // but we want predictable behaviour)
    let our = hset(&["tok_a"]);
    let target = vec_ids(&["tok_a", "tok_a"]);
    // Each occurrence in target is checked individually -- both match -> 2
    // This is intentional: if the API ever returns duplicates we count them.
    assert_eq!(count_intersection(&our, &target), 2);
}

// -- Realistic token ID format -----------------------------------------------

#[test]
fn realistic_long_token_ids() {
    let our = hset(&[
        "21742633143470315552392769629011789491048130483316390788235188509469430617252",
        "52114319501245915516055106046884209969926127482827954674443846427813813222426",
    ]);
    let target = vec_ids(&[
        "21742633143470315552392769629011789491048130483316390788235188509469430617252",
        "99999999999999999999999999999999999999999999999999999999999999999999999999999",
    ]);
    // First ID matches, second doesn't -> 1
    assert_eq!(count_intersection(&our, &target), 1);
}

// -- Multiple targets scenario (simulated by calling twice) ------------------

#[test]
fn multi_target_sum_is_additive() {
    // Simulate two target wallets being summed in the caller
    let our = hset(&["tok_a", "tok_b", "tok_c"]);

    let target1 = vec_ids(&["tok_a", "tok_x"]); // 1 match
    let target2 = vec_ids(&["tok_b", "tok_c", "tok_y"]); // 2 matches

    let total = count_intersection(&our, &target1) + count_intersection(&our, &target2);
    assert_eq!(total, 3);
}
