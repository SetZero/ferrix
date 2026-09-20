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

/// `<(cmd)` and `>(cmd)`: the word is the name of a pipe the command runs
/// on the other end of. oh-my-zsh's own lib reaches `source <(dircolors -b)`
/// before any theme is loaded.
#[test]
fn process_substitution_names_a_pipe() {
    assert_eq!(run("cat <(echo hi)"), "hi\n");
    assert_eq!(run("source <(echo x=5); echo $x"), "5\n");
    assert_eq!(run("cat <(echo a) <(echo b)"), "a\nb\n");
    // The parentheses inside belong to the command, not to the construct.
    assert_eq!(run("cat <(echo '(x)')"), "(x)\n");
    assert_eq!(run("cat <((echo nested))"), "nested\n");
    // As a redirection target, either way round.
    assert_eq!(run("wc -l < <(printf 'a\\nb\\n')"), "2\n");
    // The name is a word, so it can stand in the middle of one.
    assert!(run("echo A<(echo b)C").starts_with("A/proc/self/fd/"));
    // `<` also begins a numeric glob, which is a pattern and not a command.
    assert_eq!(run("[[ 5 = <-> ]] && echo num"), "num\n");
    assert_eq!(run("[[ 12 = <1-20> ]] && echo in"), "in\n");
    assert_eq!(run("case 7 in <->) echo digits;; esac"), "digits\n");
}

/// A shell that keeps its end of the pipe open must also let go of it, or a
/// prompt that substitutes on every draw runs the shell out of descriptors.
#[test]
fn process_substitution_closes_its_descriptors() {
    assert_eq!(
        run("for i in $(seq 1 300); do cat <(echo x) >/dev/null; done; echo done"),
        "done\n"
    );
}

/// A multibyte character survives being quoted. The shell escapes some
/// bytes internally, and escaping them twice is what turned agnoster's and
/// avit's glyphs into nonsense.
#[test]
fn a_multibyte_character_survives_quoting() {
    // U+25B6, whose middle byte is one the shell uses for a token of its own.
    assert_eq!(run("echo '\u{25b6}'"), "\u{25b6}\n");
    assert_eq!(run("echo \"\u{25b6}\""), "\u{25b6}\n");
    assert_eq!(run("echo \u{25b6}"), "\u{25b6}\n");
    assert_eq!(run("v='\u{25b6}'; echo $v"), "\u{25b6}\n");
    assert_eq!(run("echo $'\\u25b6'"), "\u{25b6}\n");
    assert_eq!(run("echo $'\\xe2\\x96\\xb6'"), "\u{25b6}\n");
    // Quoting still keeps a glob character literal.
    assert_eq!(run("echo '*'"), "*\n");
}

/// `%n` and `$USERNAME` are the password database's answer, not `$USER`:
/// agnoster asks `$USERNAME` whether the user is worth naming at all.
#[test]
fn the_user_is_named_by_the_password_database() {
    let who = run("print -Pn -- '%n'");
    assert!(!who.is_empty(), "%n gave nothing");
    assert_eq!(run("echo $USERNAME"), format!("{who}\n"));
    // A bogus $USER does not change either of them.
    assert_eq!(run("USER=nobody; print -Pn -- '%n'"), who);
    // LOGNAME comes from the same place, so a theme comparing the two --
    // avit does -- sees no difference that is only a missing value.
    assert_eq!(run("echo $LOGNAME"), format!("{who}\n"));
    assert_eq!(run("[[ $LOGNAME == $USERNAME ]] && echo same"), "same\n");
}

