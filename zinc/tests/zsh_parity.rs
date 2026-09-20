//! What oh-my-zsh needs of the shell, checked against what zsh 5.9 does.
//!
//! Every case here came from running oh-my-zsh's installer or its start-up
//! under zinc and finding the place it stopped. The expected values are what
//! `zsh -c` prints on the machine this was written on; they are written out
//! rather than measured at test time, so the test says the same thing on a
//! machine with no zsh installed.

use std::process::Command;

/// What `zinc -c script` printed on standard output.
fn run(script: &str) -> String {
    match Command::new(env!("CARGO_BIN_EXE_zinc"))
        .args(["-c", script])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => format!("zinc did not start: {error}"),
    }
}

/// `${=spec}` splits at `$IFS` whether or not it is quoted, and an IFS
/// character that is not whitespace keeps the empty fields around it.
#[test]
fn a_forced_split_follows_the_ifs_rules() {
    assert_eq!(run("v='a b c'; print -l ${=v}"), "a\nb\nc\n");
    assert_eq!(run("v='a b c'; print -l \"${=v}\""), "a\nb\nc\n");
    assert_eq!(run("IFS=.; v='a..b'; print -l ${=v}"), "a\n\nb\n");
    assert_eq!(run("IFS=.; v='.a.'; a=(${=v}); echo ${#a}"), "3\n");
    assert_eq!(run("IFS=' .'; v='a . b'; a=(${=v}); echo ${#a}"), "2\n");
    assert_eq!(run("v=''; a=(${=v}); echo ${#a}"), "0\n");
    assert_eq!(run("IFS=; v='a b'; a=(${=v}); echo ${#a}"), "1\n");
    // The same rule for what a command substitution splits.
    assert_eq!(run("IFS=.; a=($(echo 'a..b')); echo ${#a}"), "3\n");
    assert_eq!(run("a=($(echo '  x  y ')); echo ${#a}"), "2\n");
}

/// Arithmetic reaches into arrays and hashes. `is-at-least`, which
/// oh-my-zsh asks about every version it cares about, is written in it.
#[test]
fn arithmetic_subscripts_a_parameter() {
    assert_eq!(run("a=(5 7); echo $(( a[1] + a[2] ))"), "12\n");
    assert_eq!(run("a=(5 7 9); i=1; echo $(( a[i+1] ))"), "7\n");
    assert_eq!(run("a=(5 7); echo $(( a[-1] )) $(( a[5] ))"), "7 0\n");
    // A hash takes its subscript literally, so `h[k]` is the key `k`.
    assert_eq!(run("typeset -A h; h[x]=4; k=x; echo $(( h[k] ))"), "0\n");
    assert_eq!(run("typeset -A h; h[x]=4; k=x; echo $(( h[$k] ))"), "4\n");
    // A scalar's subscript is one character of it.
    assert_eq!(run("s=59; echo $(( s[1] )) $(( s[2] ))"), "5 9\n");
    // And they are lvalues.
    assert_eq!(run("a=(5); (( a[1] += 2 )); echo $a[1]"), "7\n");
    assert_eq!(run("a=(5); (( ++a[1] )); echo $a[1]"), "6\n");
    assert_eq!(
        run("typeset -A h; h[k]=1; (( h[k] = 8 )); echo $h[k]"),
        "8\n"
    );
}

/// A hash reads the subscript flags differently from an array: zsh's
/// `colors` builds `$fg` out of `${color[(I)fg-*]}`, every matching key.
#[test]
fn subscript_flags_on_a_hash_answer_with_keys_and_values() {
    const HASH: &str = "typeset -A c; c=(fg-red 31 fg-blue 34 bg-red 41); ";
    assert_eq!(
        run(&format!("{HASH}echo ${{c[(I)fg-*]}}")),
        "fg-red fg-blue\n"
    );
    assert_eq!(run(&format!("{HASH}echo ${{c[(i)fg-*]}}")), "fg-red\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(r)3*]}}")), "31\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(R)3*]}}")), "31 34\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(k)fg-red]}}")), "31\n");
    // An array still answers with indices, the last one for `I`.
    assert_eq!(run("a=(x y z y); echo ${a[(I)y]} ${a[(i)y]}"), "4 2\n");
    assert_eq!(run("a=(x y); echo ${a[(I)q]}"), "0\n");
}
