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

/// The prompt has two names and is one parameter, which is how a theme that
/// sets `PROMPT` changes the prompt the shell prints.
#[test]
fn the_prompt_parameters_share_one_value() {
    assert_eq!(run("PROMPT='x> '; echo \"[$PS1]\""), "[x> ]\n");
    assert_eq!(
        run("PS1='y> '; echo \"[$PROMPT][$prompt]\""),
        "[y> ][y> ]\n"
    );
    assert_eq!(run("PROMPT2='c> '; echo \"[$PS2]\""), "[c> ]\n");
    // The right-hand prompts are two parameters in zsh, not one.
    assert_eq!(run("RPROMPT=r; echo \"[$RPS1]\""), "[]\n");
}

/// The colour and attribute escapes, as the codes zsh's terminal
/// capabilities produce for a 256-colour terminal. Agnoster is built out of
/// these and nothing else.
#[test]
fn prompt_colours_are_the_sequences_zsh_writes() {
    let p = |spec: &str| run(&format!("print -Pn -- '{spec}'"));
    assert_eq!(p("%F{blue}"), "\x1b[34m");
    assert_eq!(p("%F{black}"), "\x1b[30m");
    assert_eq!(p("%K{blue}"), "\x1b[44m");
    // 0-7 are the plain codes, 8-15 the bright ones, the rest are indexed.
    assert_eq!(p("%F{7}"), "\x1b[37m");
    assert_eq!(p("%F{9}"), "\x1b[91m");
    assert_eq!(p("%F{200}"), "\x1b[38;5;200m");
    assert_eq!(p("%K{200}"), "\x1b[48;5;200m");
    // A name the terminal would not know is the default colour, not an error.
    assert_eq!(p("%F{default}"), "\x1b[39m");
    assert_eq!(p("%F{bogus}"), "\x1b[39m");
    assert_eq!(p("%f%k"), "\x1b[39m\x1b[49m");
    // There is no "not bold", so `%b` resets everything, as zsh's does.
    assert_eq!(p("%B%b"), "\x1b[1m\x1b[0m");
    assert_eq!(p("%U%u%S%s"), "\x1b[4m\x1b[24m\x1b[7m\x1b[27m");
    // `%{...%}` are markers; what they enclose is literal.
    assert_eq!(p("%{raw%}"), "raw");
    assert_eq!(p("%%"), "%");
}

/// `%(c.true.false)`, whose branches are themselves prompts.
#[test]
fn a_prompt_ternary_picks_a_branch() {
    let p = |spec: &str| run(&format!("print -Pn -- '{spec}'"));
    // The tests run unprivileged, so `!` is false and `?` starts true.
    assert_eq!(p("%(!.root.user)"), "user");
    assert_eq!(p("%(?.ok.bad)"), "ok");
    assert_eq!(p("%(100j.busy.idle)"), "idle");
    // The chosen branch is expanded in its turn.
    assert_eq!(p("%(?.%F{green}ok%f.no)"), "\x1b[32mok\x1b[39m");
    // Text after the ternary is not swallowed by it.
    assert_eq!(p("%(!.a.b)c"), "bc");
}

/// `$commands` is every program on `$PATH` by name, which is how a script
/// asks whether one is installed. oh-my-zsh asks constantly.
#[test]
fn commands_says_which_programs_exist() {
    assert_eq!(run("echo $+commands[sh]"), "1\n");
    assert_eq!(run("echo $+commands[definitely-not-here-xyzzy]"), "0\n");
    assert_eq!(run("[[ -x $commands[sh] ]] && echo yes"), "yes\n");
}

/// A `${v/pat/repl}` whose pattern holds an escaped delimiter. agnoster
/// turns a ref into a branch name with `${ref/refs\/heads\//…}`.
#[test]
fn a_replacement_pattern_may_escape_its_delimiter() {
    assert_eq!(
        run("r=refs/heads/master; echo \"${r/refs\\/heads\\//B }\""),
        "B master\n"
    );
    assert_eq!(run("r=a/b/c; echo \"${r/\\//-}\""), "a-b/c\n");
    assert_eq!(run("r=abc; echo \"${r/b/X}\""), "aXc\n");
}