/// An escape that means nothing expands to nothing, rather than to itself.
/// The cloud theme's prompt holds a bare `%` with a space after it.
#[test]
fn an_unknown_prompt_escape_expands_to_nothing() {
    assert_eq!(run("print -Pn -- 'a% b'"), "ab");
    assert_eq!(run("print -Pn -- 'a%zb'"), "ab");
    assert_eq!(run("print -Pn -- 'a%'"), "a");
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

/// `zstyle` keeps a real database: the most specific pattern that matches the
/// context answers. A `zstyle` that answered everything made `vcs_info`
/// believe its debug styles were set and print its whole trace into the
/// middle of agnoster's prompt.
#[test]
fn a_style_is_answered_by_the_most_specific_pattern() {
    let db = "zstyle ':a:*' col A1 A2; zstyle '*:b' col B1; ";
    assert_eq!(
        run(&format!("{db}zstyle -s ':a:b' col v; echo $v")),
        "A1 A2\n"
    );
    assert_eq!(
        run(&format!("{db}zstyle -s ':a:b' col v -; echo $v")),
        "A1-A2\n"
    );
    assert_eq!(
        run(&format!("{db}zstyle -a ':a:b' col a; echo ${{#a}}")),
        "2\n"
    );
    // Weight, not the order they were written in: `*` is worth 2, `:` 3 and
    // anything else 4, so the pattern that spells more out comes first.
    let both = "zstyle ':*:b' k P1; zstyle ':a*' k P2; ";
    assert_eq!(run(&format!("{both}zstyle -s ':a:b' k v; echo $v")), "P1\n");
    // `-t` is false for an undefined style and `-T` is true, which is the
    // difference vcs_info leans on.
    assert_eq!(
        run("zstyle ':x:*' f yes; zstyle -t ':x:1' f; echo $?"),
        "0\n"
    );
    assert_eq!(
        run("zstyle ':x:*' f no; zstyle -t ':x:1' f; echo $?"),
        "1\n"
    );
    assert_eq!(run("zstyle -t ':none:1' f; echo $?"), "2\n");
    assert_eq!(run("zstyle -T ':none:1' f; echo $?"), "0\n");
    assert_eq!(
        run("zstyle ':x:*' f yes; zstyle -b ':x:1' f v; echo $v"),
        "yes\n"
    );
    assert_eq!(run("zstyle -b ':none:1' f v; echo $v"), "no\n");
    // `-e` is code, run at each lookup, whose `reply` is the value.
    assert_eq!(
        run("zstyle -e ':e:*' k 'reply=(EV)'; zstyle -s ':e:1' k v; echo $v"),
        "EV\n"
    );
    // `-d` forgets, and `-L` prints definitions back as commands.
    assert_eq!(run("zstyle ':p:*' k v1; zstyle -L"), "zstyle ':p:*' k v1\n");
    assert_eq!(run("zstyle ':p:*' k v1; zstyle -d; zstyle -L"), "");
}

/// `$functions` names every function the shell knows, defined or only marked
/// by `autoload`. vcs_info asks `(( $+functions[VCS_INFO_detect_git] ))`
/// before it will use a backend.
#[test]
fn functions_says_which_functions_exist() {
    assert_eq!(run("f() { :; }; echo ${+functions[f]}"), "1\n");
    assert_eq!(run("echo ${+functions[nope]}"), "0\n");
    assert_eq!(run("f() { :; }; v=f; echo ${+functions[$v]}"), "1\n");
    // A name marked but never found on `fpath` answers as zsh's does.
    assert_eq!(
        run("fpath=(); autoload -Uz nowhere; echo $functions[nowhere]"),
        "builtin autoload -XU\n"
    );
}

/// EXTENDED_GLOB's `x~y`: what `x` matches, less what `y` does. vcs_info
/// finds its backends with `${^fpath}/VCS_INFO_get_data_*~*(\~|.zwc)(N)`,
/// which without the operator either finds everything or nothing.
#[test]
fn the_except_operator_takes_matches_away() {
    let e = "setopt extendedglob; ";
    assert_eq!(run(&format!("{e}[[ abc == *~*x ]] && echo yes")), "yes\n");
    assert_eq!(run(&format!("{e}[[ abc == *~abc ]] || echo no")), "no\n");
    assert_eq!(
        run(&format!("{e}[[ abc == *~(x|y) ]] && echo yes")),
        "yes\n"
    );
    // `|` binds looser than `~`, so this is `(a*~ab)` or `ac`. The bar is
    // written inside a group because a bare one ends the condition, in zsh
    // as here.
    assert_eq!(
        run(&format!("{e}[[ ac == (a*~ab|ac) ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ ad == (a*~ab|ac) ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ ab == (a*~ab|ac) ]] || echo no")),
        "no\n"
    );
    // The backup and compiled files vcs_info's own glob leaves out.
    let p = "*~*(\\~|.zwc)";
    assert_eq!(
        run(&format!("{e}[[ get_data_git == {p} ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ get_data_git.zwc == {p} ]] || echo no")),
        "no\n"
    );
    // A `~` with nothing after it is the character, and a word with no
    // wildcard beside it is not a glob at all.
    assert_eq!(run(&format!("{e}[[ f~ == *~ ]] && echo yes")), "yes\n");
    assert_eq!(run(&format!("{e}echo a~b")), "a~b\n");
}

/// `(#b)` remembers where each group matched, in `$match` and the two arrays
/// of offsets. vcs_info names its backends with it -- `${file:#(#b)…_(*)}` --
/// and its git backend reads a ref apart the same way.
#[test]
fn a_pattern_can_remember_its_groups() {
    let e = "setopt extendedglob; ";
    assert_eq!(
        run(&format!(
            "{e}f=VCS_INFO_get_data_git; : ${{f:#(#b)VCS_INFO_get_data_(*)}}; echo $match $mbegin $mend"
        )),
        "git 19 21\n"
    );
    assert_eq!(
        run(&format!("{e}[[ abcdef == (#b)a(b*)(e)f ]] && echo $match")),
        "bcd e\n"
    );
    assert_eq!(
        run(&format!(
            "{e}[[ abcdef == (#b)a(b*)(e)f ]] && echo $mbegin $mend"
        )),
        "2 5 4 5\n"
    );
    // A group takes as much as it can, which is the path zsh reports.
    assert_eq!(
        run(&format!("{e}[[ aXbXc == (#b)(*)X* ]] && echo $match")),
        "aXb\n"
    );
    // Without the flag nothing is written down.
    assert_eq!(run(&format!("{e}[[ ab == (a)(b) ]] && echo ok")), "ok\n");
}

/// A brace range expands its endpoints first: zsh runs parameter expansion
/// before brace expansion, the opposite of the other shells. `vcs_info`
/// walks its messages with `{1..${#msgs}}`, and compaudit writes the same
/// thing as `{1..$#_i_addfiles}`.
#[test]
fn a_brace_range_counts_a_parameter_in_its_endpoints() {
    assert_eq!(
        run("a=(x y z); echo {1..${#a}}"),
        "1 2 3
"
    );
    assert_eq!(
        run("a=(x y z); echo {1..$#a}"),
        "1 2 3
"
    );
    assert_eq!(
        run("n=4; echo {1..$n}"),
        "1 2 3 4
"
    );
    assert_eq!(
        run("s=abcd; echo {1..$#s}"),
        "1 2 3 4
"
    );
    // A step is an endpoint too, and a literal range is what it always was.
    assert_eq!(
        run("n=3; echo {1..10..$n}"),
        "1 4 7 10
"
    );
    assert_eq!(
        run("echo {a..c}"),
        "a b c
"
    );
    // Nothing to count is nothing to expand: the range stays as written.
    assert_eq!(
        run("a=(); echo {1..${#a}}"),
        "1 0
"
    );
}
